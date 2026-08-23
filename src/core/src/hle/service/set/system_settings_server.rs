// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/set/system_settings_server.h and system_settings_server.cpp
//!
//! ISystemSettingsServer service ("set:sys").

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::mem::{ManuallyDrop, MaybeUninit};
use std::path::Path;
use std::sync::Mutex;

use common::uuid::UUID;

use super::setting_formats::appln_settings::{default_appln_settings, ApplnSettings};
use super::setting_formats::device_settings::{default_device_settings, DeviceSettings};
use super::setting_formats::private_settings::{default_private_settings, PrivateSettings};
use super::setting_formats::system_settings::{default_system_settings, SystemSettings};
use super::settings_types::*;
use crate::hle::result::{ErrorModule, ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::psc::time::common::{
    LocationName, SteadyClockTimePoint, SystemClockContext,
};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// Settings file version, matching upstream SETTINGS_VERSION.
const SETTINGS_VERSION: u32 = 4;

/// Settings magic bytes, matching upstream SETTINGS_MAGIC.
const SETTINGS_MAGIC: u64 = u64::from_le_bytes([b'y', b'u', b'z', b'u', b'_', b's', b'e', b't']);

/// Settings file header.
///
/// Corresponds to `SettingsHeader` in upstream system_settings_server.cpp.
#[repr(C)]
pub struct SettingsHeader {
    pub magic: u64,
    pub version: u32,
    pub reserved: u32,
}
const _: () = assert!(core::mem::size_of::<SettingsHeader>() == 0x10);

/// Marker for the four POD payloads copied verbatim by Eden's settings service.
///
/// Every bit pattern must be valid because `LoadSettingsFile` reads the complete payload from
/// disk. Implementations are intentionally limited to the four upstream settings formats.
trait SettingsPayload: Copy {}

impl SettingsPayload for SystemSettings {}
impl SettingsPayload for PrivateSettings {}
impl SettingsPayload for DeviceSettings {}
impl SettingsPayload for ApplnSettings {}

fn settings_payload_as_bytes<T: SettingsPayload>(settings: &T) -> &[u8] {
    unsafe {
        core::slice::from_raw_parts(
            (settings as *const T).cast::<u8>(),
            core::mem::size_of::<T>(),
        )
    }
}

fn read_settings_payload<T: SettingsPayload>(file: &mut File) -> std::io::Result<T> {
    let mut settings = MaybeUninit::<T>::uninit();
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(
            settings.as_mut_ptr().cast::<u8>(),
            core::mem::size_of::<T>(),
        )
    };
    file.read_exact(bytes)?;
    Ok(unsafe { settings.assume_init() })
}

fn system_clock_context_to_bytes(context: SystemClockContext) -> [u8; 0x20] {
    unsafe { core::mem::transmute(context) }
}

fn system_clock_context_from_bytes(bytes: [u8; 0x20]) -> SystemClockContext {
    unsafe { core::mem::transmute(bytes) }
}

fn steady_clock_time_point_to_bytes(time_point: SteadyClockTimePoint) -> [u8; 0x18] {
    unsafe { core::mem::transmute(time_point) }
}

fn steady_clock_time_point_from_bytes(bytes: [u8; 0x18]) -> SteadyClockTimePoint {
    unsafe { core::mem::transmute(bytes) }
}

/// IPC command IDs for ISystemSettingsServer ("set:sys").
/// Matches upstream system_settings_server.cpp lines 94-303.
pub mod commands {
    pub const SET_LANGUAGE_CODE: u32 = 0;
    pub const GET_FIRMWARE_VERSION: u32 = 3;
    pub const GET_FIRMWARE_VERSION2: u32 = 4;
    pub const GET_LOCK_SCREEN_FLAG: u32 = 7;
    pub const SET_LOCK_SCREEN_FLAG: u32 = 8;
    pub const GET_EXTERNAL_STEADY_CLOCK_SOURCE_ID: u32 = 13;
    pub const SET_EXTERNAL_STEADY_CLOCK_SOURCE_ID: u32 = 14;
    pub const GET_USER_SYSTEM_CLOCK_CONTEXT: u32 = 15;
    pub const SET_USER_SYSTEM_CLOCK_CONTEXT: u32 = 16;
    pub const GET_ACCOUNT_SETTINGS: u32 = 17;
    pub const SET_ACCOUNT_SETTINGS: u32 = 18;
    pub const GET_EULA_VERSIONS: u32 = 21;
    pub const SET_EULA_VERSIONS: u32 = 22;
    pub const GET_COLOR_SET_ID: u32 = 23;
    pub const SET_COLOR_SET_ID: u32 = 24;
    pub const GET_NOTIFICATION_SETTINGS: u32 = 29;
    pub const SET_NOTIFICATION_SETTINGS: u32 = 30;
    pub const GET_ACCOUNT_NOTIFICATION_SETTINGS: u32 = 31;
    pub const SET_ACCOUNT_NOTIFICATION_SETTINGS: u32 = 32;
    pub const GET_VIBRATION_MASTER_VOLUME: u32 = 35;
    pub const SET_VIBRATION_MASTER_VOLUME: u32 = 36;
    pub const GET_SETTINGS_ITEM_VALUE_SIZE: u32 = 37;
    pub const GET_SETTINGS_ITEM_VALUE: u32 = 38;
    pub const GET_TV_SETTINGS: u32 = 39;
    pub const SET_TV_SETTINGS: u32 = 40;
    pub const GET_AUDIO_OUTPUT_MODE: u32 = 43;
    pub const SET_AUDIO_OUTPUT_MODE: u32 = 44;
    pub const GET_SPEAKER_AUTO_MUTE_FLAG: u32 = 45;
    pub const SET_SPEAKER_AUTO_MUTE_FLAG: u32 = 46;
    pub const GET_QUEST_FLAG: u32 = 47;
    pub const SET_QUEST_FLAG: u32 = 48;
    pub const GET_DEVICE_TIME_ZONE_LOCATION_NAME: u32 = 53;
    pub const SET_DEVICE_TIME_ZONE_LOCATION_NAME: u32 = 54;
    pub const SET_REGION_CODE: u32 = 57;
    pub const GET_NETWORK_SYSTEM_CLOCK_CONTEXT: u32 = 58;
    pub const SET_NETWORK_SYSTEM_CLOCK_CONTEXT: u32 = 59;
    pub const IS_USER_SYSTEM_CLOCK_AUTOMATIC_CORRECTION_ENABLED: u32 = 60;
    pub const SET_USER_SYSTEM_CLOCK_AUTOMATIC_CORRECTION_ENABLED: u32 = 61;
    pub const GET_DEBUG_MODE_FLAG: u32 = 62;
    pub const GET_PRIMARY_ALBUM_STORAGE: u32 = 63;
    pub const SET_PRIMARY_ALBUM_STORAGE: u32 = 64;
    pub const GET_BATTERY_LOT: u32 = 67;
    pub const GET_SERIAL_NUMBER: u32 = 68;
    pub const GET_NFC_ENABLE_FLAG: u32 = 69;
    pub const SET_NFC_ENABLE_FLAG: u32 = 70;
    pub const GET_SLEEP_SETTINGS: u32 = 71;
    pub const SET_SLEEP_SETTINGS: u32 = 72;
    pub const GET_WIRELESS_LAN_ENABLE_FLAG: u32 = 73;
    pub const SET_WIRELESS_LAN_ENABLE_FLAG: u32 = 74;
    pub const GET_INITIAL_LAUNCH_SETTINGS: u32 = 75;
    pub const SET_INITIAL_LAUNCH_SETTINGS: u32 = 76;
    pub const GET_DEVICE_NICK_NAME: u32 = 77;
    pub const SET_DEVICE_NICK_NAME: u32 = 78;
    pub const GET_PRODUCT_MODEL: u32 = 79;
    pub const GET_BLUETOOTH_ENABLE_FLAG: u32 = 88;
    pub const SET_BLUETOOTH_ENABLE_FLAG: u32 = 89;
    pub const GET_MII_AUTHOR_ID: u32 = 90;
    pub const GET_AUTO_UPDATE_ENABLE_FLAG: u32 = 95;
    pub const SET_AUTO_UPDATE_ENABLE_FLAG: u32 = 96;
    pub const GET_BATTERY_PERCENTAGE_FLAG: u32 = 99;
    pub const SET_BATTERY_PERCENTAGE_FLAG: u32 = 100;
    pub const SET_EXTERNAL_STEADY_CLOCK_INTERNAL_OFFSET: u32 = 105;
    pub const GET_EXTERNAL_STEADY_CLOCK_INTERNAL_OFFSET: u32 = 106;
    pub const GET_PUSH_NOTIFICATION_ACTIVITY_MODE_ON_SLEEP: u32 = 120;
    pub const SET_PUSH_NOTIFICATION_ACTIVITY_MODE_ON_SLEEP: u32 = 121;
    pub const GET_ERROR_REPORT_SHARE_PERMISSION: u32 = 124;
    pub const SET_ERROR_REPORT_SHARE_PERMISSION: u32 = 125;
    pub const GET_APPLET_LAUNCH_FLAGS: u32 = 126;
    pub const SET_APPLET_LAUNCH_FLAGS: u32 = 127;
    pub const GET_KEYBOARD_LAYOUT: u32 = 136;
    pub const SET_KEYBOARD_LAYOUT: u32 = 137;
    pub const GET_DEVICE_TIME_ZONE_LOCATION_UPDATED_TIME: u32 = 150;
    pub const SET_DEVICE_TIME_ZONE_LOCATION_UPDATED_TIME: u32 = 151;
    pub const GET_USER_SYSTEM_CLOCK_AUTOMATIC_CORRECTION_UPDATED_TIME: u32 = 152;
    pub const SET_USER_SYSTEM_CLOCK_AUTOMATIC_CORRECTION_UPDATED_TIME: u32 = 153;
    pub const GET_CHINESE_TRADITIONAL_INPUT_METHOD: u32 = 170;
    pub const GET_HOME_MENU_SCHEME: u32 = 174;
    pub const GET_PLATFORM_REGION: u32 = 183;
    pub const SET_PLATFORM_REGION: u32 = 184;
    pub const GET_HOME_MENU_SCHEME_MODEL: u32 = 185;
    pub const GET_TOUCH_SCREEN_MODE: u32 = 187;
    pub const SET_TOUCH_SCREEN_MODE: u32 = 188;
    pub const GET_FIELD_TESTING_FLAG: u32 = 201;
    pub const GET_HEADPHONE_VOLUME_UPDATE_FLAG: u32 = 117;
    pub const SET_HEADPHONE_VOLUME_UPDATE_FLAG: u32 = 118;
    pub const GET_PANEL_CRC_MODE: u32 = 203;
    pub const SET_PANEL_CRC_MODE: u32 = 204;
}

/// ISystemSettingsServer — "set:sys" service.
///
/// Corresponds to `ISystemSettingsServer` in upstream system_settings_server.h.
///
/// Holds system, private, device, and application settings matching
/// upstream's m_system_settings, m_private_settings, m_device_settings, m_appln_settings.
pub struct ISystemSettingsServer {
    system_settings: SystemSettings,
    private_settings: PrivateSettings,
    device_settings: DeviceSettings,
    appln_settings: ApplnSettings,
    #[cfg(test)]
    persistence_enabled: bool,
}

impl ISystemSettingsServer {
    pub fn new() -> Self {
        Self::new_impl(true)
    }

    fn new_impl(setup_persistence: bool) -> Self {
        let mut server = ManuallyDrop::new(Self {
            system_settings: default_system_settings(),
            private_settings: default_private_settings(),
            device_settings: default_device_settings(),
            appln_settings: default_appln_settings(),
            #[cfg(test)]
            persistence_enabled: setup_persistence,
        });

        if setup_persistence {
            server.setup_settings();
        }

        server.system_settings.region_code =
            *common::settings::values().region_index.get_value() as u32;
        let user_system_clock_context =
            system_clock_context_to_bytes(server.system_settings.user_system_clock_context);
        server.system_settings.eula_versions[0] = EulaVersion {
            version: 0x10000,
            region_code: server.system_settings.region_code,
            clock_type: EulaVersionClockType::SteadyClock as u32,
            _padding: [0; 4],
            system_clock_context: user_system_clock_context,
        };
        server.system_settings.eula_version_count = 1;

        ManuallyDrop::into_inner(server)
    }

    #[cfg(test)]
    fn new_for_test() -> Self {
        Self::new_impl(false)
    }

    fn load_settings_file<T: SettingsPayload>(
        path: &Path,
        settings: &mut T,
        default_func: impl Fn() -> T,
    ) -> bool {
        if !common::fs::fs::create_dirs(path) {
            return false;
        }

        let settings_file = path.join("settings.dat");
        let exists = settings_file.exists();
        let expected_size =
            (core::mem::size_of::<SettingsHeader>() + core::mem::size_of::<T>()) as u64;
        let file_size_ok = exists
            && settings_file
                .metadata()
                .is_ok_and(|metadata| metadata.len() == expected_size);

        let reset_to_default = || -> bool {
            let default_settings = default_func();
            let header = SettingsHeader {
                magic: SETTINGS_MAGIC,
                version: SETTINGS_VERSION,
                reserved: 0,
            };
            let mut file = match File::create(&settings_file) {
                Ok(file) => file,
                Err(error) => {
                    log::error!(
                        "Failed to create settings file {}: {}",
                        settings_file.display(),
                        error
                    );
                    return false;
                }
            };
            let mut header_bytes = [0u8; core::mem::size_of::<SettingsHeader>()];
            header_bytes[0..8].copy_from_slice(&header.magic.to_ne_bytes());
            header_bytes[8..12].copy_from_slice(&header.version.to_ne_bytes());
            header_bytes[12..16].copy_from_slice(&header.reserved.to_ne_bytes());
            file.write_all(&header_bytes).is_ok()
                && file
                    .write_all(settings_payload_as_bytes(&default_settings))
                    .is_ok()
                && file.flush().is_ok()
        };

        let is_header_valid = |file: &mut File| -> bool {
            let mut header = [0u8; core::mem::size_of::<SettingsHeader>()];
            if file.read_exact(&mut header).is_err() {
                return false;
            }
            let magic = u64::from_ne_bytes(header[0..8].try_into().unwrap());
            let version = u32::from_ne_bytes(header[8..12].try_into().unwrap());
            magic == SETTINGS_MAGIC && version >= SETTINGS_VERSION
        };

        if (!exists || !file_size_ok) && !reset_to_default() {
            return false;
        }

        let mut file = File::open(&settings_file).ok();
        if !file.as_mut().is_some_and(&is_header_valid) {
            drop(file);
            if !reset_to_default() {
                return false;
            }
            file = File::open(&settings_file).ok();
            if !file.as_mut().is_some_and(&is_header_valid) {
                return false;
            }
        }

        match read_settings_payload(file.as_mut().unwrap()) {
            Ok(loaded_settings) => {
                *settings = loaded_settings;
                true
            }
            Err(error) => {
                log::error!(
                    "Failed to read settings payload {}: {}",
                    settings_file.display(),
                    error
                );
                false
            }
        }
    }

    fn store_settings_file<T: SettingsPayload>(path: &Path, settings: &T) -> bool {
        if !common::fs::fs::is_dir(path) {
            return false;
        }

        let settings_tmp_file = path.join("settings.tmp");
        let settings_file = path.join("settings.dat");
        let mut file = match File::create(&settings_tmp_file) {
            Ok(file) => file,
            Err(_) => return false,
        };
        let header = SettingsHeader {
            magic: SETTINGS_MAGIC,
            version: SETTINGS_VERSION,
            reserved: 0,
        };
        let mut header_bytes = [0u8; core::mem::size_of::<SettingsHeader>()];
        header_bytes[0..8].copy_from_slice(&header.magic.to_ne_bytes());
        header_bytes[8..12].copy_from_slice(&header.version.to_ne_bytes());
        header_bytes[12..16].copy_from_slice(&header.reserved.to_ne_bytes());
        let _ = file.write_all(&header_bytes);
        let _ = file.write_all(settings_payload_as_bytes(settings));
        drop(file);

        fs::rename(settings_tmp_file, settings_file).is_ok()
    }

    fn setup_settings(&mut self) {
        let nand_dir =
            common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::NANDDir);
        let system_dir = nand_dir.join("system/save/8000000000000050");
        assert!(Self::load_settings_file(
            &system_dir,
            &mut self.system_settings,
            default_system_settings,
        ));

        let private_dir = nand_dir.join("system/save/8000000000000052");
        assert!(Self::load_settings_file(
            &private_dir,
            &mut self.private_settings,
            default_private_settings,
        ));

        let device_dir = nand_dir.join("system/save/8000000000000053");
        assert!(Self::load_settings_file(
            &device_dir,
            &mut self.device_settings,
            default_device_settings,
        ));

        let appln_dir = nand_dir.join("system/save/8000000000000054");
        assert!(Self::load_settings_file(
            &appln_dir,
            &mut self.appln_settings,
            default_appln_settings,
        ));
    }

    fn store_settings(&self) {
        let nand_dir =
            common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::NANDDir);
        let system_dir = nand_dir.join("system/save/8000000000000050");
        if !Self::store_settings_file(&system_dir, &self.system_settings) {
            log::error!("Failed to store System settings");
        }

        let private_dir = nand_dir.join("system/save/8000000000000052");
        if !Self::store_settings_file(&private_dir, &self.private_settings) {
            log::error!("Failed to store Private settings");
        }

        let device_dir = nand_dir.join("system/save/8000000000000053");
        if !Self::store_settings_file(&device_dir, &self.device_settings) {
            log::error!("Failed to store Device settings");
        }

        let appln_dir = nand_dir.join("system/save/8000000000000054");
        if !Self::store_settings_file(&appln_dir, &self.appln_settings) {
            log::error!("Failed to store ApplLn settings");
        }
    }

    fn set_save_needed(&mut self) {
        #[cfg(test)]
        if !self.persistence_enabled {
            return;
        }
        self.store_settings();
    }

    // --- Getter/Setter implementations matching upstream ---

    pub fn set_language_code(&mut self, language_code: u64) {
        log::debug!("ISystemSettingsServer::SetLanguageCode called");
        self.system_settings.language_code = language_code;
        if let Some(index) = AVAILABLE_LANGUAGE_CODES
            .iter()
            .position(|candidate| *candidate as u64 == language_code)
        {
            let current = *common::settings::values().language_index.get_value() as u32 as usize;
            if index >= current {
                if let Some(language) = common::settings_enums::Language::from_u32(index as u32) {
                    common::settings::values_mut()
                        .language_index
                        .set_value(language);
                }
            }
        }
        self.set_save_needed();
    }

    pub fn get_firmware_version(&self) -> FirmwareVersionFormat {
        log::debug!("ISystemSettingsServer::GetFirmwareVersion called");
        // Return a default firmware version. Full implementation would
        // read from system archives.
        let mut fw = FirmwareVersionFormat::default();
        fw.major = 18;
        fw.minor = 0;
        fw.micro = 0;
        let display = b"18.0.0";
        fw.display_version[..display.len()].copy_from_slice(display);
        let title = b"NintendoSDK Firmware for NX 18.0.0-1.0";
        fw.display_title[..title.len()].copy_from_slice(title);
        fw
    }

    pub fn get_firmware_version2(&self) -> FirmwareVersionFormat {
        log::debug!("ISystemSettingsServer::GetFirmwareVersion2 called");
        self.get_firmware_version()
    }

    pub fn get_lock_screen_flag(&self) -> bool {
        log::debug!("ISystemSettingsServer::GetLockScreenFlag called");
        self.system_settings.lock_screen_flag != 0
    }

    pub fn set_lock_screen_flag(&mut self, flag: bool) {
        log::debug!("ISystemSettingsServer::SetLockScreenFlag called");
        self.system_settings.lock_screen_flag = u8::from(flag);
        self.set_save_needed();
    }

    pub fn get_external_steady_clock_source_id(&self) -> [u8; 16] {
        log::debug!("ISystemSettingsServer::GetExternalSteadyClockSourceId called");
        self.private_settings.external_clock_source_id
    }

    pub fn set_external_steady_clock_source_id(&mut self, id: [u8; 16]) {
        log::debug!("ISystemSettingsServer::SetExternalSteadyClockSourceId called");
        self.private_settings.external_clock_source_id = id;
        self.set_save_needed();
    }

    pub fn get_user_system_clock_context(&self) -> [u8; 0x20] {
        log::debug!("ISystemSettingsServer::GetUserSystemClockContext called");
        system_clock_context_to_bytes(self.system_settings.user_system_clock_context)
    }

    pub fn set_user_system_clock_context(&mut self, context: [u8; 0x20]) {
        log::debug!("ISystemSettingsServer::SetUserSystemClockContext called");
        self.system_settings.user_system_clock_context = system_clock_context_from_bytes(context);
        self.set_save_needed();
    }

    pub fn get_account_settings(&self) -> AccountSettings {
        log::debug!("ISystemSettingsServer::GetAccountSettings called");
        self.system_settings.account_settings
    }

    pub fn set_account_settings(&mut self, settings: AccountSettings) {
        log::debug!("ISystemSettingsServer::SetAccountSettings called");
        self.system_settings.account_settings = settings;
        self.set_save_needed();
    }

    pub fn get_eula_versions(&self) -> (i32, &[EulaVersion]) {
        log::debug!("ISystemSettingsServer::GetEulaVersions called");
        let count = self
            .system_settings
            .eula_version_count
            .clamp(0, self.system_settings.eula_versions.len() as i32);
        (count, &self.system_settings.eula_versions[..count as usize])
    }

    pub fn set_eula_versions(&mut self, versions: Vec<EulaVersion>) {
        log::debug!("ISystemSettingsServer::SetEulaVersions called");
        assert!(versions.len() <= self.system_settings.eula_versions.len());
        self.system_settings.eula_versions = [EulaVersion::default(); 32];
        self.system_settings.eula_versions[..versions.len()].copy_from_slice(&versions);
        self.system_settings.eula_version_count = versions.len() as i32;
        self.set_save_needed();
    }

    pub fn get_color_set_id(&self) -> u32 {
        log::debug!("ISystemSettingsServer::GetColorSetId called");
        self.system_settings.color_set_id
    }

    pub fn set_color_set_id(&mut self, color_set: u32) {
        log::debug!("ISystemSettingsServer::SetColorSetId called");
        self.system_settings.color_set_id = color_set;
        self.set_save_needed();
    }

    pub fn get_notification_settings(&self) -> NotificationSettings {
        log::debug!("ISystemSettingsServer::GetNotificationSettings called");
        self.system_settings.notification_settings
    }

    pub fn set_notification_settings(&mut self, settings: NotificationSettings) {
        log::debug!("ISystemSettingsServer::SetNotificationSettings called");
        self.system_settings.notification_settings = settings;
        self.set_save_needed();
    }

    pub fn get_account_notification_settings(&self) -> (i32, &[AccountNotificationSettings]) {
        log::debug!("ISystemSettingsServer::GetAccountNotificationSettings called");
        let count = self
            .system_settings
            .account_notification_settings_count
            .clamp(
                0,
                self.system_settings.account_notification_settings.len() as i32,
            );
        (
            count,
            &self.system_settings.account_notification_settings[..count as usize],
        )
    }

    pub fn set_account_notification_settings(
        &mut self,
        settings: Vec<AccountNotificationSettings>,
    ) {
        log::debug!("ISystemSettingsServer::SetAccountNotificationSettings called");
        assert!(settings.len() <= self.system_settings.account_notification_settings.len());
        self.system_settings.account_notification_settings =
            [AccountNotificationSettings::default(); 8];
        self.system_settings.account_notification_settings[..settings.len()]
            .copy_from_slice(&settings);
        self.system_settings.account_notification_settings_count = settings.len() as i32;
        self.set_save_needed();
    }

    pub fn get_vibration_master_volume(&self) -> f32 {
        log::debug!("ISystemSettingsServer::GetVibrationMasterVolume called");
        self.system_settings.vibration_master_volume
    }

    pub fn set_vibration_master_volume(&mut self, volume: f32) {
        log::debug!("ISystemSettingsServer::SetVibrationMasterVolume called");
        self.system_settings.vibration_master_volume = volume;
        self.set_save_needed();
    }

    pub fn get_tv_settings(&self) -> TvSettings {
        log::debug!("ISystemSettingsServer::GetTvSettings called");
        self.system_settings.tv_settings
    }

    pub fn set_tv_settings(&mut self, settings: TvSettings) {
        log::debug!("ISystemSettingsServer::SetTvSettings called");
        self.system_settings.tv_settings = settings;
        self.set_save_needed();
    }

    pub fn get_audio_output_mode(&self, target: u32) -> u32 {
        log::debug!(
            "ISystemSettingsServer::GetAudioOutputMode called, target={:?}",
            target
        );
        match target {
            1 => self.system_settings.audio_output_mode_hdmi,
            2 => self.system_settings.audio_output_mode_speaker,
            3 => self.system_settings.audio_output_mode_headphone,
            4 => self.system_settings.audio_output_mode_type3,
            5 => self.system_settings.audio_output_mode_type4,
            _ => {
                log::error!("Invalid audio output mode target {}", target);
                AudioOutputMode::Ch1 as u32
            }
        }
    }

    pub fn set_audio_output_mode(&mut self, target: u32, mode: u32) {
        log::debug!("ISystemSettingsServer::SetAudioOutputMode called");
        match target {
            1 => self.system_settings.audio_output_mode_hdmi = mode,
            2 => self.system_settings.audio_output_mode_speaker = mode,
            3 => self.system_settings.audio_output_mode_headphone = mode,
            4 => self.system_settings.audio_output_mode_type3 = mode,
            5 => self.system_settings.audio_output_mode_type4 = mode,
            _ => log::error!("Invalid audio output mode target {}", target),
        }
        self.set_save_needed();
    }

    pub fn get_speaker_auto_mute_flag(&self) -> bool {
        log::debug!("ISystemSettingsServer::GetSpeakerAutoMuteFlag called");
        self.system_settings.force_mute_on_headphone_removed != 0
    }

    pub fn set_speaker_auto_mute_flag(&mut self, flag: bool) {
        log::debug!("ISystemSettingsServer::SetSpeakerAutoMuteFlag called");
        self.system_settings.force_mute_on_headphone_removed = u8::from(flag);
        self.set_save_needed();
    }

    pub fn get_quest_flag(&self) -> u8 {
        log::debug!("ISystemSettingsServer::GetQuestFlag called");
        self.system_settings.quest_flag
    }

    pub fn set_quest_flag(&mut self, flag: u8) {
        log::debug!("ISystemSettingsServer::SetQuestFlag called");
        self.system_settings.quest_flag = flag;
        self.set_save_needed();
    }

    pub fn get_device_time_zone_location_name(&self) -> [u8; 0x24] {
        log::debug!("ISystemSettingsServer::GetDeviceTimeZoneLocationName called");
        self.system_settings.device_time_zone_location_name
    }

    pub fn set_device_time_zone_location_name(&mut self, name: [u8; 0x24]) {
        log::debug!("ISystemSettingsServer::SetDeviceTimeZoneLocationName called");
        self.system_settings.device_time_zone_location_name = name;
        self.set_save_needed();
    }

    pub fn set_region_code(&mut self, region: u32) {
        log::debug!("ISystemSettingsServer::SetRegionCode called");
        self.system_settings.region_code = region;
        if let Some(region) = common::settings_enums::Region::from_u32(region) {
            common::settings::values_mut()
                .region_index
                .set_value(region);
        }
        self.set_save_needed();
    }

    pub fn get_network_system_clock_context(&self) -> [u8; 0x20] {
        log::debug!("ISystemSettingsServer::GetNetworkSystemClockContext called");
        system_clock_context_to_bytes(self.system_settings.network_system_clock_context)
    }

    pub fn set_network_system_clock_context(&mut self, context: [u8; 0x20]) {
        log::debug!("ISystemSettingsServer::SetNetworkSystemClockContext called");
        self.system_settings.network_system_clock_context =
            system_clock_context_from_bytes(context);
        self.set_save_needed();
    }

    pub fn is_user_system_clock_automatic_correction_enabled(&self) -> bool {
        log::debug!("ISystemSettingsServer::IsUserSystemClockAutomaticCorrectionEnabled called");
        self.system_settings
            .user_system_clock_automatic_correction_enabled
            != 0
    }

    pub fn set_user_system_clock_automatic_correction_enabled(&mut self, enabled: bool) {
        log::debug!("ISystemSettingsServer::SetUserSystemClockAutomaticCorrectionEnabled called");
        self.system_settings
            .user_system_clock_automatic_correction_enabled = u8::from(enabled);
        self.set_save_needed();
    }

    pub fn get_debug_mode_flag(&self) -> bool {
        log::debug!("ISystemSettingsServer::GetDebugModeFlag called");
        // Upstream reads from GetSettingsItemValueImpl("settings_debug", "is_debug_mode_enabled").
        // Default to false (non-debug mode).
        false
    }

    pub fn get_primary_album_storage(&self) -> u32 {
        log::debug!("ISystemSettingsServer::GetPrimaryAlbumStorage called");
        self.system_settings.primary_album_storage
    }

    pub fn set_primary_album_storage(&mut self, storage: u32) {
        log::debug!("ISystemSettingsServer::SetPrimaryAlbumStorage called");
        self.system_settings.primary_album_storage = storage;
        self.set_save_needed();
    }

    pub fn get_battery_lot(&self) -> BatteryLot {
        log::debug!("ISystemSettingsServer::GetBatteryLot called");
        BatteryLot::default()
    }

    pub fn get_serial_number(&self) -> SerialNumber {
        log::debug!("ISystemSettingsServer::GetSerialNumber called");
        SerialNumber::default()
    }

    pub fn get_nfc_enable_flag(&self) -> bool {
        log::debug!("ISystemSettingsServer::GetNfcEnableFlag called");
        self.system_settings.nfc_enable_flag != 0
    }

    pub fn set_nfc_enable_flag(&mut self, flag: bool) {
        log::debug!("ISystemSettingsServer::SetNfcEnableFlag called");
        self.system_settings.nfc_enable_flag = u8::from(flag);
        self.set_save_needed();
    }

    pub fn get_sleep_settings(&self) -> SleepSettings {
        log::debug!("ISystemSettingsServer::GetSleepSettings called");
        self.system_settings.sleep_settings
    }

    pub fn set_sleep_settings(&mut self, settings: SleepSettings) {
        log::debug!("ISystemSettingsServer::SetSleepSettings called");
        self.system_settings.sleep_settings = settings;
        self.set_save_needed();
    }

    pub fn get_wireless_lan_enable_flag(&self) -> bool {
        log::debug!("ISystemSettingsServer::GetWirelessLanEnableFlag called");
        self.system_settings.wireless_lan_enable_flag != 0
    }

    pub fn set_wireless_lan_enable_flag(&mut self, flag: bool) {
        log::debug!("ISystemSettingsServer::SetWirelessLanEnableFlag called");
        self.system_settings.wireless_lan_enable_flag = u8::from(flag);
        self.set_save_needed();
    }

    pub fn get_initial_launch_settings(&self) -> InitialLaunchSettings {
        log::debug!("ISystemSettingsServer::GetInitialLaunchSettings called");
        InitialLaunchSettings {
            flags: self.system_settings.initial_launch_settings_packed.flags,
            _padding: [0; 4],
            timestamp: self
                .system_settings
                .initial_launch_settings_packed
                .timestamp,
        }
    }

    pub fn set_initial_launch_settings(&mut self, settings: InitialLaunchSettings) {
        log::debug!("ISystemSettingsServer::SetInitialLaunchSettings called");
        self.system_settings.initial_launch_settings_packed.flags = settings.flags;
        self.system_settings
            .initial_launch_settings_packed
            .timestamp = settings.timestamp;
        self.set_save_needed();
    }

    pub fn get_device_nick_name(&self) -> [u8; 0x80] {
        log::debug!("ISystemSettingsServer::GetDeviceNickName called");
        let mut out = [0; 0x80];
        let name = common::settings::values().device_name.get_value().clone();
        let bytes = name.as_bytes();
        let count = bytes.len().min(out.len());
        out[..count].copy_from_slice(&bytes[..count]);
        out
    }

    pub fn set_device_nick_name(&mut self, name: [u8; 0x80]) {
        log::debug!("ISystemSettingsServer::SetDeviceNickName called");
        let length = name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(name.len());
        common::settings::values_mut()
            .device_name
            .set_value(String::from_utf8_lossy(&name[..length]).into_owned());
    }

    pub fn get_product_model(&self) -> u32 {
        log::debug!("ISystemSettingsServer::GetProductModel called");
        // 1 = NX (Switch), matching upstream
        1
    }

    pub fn get_bluetooth_enable_flag(&self) -> bool {
        log::debug!("ISystemSettingsServer::GetBluetoothEnableFlag called");
        self.system_settings.bluetooth_enable_flag != 0
    }

    pub fn set_bluetooth_enable_flag(&mut self, flag: bool) {
        log::debug!("ISystemSettingsServer::SetBluetoothEnableFlag called");
        self.system_settings.bluetooth_enable_flag = u8::from(flag);
        self.set_save_needed();
    }

    pub fn get_mii_author_id(&mut self) -> [u8; 16] {
        log::debug!("ISystemSettingsServer::GetMiiAuthorId called");
        if self.system_settings.mii_author_id == [0; 16] {
            self.system_settings.mii_author_id = UUID::make_default().uuid;
            self.set_save_needed();
        }
        self.system_settings.mii_author_id
    }

    pub fn get_auto_update_enable_flag(&self) -> bool {
        log::debug!("ISystemSettingsServer::GetAutoUpdateEnableFlag called");
        self.system_settings.auto_update_enable_flag != 0
    }

    pub fn set_auto_update_enable_flag(&mut self, flag: bool) {
        log::debug!("ISystemSettingsServer::SetAutoUpdateEnableFlag called");
        self.system_settings.auto_update_enable_flag = u8::from(flag);
        self.set_save_needed();
    }

    pub fn get_battery_percentage_flag(&self) -> bool {
        log::debug!("ISystemSettingsServer::GetBatteryPercentageFlag called");
        self.system_settings.battery_percentage_flag != 0
    }

    pub fn set_battery_percentage_flag(&mut self, flag: bool) {
        log::debug!("ISystemSettingsServer::SetBatteryPercentageFlag called");
        self.system_settings.battery_percentage_flag = u8::from(flag);
        self.set_save_needed();
    }

    pub fn set_external_steady_clock_internal_offset(&mut self, offset: i64) {
        log::debug!("ISystemSettingsServer::SetExternalSteadyClockInternalOffset called");
        self.private_settings.external_steady_clock_internal_offset = offset;
        self.set_save_needed();
    }

    pub fn get_external_steady_clock_internal_offset(&self) -> i64 {
        log::debug!("ISystemSettingsServer::GetExternalSteadyClockInternalOffset called");
        self.private_settings.external_steady_clock_internal_offset
    }

    pub fn get_push_notification_activity_mode_on_sleep(&self) -> i32 {
        log::debug!("ISystemSettingsServer::GetPushNotificationActivityModeOnSleep called");
        self.system_settings
            .push_notification_activity_mode_on_sleep
    }

    pub fn set_push_notification_activity_mode_on_sleep(&mut self, mode: i32) {
        log::debug!("ISystemSettingsServer::SetPushNotificationActivityModeOnSleep called");
        self.system_settings
            .push_notification_activity_mode_on_sleep = mode;
        self.set_save_needed();
    }

    pub fn get_error_report_share_permission(&self) -> u32 {
        log::debug!("ISystemSettingsServer::GetErrorReportSharePermission called");
        self.system_settings.error_report_share_permission
    }

    pub fn set_error_report_share_permission(&mut self, permission: u32) {
        log::debug!("ISystemSettingsServer::SetErrorReportSharePermission called");
        self.system_settings.error_report_share_permission = permission;
        self.set_save_needed();
    }

    pub fn get_applet_launch_flags(&self) -> u32 {
        log::debug!("ISystemSettingsServer::GetAppletLaunchFlags called");
        self.system_settings.applet_launch_flag
    }

    pub fn set_applet_launch_flags(&mut self, flags: u32) {
        log::debug!("ISystemSettingsServer::SetAppletLaunchFlags called");
        self.system_settings.applet_launch_flag = flags;
        self.set_save_needed();
    }

    pub fn get_keyboard_layout(&self) -> u32 {
        log::debug!("ISystemSettingsServer::GetKeyboardLayout called");
        self.system_settings.keyboard_layout
    }

    pub fn set_keyboard_layout(&mut self, layout: u32) {
        log::debug!("ISystemSettingsServer::SetKeyboardLayout called");
        self.system_settings.keyboard_layout = layout;
        self.set_save_needed();
    }

    pub fn get_device_time_zone_location_updated_time(&self) -> [u8; 0x18] {
        log::debug!("ISystemSettingsServer::GetDeviceTimeZoneLocationUpdatedTime called");
        steady_clock_time_point_to_bytes(
            self.system_settings.device_time_zone_location_updated_time,
        )
    }

    pub fn set_device_time_zone_location_updated_time(&mut self, time: [u8; 0x18]) {
        log::debug!("ISystemSettingsServer::SetDeviceTimeZoneLocationUpdatedTime called");
        self.system_settings.device_time_zone_location_updated_time =
            steady_clock_time_point_from_bytes(time);
        self.set_save_needed();
    }

    pub fn get_user_system_clock_automatic_correction_updated_time(&self) -> [u8; 0x18] {
        log::debug!(
            "ISystemSettingsServer::GetUserSystemClockAutomaticCorrectionUpdatedTime called"
        );
        steady_clock_time_point_to_bytes(
            self.system_settings
                .user_system_clock_automatic_correction_updated_time_point,
        )
    }

    pub fn set_user_system_clock_automatic_correction_updated_time(&mut self, time: [u8; 0x18]) {
        log::debug!(
            "ISystemSettingsServer::SetUserSystemClockAutomaticCorrectionUpdatedTime called"
        );
        self.system_settings
            .user_system_clock_automatic_correction_updated_time_point =
            steady_clock_time_point_from_bytes(time);
        self.set_save_needed();
    }

    pub fn get_chinese_traditional_input_method(&self) -> u32 {
        log::debug!("ISystemSettingsServer::GetChineseTraditionalInputMethod called");
        self.system_settings.chinese_traditional_input_method
    }

    pub fn get_home_menu_scheme(&self) -> HomeMenuScheme {
        log::debug!("ISystemSettingsServer::GetHomeMenuScheme called");
        HomeMenuScheme {
            main: 0xFF323232,
            back: 0xFF323232,
            sub: 0xFFFFFFFF,
            bezel: 0xFFFFFFFF,
            extra: 0xFF000000,
        }
    }

    pub fn get_home_menu_scheme_model(&self) -> u32 {
        log::debug!("ISystemSettingsServer::GetHomeMenuSchemeModel called");
        0
    }

    pub fn get_touch_screen_mode(&self) -> u32 {
        log::debug!("ISystemSettingsServer::GetTouchScreenMode called");
        self.system_settings.touch_screen_mode
    }

    pub fn set_touch_screen_mode(&mut self, mode: u32) {
        log::debug!("ISystemSettingsServer::SetTouchScreenMode called");
        self.system_settings.touch_screen_mode = mode;
        self.set_save_needed();
    }

    pub fn get_platform_region(&self) -> i32 {
        log::debug!("ISystemSettingsServer::GetPlatformRegion called");
        PlatformRegion::Global as i32
    }

    pub fn set_platform_region(&mut self, _region: i32) {
        log::debug!("ISystemSettingsServer::SetPlatformRegion called");
    }

    pub fn get_field_testing_flag(&self) -> bool {
        log::debug!("ISystemSettingsServer::GetFieldTestingFlag called");
        self.system_settings.field_testing_flag != 0
    }

    pub fn get_panel_crc_mode(&self) -> i32 {
        log::debug!("ISystemSettingsServer::GetPanelCrcMode called");
        self.system_settings.panel_crc_mode
    }

    pub fn set_panel_crc_mode(&mut self, mode: i32) {
        log::debug!("ISystemSettingsServer::SetPanelCrcMode called");
        self.system_settings.panel_crc_mode = mode;
        self.set_save_needed();
    }

    pub fn get_headphone_volume_update_flag(&self) -> bool {
        log::debug!("ISystemSettingsServer::GetHeadphoneVolumeUpdateFlag called");
        self.system_settings.heaphone_volume_update_flag != 0
    }

    pub fn set_headphone_volume_update_flag(&mut self, flag: bool) {
        log::debug!("ISystemSettingsServer::SetHeadphoneVolumeUpdateFlag called");
        self.system_settings.heaphone_volume_update_flag = u8::from(flag);
        self.set_save_needed();
    }

    pub fn get_settings_item_value_bytes(&self, category: &str, name: &str) -> Option<Vec<u8>> {
        get_settings().get(category)?.get(name).cloned()
    }

    pub fn get_settings_item_value_i32(&self, category: &str, name: &str) -> Option<i32> {
        let bytes = self.get_settings_item_value_bytes(category, name)?;
        let raw: [u8; 4] = bytes.as_slice().try_into().ok()?;
        Some(i32::from_le_bytes(raw))
    }
}

impl Drop for ISystemSettingsServer {
    fn drop(&mut self) {
        self.set_save_needed();
    }
}

/// Returns the built-in settings item map, matching upstream `GetSettings()` in
/// system_settings_server.cpp lines 654-696.
///
/// Keys are (category, name) -> Vec<u8> raw value bytes.
fn get_settings() -> BTreeMap<String, BTreeMap<String, Vec<u8>>> {
    fn to_bytes_u64(v: u64) -> Vec<u8> {
        v.to_le_bytes().to_vec()
    }
    fn to_bytes_i32(v: i32) -> Vec<u8> {
        v.to_le_bytes().to_vec()
    }
    fn to_bytes_bool(v: bool) -> Vec<u8> {
        vec![v as u8]
    }

    let mut ret: BTreeMap<String, BTreeMap<String, Vec<u8>>> = BTreeMap::new();

    // AM / hbloader
    ret.entry("hbloader".into())
        .or_default()
        .insert("applet_heap_size".into(), to_bytes_u64(0x0));
    ret.entry("hbloader".into()).or_default().insert(
        "applet_heap_reservation_size".into(),
        to_bytes_u64(0x8600000),
    );

    // Time
    ret.entry("time".into()).or_default().insert(
        "notify_time_to_fs_interval_seconds".into(),
        to_bytes_i32(600),
    );
    ret.entry("time".into()).or_default().insert(
        "standard_network_clock_sufficient_accuracy_minutes".into(),
        to_bytes_i32(43200),
    );
    ret.entry("time".into()).or_default().insert(
        "standard_steady_clock_rtc_update_interval_minutes".into(),
        to_bytes_i32(5),
    );
    ret.entry("time".into()).or_default().insert(
        "standard_steady_clock_test_offset_minutes".into(),
        to_bytes_i32(0),
    );
    ret.entry("time".into()).or_default().insert(
        "standard_user_clock_initial_year".into(),
        to_bytes_i32(2023),
    );

    // HID
    ret.entry("hid".into())
        .or_default()
        .insert("has_rail_interface".into(), to_bytes_bool(true));
    ret.entry("hid".into())
        .or_default()
        .insert("has_sio_mcu".into(), to_bytes_bool(true));
    ret.entry("hid_debug".into())
        .or_default()
        .insert("enables_debugpad".into(), to_bytes_bool(true));
    ret.entry("hid_debug".into())
        .or_default()
        .insert("manages_devices".into(), to_bytes_bool(true));
    ret.entry("hid_debug".into())
        .or_default()
        .insert("manages_touch_ic_i2c".into(), to_bytes_bool(true));
    ret.entry("hid_debug".into())
        .or_default()
        .insert("emulate_future_device".into(), to_bytes_bool(false));
    ret.entry("hid_debug".into())
        .or_default()
        .insert("emulate_mcu_hardware_error".into(), to_bytes_bool(false));
    ret.entry("hid_debug".into())
        .or_default()
        .insert("enables_rail".into(), to_bytes_bool(true));
    ret.entry("hid_debug".into()).or_default().insert(
        "emulate_firmware_update_failure".into(),
        to_bytes_bool(false),
    );
    ret.entry("hid_debug".into())
        .or_default()
        .insert("failure_firmware_update".into(), to_bytes_i32(0));
    ret.entry("hid_debug".into())
        .or_default()
        .insert("ble_disabled".into(), to_bytes_bool(false));
    ret.entry("hid_debug".into())
        .or_default()
        .insert("dscale_disabled".into(), to_bytes_bool(false));
    ret.entry("hid_debug".into())
        .or_default()
        .insert("force_handheld".into(), to_bytes_bool(true));
    ret.entry("hid_debug".into())
        .or_default()
        .insert("disabled_features_per_id".into(), vec![0u8; 0xa8]);
    ret.entry("hid_debug".into()).or_default().insert(
        "touch_firmware_auto_update_disabled".into(),
        to_bytes_bool(false),
    );

    // Mii
    ret.entry("mii".into())
        .or_default()
        .insert("is_db_test_mode_enabled".into(), to_bytes_bool(false));

    // Settings
    ret.entry("settings_debug".into())
        .or_default()
        .insert("is_debug_mode_enabled".into(), to_bytes_bool(false));

    // Error
    ret.entry("err".into())
        .or_default()
        .insert("applet_auto_close".into(), to_bytes_bool(false));

    ret
}

/// GetFirmwareVersionImpl - reads firmware version from system archives.
///
/// Corresponds to upstream `GetFirmwareVersionImpl` in system_settings_server.cpp.
/// This is a simplified version that returns default firmware info.
pub fn get_firmware_version_impl(fw_type: GetFirmwareVersionType) -> FirmwareVersionFormat {
    log::debug!("GetFirmwareVersionImpl called, type={:?}", fw_type);

    let mut out = FirmwareVersionFormat::default();
    out.major = 18;
    out.minor = 0;
    out.micro = 0;

    let display = b"18.0.0";
    out.display_version[..display.len()].copy_from_slice(display);

    let title = b"NintendoSDK Firmware for NX 18.0.0-1.0";
    out.display_title[..title.len()].copy_from_slice(title);

    out
}

// ---------------------------------------------------------------------------
// ServiceFramework wiring — makes ISystemSettingsServer a real IPC service
// ---------------------------------------------------------------------------

/// IPC-facing service wrapper for set:sys.
/// Holds ISystemSettingsServer behind a Mutex for interior mutability
/// (handlers receive &dyn ServiceFramework, not &mut self).
pub struct SystemSettingsService {
    /// Inner settings state. Public for direct service-to-service access,
    /// matching upstream where services call methods directly on handler objects
    /// via ServiceManager::GetService<T>().
    pub inner: Mutex<ISystemSettingsServer>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl SystemSettingsService {
    pub fn new() -> Self {
        Self::new_with_inner(ISystemSettingsServer::new())
    }

    /// Direct service-to-service counterpart of
    /// `ISystemSettingsServer::GetDeviceTimeZoneLocationName`.
    pub fn get_device_time_zone_location_name(&self) -> LocationName {
        self.inner
            .lock()
            .unwrap()
            .get_device_time_zone_location_name()
    }

    /// Direct service-to-service counterpart of
    /// `ISystemSettingsServer::SetDeviceTimeZoneLocationName`.
    pub fn set_device_time_zone_location_name(&self, name: LocationName) {
        self.inner
            .lock()
            .unwrap()
            .set_device_time_zone_location_name(name);
    }

    /// Direct service-to-service counterpart of
    /// `ISystemSettingsServer::GetDeviceTimeZoneLocationUpdatedTime`.
    pub fn get_device_time_zone_location_updated_time(&self) -> SteadyClockTimePoint {
        steady_clock_time_point_from_bytes(
            self.inner
                .lock()
                .unwrap()
                .get_device_time_zone_location_updated_time(),
        )
    }

    /// Direct service-to-service counterpart of
    /// `ISystemSettingsServer::SetDeviceTimeZoneLocationUpdatedTime`.
    pub fn set_device_time_zone_location_updated_time(&self, time_point: SteadyClockTimePoint) {
        self.inner
            .lock()
            .unwrap()
            .set_device_time_zone_location_updated_time(steady_clock_time_point_to_bytes(
                time_point,
            ));
    }

    fn new_with_inner(inner: ISystemSettingsServer) -> Self {
        Self {
            inner: Mutex::new(inner),
            handlers: build_handler_map(&[
                // Matches upstream system_settings_server.cpp constructor function table.
                // All commands that upstream wires to a non-null handler are included here.
                (
                    commands::SET_LANGUAGE_CODE,
                    Some(Self::set_language_code_handler),
                    "SetLanguageCode",
                ),
                (
                    commands::GET_FIRMWARE_VERSION,
                    Some(Self::get_firmware_version_handler),
                    "GetFirmwareVersion",
                ),
                (
                    commands::GET_FIRMWARE_VERSION2,
                    Some(Self::get_firmware_version2_handler),
                    "GetFirmwareVersion2",
                ),
                (
                    commands::GET_LOCK_SCREEN_FLAG,
                    Some(Self::get_lock_screen_flag_handler),
                    "GetLockScreenFlag",
                ),
                (
                    commands::SET_LOCK_SCREEN_FLAG,
                    Some(Self::set_lock_screen_flag_handler),
                    "SetLockScreenFlag",
                ),
                (
                    commands::GET_EXTERNAL_STEADY_CLOCK_SOURCE_ID,
                    Some(Self::get_external_steady_clock_source_id_handler),
                    "GetExternalSteadyClockSourceId",
                ),
                (
                    commands::SET_EXTERNAL_STEADY_CLOCK_SOURCE_ID,
                    Some(Self::set_external_steady_clock_source_id_handler),
                    "SetExternalSteadyClockSourceId",
                ),
                (
                    commands::GET_USER_SYSTEM_CLOCK_CONTEXT,
                    Some(Self::get_user_system_clock_context_handler),
                    "GetUserSystemClockContext",
                ),
                (
                    commands::SET_USER_SYSTEM_CLOCK_CONTEXT,
                    Some(Self::set_user_system_clock_context_handler),
                    "SetUserSystemClockContext",
                ),
                (
                    commands::GET_ACCOUNT_SETTINGS,
                    Some(Self::get_account_settings_handler),
                    "GetAccountSettings",
                ),
                (
                    commands::SET_ACCOUNT_SETTINGS,
                    Some(Self::set_account_settings_handler),
                    "SetAccountSettings",
                ),
                (
                    commands::GET_EULA_VERSIONS,
                    Some(Self::get_eula_versions_handler),
                    "GetEulaVersions",
                ),
                (
                    commands::SET_EULA_VERSIONS,
                    Some(Self::set_eula_versions_handler),
                    "SetEulaVersions",
                ),
                (
                    commands::GET_COLOR_SET_ID,
                    Some(Self::get_color_set_id_handler),
                    "GetColorSetId",
                ),
                (
                    commands::SET_COLOR_SET_ID,
                    Some(Self::set_color_set_id_handler),
                    "SetColorSetId",
                ),
                (
                    commands::GET_NOTIFICATION_SETTINGS,
                    Some(Self::get_notification_settings_handler),
                    "GetNotificationSettings",
                ),
                (
                    commands::SET_NOTIFICATION_SETTINGS,
                    Some(Self::set_notification_settings_handler),
                    "SetNotificationSettings",
                ),
                (
                    commands::GET_ACCOUNT_NOTIFICATION_SETTINGS,
                    Some(Self::get_account_notification_settings_handler),
                    "GetAccountNotificationSettings",
                ),
                (
                    commands::SET_ACCOUNT_NOTIFICATION_SETTINGS,
                    Some(Self::set_account_notification_settings_handler),
                    "SetAccountNotificationSettings",
                ),
                (
                    commands::GET_VIBRATION_MASTER_VOLUME,
                    Some(Self::get_vibration_master_volume_handler),
                    "GetVibrationMasterVolume",
                ),
                (
                    commands::SET_VIBRATION_MASTER_VOLUME,
                    Some(Self::set_vibration_master_volume_handler),
                    "SetVibrationMasterVolume",
                ),
                (
                    commands::GET_SETTINGS_ITEM_VALUE_SIZE,
                    Some(Self::get_settings_item_value_size_handler),
                    "GetSettingsItemValueSize",
                ),
                (
                    commands::GET_SETTINGS_ITEM_VALUE,
                    Some(Self::get_settings_item_value_handler),
                    "GetSettingsItemValue",
                ),
                (
                    commands::GET_TV_SETTINGS,
                    Some(Self::get_tv_settings_handler),
                    "GetTvSettings",
                ),
                (
                    commands::SET_TV_SETTINGS,
                    Some(Self::set_tv_settings_handler),
                    "SetTvSettings",
                ),
                (
                    commands::GET_AUDIO_OUTPUT_MODE,
                    Some(Self::get_audio_output_mode_handler),
                    "GetAudioOutputMode",
                ),
                (
                    commands::SET_AUDIO_OUTPUT_MODE,
                    Some(Self::set_audio_output_mode_handler),
                    "SetAudioOutputMode",
                ),
                (
                    commands::GET_SPEAKER_AUTO_MUTE_FLAG,
                    Some(Self::get_speaker_auto_mute_flag_handler),
                    "GetSpeakerAutoMuteFlag",
                ),
                (
                    commands::SET_SPEAKER_AUTO_MUTE_FLAG,
                    Some(Self::set_speaker_auto_mute_flag_handler),
                    "SetSpeakerAutoMuteFlag",
                ),
                (
                    commands::GET_QUEST_FLAG,
                    Some(Self::get_quest_flag_handler),
                    "GetQuestFlag",
                ),
                (
                    commands::SET_QUEST_FLAG,
                    Some(Self::set_quest_flag_handler),
                    "SetQuestFlag",
                ),
                (
                    commands::GET_DEVICE_TIME_ZONE_LOCATION_NAME,
                    Some(Self::get_device_time_zone_location_name_handler),
                    "GetDeviceTimeZoneLocationName",
                ),
                (
                    commands::SET_DEVICE_TIME_ZONE_LOCATION_NAME,
                    Some(Self::set_device_time_zone_location_name_handler),
                    "SetDeviceTimeZoneLocationName",
                ),
                (
                    commands::SET_REGION_CODE,
                    Some(Self::set_region_code_handler),
                    "SetRegionCode",
                ),
                (
                    commands::GET_NETWORK_SYSTEM_CLOCK_CONTEXT,
                    Some(Self::get_network_system_clock_context_handler),
                    "GetNetworkSystemClockContext",
                ),
                (
                    commands::SET_NETWORK_SYSTEM_CLOCK_CONTEXT,
                    Some(Self::set_network_system_clock_context_handler),
                    "SetNetworkSystemClockContext",
                ),
                (
                    commands::IS_USER_SYSTEM_CLOCK_AUTOMATIC_CORRECTION_ENABLED,
                    Some(Self::is_user_system_clock_automatic_correction_enabled_handler),
                    "IsUserSystemClockAutomaticCorrectionEnabled",
                ),
                (
                    commands::SET_USER_SYSTEM_CLOCK_AUTOMATIC_CORRECTION_ENABLED,
                    Some(Self::set_user_system_clock_automatic_correction_enabled_handler),
                    "SetUserSystemClockAutomaticCorrectionEnabled",
                ),
                (
                    commands::GET_DEBUG_MODE_FLAG,
                    Some(Self::get_debug_mode_flag_handler),
                    "GetDebugModeFlag",
                ),
                (
                    commands::GET_PRIMARY_ALBUM_STORAGE,
                    Some(Self::get_primary_album_storage_handler),
                    "GetPrimaryAlbumStorage",
                ),
                (
                    commands::SET_PRIMARY_ALBUM_STORAGE,
                    Some(Self::set_primary_album_storage_handler),
                    "SetPrimaryAlbumStorage",
                ),
                (
                    commands::GET_BATTERY_LOT,
                    Some(Self::get_battery_lot_handler),
                    "GetBatteryLot",
                ),
                (
                    commands::GET_SERIAL_NUMBER,
                    Some(Self::get_serial_number_handler),
                    "GetSerialNumber",
                ),
                (
                    commands::GET_NFC_ENABLE_FLAG,
                    Some(Self::get_nfc_enable_flag_handler),
                    "GetNfcEnableFlag",
                ),
                (
                    commands::SET_NFC_ENABLE_FLAG,
                    Some(Self::set_nfc_enable_flag_handler),
                    "SetNfcEnableFlag",
                ),
                (
                    commands::GET_SLEEP_SETTINGS,
                    Some(Self::get_sleep_settings_handler),
                    "GetSleepSettings",
                ),
                (
                    commands::SET_SLEEP_SETTINGS,
                    Some(Self::set_sleep_settings_handler),
                    "SetSleepSettings",
                ),
                (
                    commands::GET_WIRELESS_LAN_ENABLE_FLAG,
                    Some(Self::get_wireless_lan_enable_flag_handler),
                    "GetWirelessLanEnableFlag",
                ),
                (
                    commands::SET_WIRELESS_LAN_ENABLE_FLAG,
                    Some(Self::set_wireless_lan_enable_flag_handler),
                    "SetWirelessLanEnableFlag",
                ),
                (
                    commands::GET_INITIAL_LAUNCH_SETTINGS,
                    Some(Self::get_initial_launch_settings_handler),
                    "GetInitialLaunchSettings",
                ),
                (
                    commands::SET_INITIAL_LAUNCH_SETTINGS,
                    Some(Self::set_initial_launch_settings_handler),
                    "SetInitialLaunchSettings",
                ),
                (
                    commands::GET_DEVICE_NICK_NAME,
                    Some(Self::get_device_nick_name_handler),
                    "GetDeviceNickName",
                ),
                (
                    commands::SET_DEVICE_NICK_NAME,
                    Some(Self::set_device_nick_name_handler),
                    "SetDeviceNickName",
                ),
                (
                    commands::GET_PRODUCT_MODEL,
                    Some(Self::get_product_model_handler),
                    "GetProductModel",
                ),
                (
                    commands::GET_BLUETOOTH_ENABLE_FLAG,
                    Some(Self::get_bluetooth_enable_flag_handler),
                    "GetBluetoothEnableFlag",
                ),
                (
                    commands::SET_BLUETOOTH_ENABLE_FLAG,
                    Some(Self::set_bluetooth_enable_flag_handler),
                    "SetBluetoothEnableFlag",
                ),
                (
                    commands::GET_MII_AUTHOR_ID,
                    Some(Self::get_mii_author_id_handler),
                    "GetMiiAuthorId",
                ),
                (
                    commands::GET_AUTO_UPDATE_ENABLE_FLAG,
                    Some(Self::get_auto_update_enable_flag_handler),
                    "GetAutoUpdateEnableFlag",
                ),
                (
                    commands::SET_AUTO_UPDATE_ENABLE_FLAG,
                    Some(Self::set_auto_update_enable_flag_handler),
                    "SetAutoUpdateEnableFlag",
                ),
                (
                    commands::GET_BATTERY_PERCENTAGE_FLAG,
                    Some(Self::get_battery_percentage_flag_handler),
                    "GetBatteryPercentageFlag",
                ),
                (
                    commands::SET_BATTERY_PERCENTAGE_FLAG,
                    Some(Self::set_battery_percentage_flag_handler),
                    "SetBatteryPercentageFlag",
                ),
                (
                    commands::SET_EXTERNAL_STEADY_CLOCK_INTERNAL_OFFSET,
                    Some(Self::set_external_steady_clock_internal_offset_handler),
                    "SetExternalSteadyClockInternalOffset",
                ),
                (
                    commands::GET_EXTERNAL_STEADY_CLOCK_INTERNAL_OFFSET,
                    Some(Self::get_external_steady_clock_internal_offset_handler),
                    "GetExternalSteadyClockInternalOffset",
                ),
                (
                    commands::GET_HEADPHONE_VOLUME_UPDATE_FLAG,
                    Some(Self::get_headphone_volume_update_flag_handler),
                    "GetHeadphoneVolumeUpdateFlag",
                ),
                (
                    commands::SET_HEADPHONE_VOLUME_UPDATE_FLAG,
                    Some(Self::set_headphone_volume_update_flag_handler),
                    "SetHeadphoneVolumeUpdateFlag",
                ),
                (
                    commands::GET_PUSH_NOTIFICATION_ACTIVITY_MODE_ON_SLEEP,
                    Some(Self::get_push_notification_activity_mode_on_sleep_handler),
                    "GetPushNotificationActivityModeOnSleep",
                ),
                (
                    commands::SET_PUSH_NOTIFICATION_ACTIVITY_MODE_ON_SLEEP,
                    Some(Self::set_push_notification_activity_mode_on_sleep_handler),
                    "SetPushNotificationActivityModeOnSleep",
                ),
                (
                    commands::GET_ERROR_REPORT_SHARE_PERMISSION,
                    Some(Self::get_error_report_share_permission_handler),
                    "GetErrorReportSharePermission",
                ),
                (
                    commands::SET_ERROR_REPORT_SHARE_PERMISSION,
                    Some(Self::set_error_report_share_permission_handler),
                    "SetErrorReportSharePermission",
                ),
                (
                    commands::GET_APPLET_LAUNCH_FLAGS,
                    Some(Self::get_applet_launch_flags_handler),
                    "GetAppletLaunchFlags",
                ),
                (
                    commands::SET_APPLET_LAUNCH_FLAGS,
                    Some(Self::set_applet_launch_flags_handler),
                    "SetAppletLaunchFlags",
                ),
                (
                    commands::GET_KEYBOARD_LAYOUT,
                    Some(Self::get_keyboard_layout_handler),
                    "GetKeyboardLayout",
                ),
                (
                    commands::SET_KEYBOARD_LAYOUT,
                    Some(Self::set_keyboard_layout_handler),
                    "SetKeyboardLayout",
                ),
                (
                    commands::GET_DEVICE_TIME_ZONE_LOCATION_UPDATED_TIME,
                    Some(Self::get_device_time_zone_location_updated_time_handler),
                    "GetDeviceTimeZoneLocationUpdatedTime",
                ),
                (
                    commands::SET_DEVICE_TIME_ZONE_LOCATION_UPDATED_TIME,
                    Some(Self::set_device_time_zone_location_updated_time_handler),
                    "SetDeviceTimeZoneLocationUpdatedTime",
                ),
                (
                    commands::GET_USER_SYSTEM_CLOCK_AUTOMATIC_CORRECTION_UPDATED_TIME,
                    Some(Self::get_user_system_clock_automatic_correction_updated_time_handler),
                    "GetUserSystemClockAutomaticCorrectionUpdatedTime",
                ),
                (
                    commands::SET_USER_SYSTEM_CLOCK_AUTOMATIC_CORRECTION_UPDATED_TIME,
                    Some(Self::set_user_system_clock_automatic_correction_updated_time_handler),
                    "SetUserSystemClockAutomaticCorrectionUpdatedTime",
                ),
                (
                    commands::GET_CHINESE_TRADITIONAL_INPUT_METHOD,
                    Some(Self::get_chinese_traditional_input_method_handler),
                    "GetChineseTraditionalInputMethod",
                ),
                (
                    commands::GET_HOME_MENU_SCHEME,
                    Some(Self::get_home_menu_scheme_handler),
                    "GetHomeMenuScheme",
                ),
                (
                    commands::GET_PLATFORM_REGION,
                    Some(Self::get_platform_region_handler),
                    "GetPlatformRegion",
                ),
                (
                    commands::SET_PLATFORM_REGION,
                    Some(Self::set_platform_region_handler),
                    "SetPlatformRegion",
                ),
                (
                    commands::GET_HOME_MENU_SCHEME_MODEL,
                    Some(Self::get_home_menu_scheme_model_handler),
                    "GetHomeMenuSchemeModel",
                ),
                (
                    commands::GET_TOUCH_SCREEN_MODE,
                    Some(Self::get_touch_screen_mode_handler),
                    "GetTouchScreenMode",
                ),
                (
                    commands::SET_TOUCH_SCREEN_MODE,
                    Some(Self::set_touch_screen_mode_handler),
                    "SetTouchScreenMode",
                ),
                (
                    commands::GET_FIELD_TESTING_FLAG,
                    Some(Self::get_field_testing_flag_handler),
                    "GetFieldTestingFlag",
                ),
                (
                    commands::GET_PANEL_CRC_MODE,
                    Some(Self::get_panel_crc_mode_handler),
                    "GetPanelCrcMode",
                ),
                (
                    commands::SET_PANEL_CRC_MODE,
                    Some(Self::set_panel_crc_mode_handler),
                    "SetPanelCrcMode",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_test() -> Self {
        Self::new_with_inner(ISystemSettingsServer::new_for_test())
    }

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    // --- IPC handlers: each reads params, calls inner, writes response ---

    fn get_firmware_version_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let inner = svc.inner.lock().unwrap();
        let fw = inner.get_firmware_version();
        log::info!(
            "ISystemSettingsServer::GetFirmwareVersion -> {}.{}.{}",
            fw.major,
            fw.minor,
            fw.micro
        );
        // FirmwareVersionFormat is 0x100 bytes, written to output buffer
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &fw as *const FirmwareVersionFormat as *const u8,
                std::mem::size_of::<FirmwareVersionFormat>(),
            )
        };
        ctx.write_buffer(bytes, 0);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_firmware_version2_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        Self::get_firmware_version_handler(this, ctx);
    }

    fn get_external_steady_clock_source_id_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let inner = svc.inner.lock().unwrap();
        let id = inner.get_external_steady_clock_source_id();
        let id_str = format!("{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            id[0],id[1],id[2],id[3], id[4],id[5], id[6],id[7], id[8],id[9], id[10],id[11],id[12],id[13],id[14],id[15]);
        log::info!(
            "ISystemSettingsServer::GetExternalSteadyClockSourceId -> {}",
            id_str
        );
        // UUID is 16 bytes, returned inline in response
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        // Push 16 bytes as 4 u32s
        for i in 0..4 {
            let word = u32::from_le_bytes([id[i * 4], id[i * 4 + 1], id[i * 4 + 2], id[i * 4 + 3]]);
            rb.push_u32(word);
        }
    }

    fn set_external_steady_clock_source_id_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let mut id = [0u8; 16];
        for i in 0..4 {
            let word = rp.pop_u32();
            id[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        let id_str = format!("{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            id[0],id[1],id[2],id[3], id[4],id[5], id[6],id[7], id[8],id[9], id[10],id[11],id[12],id[13],id[14],id[15]);
        log::info!(
            "ISystemSettingsServer::SetExternalSteadyClockSourceId({})",
            id_str
        );
        svc.inner
            .lock()
            .unwrap()
            .set_external_steady_clock_source_id(id);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_user_system_clock_context_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let inner = svc.inner.lock().unwrap();
        let context = inner.get_user_system_clock_context();
        log::info!("ISystemSettingsServer::GetUserSystemClockContext called");
        let mut rb = ResponseBuilder::new(ctx, 2 + 8, 0, 0); // 0x20 bytes = 8 u32s
        rb.push_result(RESULT_SUCCESS);
        for i in 0..8 {
            let word = u32::from_le_bytes([
                context[i * 4],
                context[i * 4 + 1],
                context[i * 4 + 2],
                context[i * 4 + 3],
            ]);
            rb.push_u32(word);
        }
    }

    fn set_user_system_clock_context_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let mut context = [0u8; 0x20];
        for i in 0..8 {
            let word = rp.pop_u32();
            context[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        log::info!("ISystemSettingsServer::SetUserSystemClockContext called");
        svc.inner
            .lock()
            .unwrap()
            .set_user_system_clock_context(context);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_vibration_master_volume_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let vol = svc.inner.lock().unwrap().get_vibration_master_volume();
        log::info!("ISystemSettingsServer::GetVibrationMasterVolume -> {}", vol);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(vol.to_bits());
    }

    fn set_vibration_master_volume_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let vol = rp.pop_f32();
        log::info!("ISystemSettingsServer::SetVibrationMasterVolume({})", vol);
        svc.inner.lock().unwrap().set_vibration_master_volume(vol);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_device_time_zone_location_name_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let name = svc
            .inner
            .lock()
            .unwrap()
            .get_device_time_zone_location_name();
        let name_str = std::str::from_utf8(&name)
            .unwrap_or("")
            .trim_end_matches('\0');
        log::info!(
            "ISystemSettingsServer::GetDeviceTimeZoneLocationName -> \"{}\"",
            name_str
        );
        // LocationName is 0x24 bytes = 9 u32s
        let mut rb = ResponseBuilder::new(ctx, 2 + 9, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        for i in 0..9 {
            let start = i * 4;
            let end = (start + 4).min(0x24);
            let mut word_bytes = [0u8; 4];
            let len = end - start;
            word_bytes[..len].copy_from_slice(&name[start..end]);
            rb.push_u32(u32::from_le_bytes(word_bytes));
        }
    }

    fn set_device_time_zone_location_name_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let mut name = [0u8; 0x24];
        for i in 0..9 {
            let word = rp.pop_u32();
            let start = i * 4;
            let end = (start + 4).min(0x24);
            name[start..end].copy_from_slice(&word.to_le_bytes()[..end - start]);
        }
        let name_str = std::str::from_utf8(&name)
            .unwrap_or("")
            .trim_end_matches('\0');
        log::info!(
            "ISystemSettingsServer::SetDeviceTimeZoneLocationName(\"{}\")",
            name_str
        );
        svc.inner
            .lock()
            .unwrap()
            .set_device_time_zone_location_name(name);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_network_system_clock_context_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let context = svc.inner.lock().unwrap().get_network_system_clock_context();
        log::info!("ISystemSettingsServer::GetNetworkSystemClockContext called");
        let mut rb = ResponseBuilder::new(ctx, 2 + 8, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        for i in 0..8 {
            let word = u32::from_le_bytes([
                context[i * 4],
                context[i * 4 + 1],
                context[i * 4 + 2],
                context[i * 4 + 3],
            ]);
            rb.push_u32(word);
        }
    }

    fn set_network_system_clock_context_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let mut context = [0u8; 0x20];
        for i in 0..8 {
            let word = rp.pop_u32();
            context[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        log::info!("ISystemSettingsServer::SetNetworkSystemClockContext called");
        svc.inner
            .lock()
            .unwrap()
            .set_network_system_clock_context(context);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn is_user_system_clock_automatic_correction_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let enabled = svc
            .inner
            .lock()
            .unwrap()
            .is_user_system_clock_automatic_correction_enabled();
        log::info!(
            "ISystemSettingsServer::IsUserSystemClockAutomaticCorrectionEnabled -> {}",
            enabled
        );
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(enabled as u32);
    }

    fn set_user_system_clock_automatic_correction_enabled_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let enabled = rp.pop_bool();
        log::info!(
            "ISystemSettingsServer::SetUserSystemClockAutomaticCorrectionEnabled({})",
            enabled
        );
        svc.inner
            .lock()
            .unwrap()
            .set_user_system_clock_automatic_correction_enabled(enabled);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_device_time_zone_location_updated_time_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let time = svc
            .inner
            .lock()
            .unwrap()
            .get_device_time_zone_location_updated_time();
        log::info!("ISystemSettingsServer::GetDeviceTimeZoneLocationUpdatedTime called");
        // SteadyClockTimePoint is 0x18 bytes = 6 u32s
        let mut rb = ResponseBuilder::new(ctx, 2 + 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        for i in 0..6 {
            let word = u32::from_le_bytes([
                time[i * 4],
                time[i * 4 + 1],
                time[i * 4 + 2],
                time[i * 4 + 3],
            ]);
            rb.push_u32(word);
        }
    }

    fn set_device_time_zone_location_updated_time_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let mut time = [0u8; 0x18];
        for i in 0..6 {
            let word = rp.pop_u32();
            time[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        log::info!("ISystemSettingsServer::SetDeviceTimeZoneLocationUpdatedTime called");
        svc.inner
            .lock()
            .unwrap()
            .set_device_time_zone_location_updated_time(time);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_user_system_clock_automatic_correction_updated_time_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let time = svc
            .inner
            .lock()
            .unwrap()
            .get_user_system_clock_automatic_correction_updated_time();
        log::info!(
            "ISystemSettingsServer::GetUserSystemClockAutomaticCorrectionUpdatedTime called"
        );
        let mut rb = ResponseBuilder::new(ctx, 2 + 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        for i in 0..6 {
            let word = u32::from_le_bytes([
                time[i * 4],
                time[i * 4 + 1],
                time[i * 4 + 2],
                time[i * 4 + 3],
            ]);
            rb.push_u32(word);
        }
    }

    fn set_user_system_clock_automatic_correction_updated_time_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let mut time = [0u8; 0x18];
        for i in 0..6 {
            let word = rp.pop_u32();
            time[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        log::info!(
            "ISystemSettingsServer::SetUserSystemClockAutomaticCorrectionUpdatedTime called"
        );
        svc.inner
            .lock()
            .unwrap()
            .set_user_system_clock_automatic_correction_updated_time(time);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_color_set_id_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let id = svc.inner.lock().unwrap().get_color_set_id();
        log::debug!("ISystemSettingsServer::GetColorSetId -> {:?}", id);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(id);
    }

    fn set_color_set_id_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let id = rp.pop_u32();
        log::debug!("ISystemSettingsServer::SetColorSetId({})", id);
        svc.inner.lock().unwrap().set_color_set_id(id);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_touch_screen_mode_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mode = svc.inner.lock().unwrap().get_touch_screen_mode();
        log::info!("ISystemSettingsServer::GetTouchScreenMode -> {:?}", mode);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(mode);
    }

    fn set_touch_screen_mode_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let mode = rp.pop_u32();
        log::debug!("ISystemSettingsServer::SetTouchScreenMode({})", mode);
        svc.inner.lock().unwrap().set_touch_screen_mode(mode);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_debug_mode_flag_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("ISystemSettingsServer::GetDebugModeFlag -> false");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(0); // false
    }

    fn get_product_model_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("ISystemSettingsServer::GetProductModel -> 1 (NX)");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(1); // ProductModel::NX
    }

    fn set_external_steady_clock_internal_offset_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let offset = rp.pop_i64();
        log::info!(
            "ISystemSettingsServer::SetExternalSteadyClockInternalOffset({})",
            offset
        );
        svc.inner
            .lock()
            .unwrap()
            .set_external_steady_clock_internal_offset(offset);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_external_steady_clock_internal_offset_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let offset = svc
            .inner
            .lock()
            .unwrap()
            .get_external_steady_clock_internal_offset();
        log::info!(
            "ISystemSettingsServer::GetExternalSteadyClockInternalOffset -> {}",
            offset
        );
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(offset as u64);
    }

    // --- New handlers matching upstream commands ---

    fn set_language_code_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let code = rp.pop_u64();
        log::info!("ISystemSettingsServer::SetLanguageCode(0x{:x})", code);
        svc.inner.lock().unwrap().set_language_code(code);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_lock_screen_flag_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let flag = svc.inner.lock().unwrap().get_lock_screen_flag();
        log::info!("ISystemSettingsServer::GetLockScreenFlag -> {}", flag);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(flag as u32);
    }

    fn set_lock_screen_flag_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let flag = rp.pop_bool();
        log::info!("ISystemSettingsServer::SetLockScreenFlag({})", flag);
        svc.inner.lock().unwrap().set_lock_screen_flag(flag);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_account_settings_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let settings = svc.inner.lock().unwrap().get_account_settings();
        log::info!(
            "ISystemSettingsServer::GetAccountSettings -> flags={}",
            settings.flags
        );
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(settings.flags);
    }

    fn set_account_settings_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let flags = rp.pop_u32();
        log::info!("ISystemSettingsServer::SetAccountSettings(flags={})", flags);
        svc.inner
            .lock()
            .unwrap()
            .set_account_settings(AccountSettings { flags });
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_eula_versions_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let inner = svc.inner.lock().unwrap();
        let (count, versions) = inner.get_eula_versions();
        log::info!("ISystemSettingsServer::GetEulaVersions -> count={}", count);
        // Write versions to output buffer as raw bytes
        if !versions.is_empty() {
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    versions.as_ptr() as *const u8,
                    versions.len() * std::mem::size_of::<EulaVersion>(),
                )
            };
            ctx.write_buffer(bytes, 0);
        }
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(count as u32);
    }

    fn set_eula_versions_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let buf = ctx.read_buffer(0);
        let eula_size = std::mem::size_of::<EulaVersion>();
        let count = buf.len() / eula_size;
        let mut versions = Vec::with_capacity(count);
        for i in 0..count {
            let mut ver = EulaVersion::default();
            let src = &buf[i * eula_size..(i + 1) * eula_size];
            unsafe {
                std::ptr::copy_nonoverlapping(
                    src.as_ptr(),
                    &mut ver as *mut EulaVersion as *mut u8,
                    eula_size,
                );
            }
            versions.push(ver);
        }
        log::info!("ISystemSettingsServer::SetEulaVersions(count={})", count);
        svc.inner.lock().unwrap().set_eula_versions(versions);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_notification_settings_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let settings = svc.inner.lock().unwrap().get_notification_settings();
        log::info!("ISystemSettingsServer::GetNotificationSettings called");
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &settings as *const NotificationSettings as *const u8,
                std::mem::size_of::<NotificationSettings>(),
            )
        };
        // NotificationSettings is 0x18 bytes = 6 u32s
        let mut rb = ResponseBuilder::new(ctx, 2 + 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw_bytes(bytes);
    }

    fn set_notification_settings_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let settings: NotificationSettings = rp.pop_raw();
        log::info!("ISystemSettingsServer::SetNotificationSettings called");
        svc.inner
            .lock()
            .unwrap()
            .set_notification_settings(settings);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_account_notification_settings_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let inner = svc.inner.lock().unwrap();
        let (count, settings) = inner.get_account_notification_settings();
        log::info!(
            "ISystemSettingsServer::GetAccountNotificationSettings -> count={}",
            count
        );
        if !settings.is_empty() {
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    settings.as_ptr() as *const u8,
                    settings.len() * std::mem::size_of::<AccountNotificationSettings>(),
                )
            };
            ctx.write_buffer(bytes, 0);
        }
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(count as u32);
    }

    fn set_account_notification_settings_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let buf = ctx.read_buffer(0);
        let item_size = std::mem::size_of::<AccountNotificationSettings>();
        let count = buf.len() / item_size;
        let mut settings = Vec::with_capacity(count);
        for i in 0..count {
            let mut item = AccountNotificationSettings::default();
            let src = &buf[i * item_size..(i + 1) * item_size];
            unsafe {
                std::ptr::copy_nonoverlapping(
                    src.as_ptr(),
                    &mut item as *mut AccountNotificationSettings as *mut u8,
                    item_size,
                );
            }
            settings.push(item);
        }
        log::info!(
            "ISystemSettingsServer::SetAccountNotificationSettings(count={})",
            count
        );
        svc.inner
            .lock()
            .unwrap()
            .set_account_notification_settings(settings);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_settings_item_value_size_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        // Upstream reads category and name from HipcPointer buffers (X descriptors)
        let category_buf = ctx.read_buffer(0);
        let name_buf = ctx.read_buffer(1);
        let category = std::str::from_utf8(&category_buf)
            .unwrap_or("")
            .trim_end_matches('\0')
            .to_string();
        let name = std::str::from_utf8(&name_buf)
            .unwrap_or("")
            .trim_end_matches('\0')
            .to_string();
        log::debug!(
            "ISystemSettingsServer::GetSettingsItemValueSize(category={}, name={})",
            category,
            name
        );

        let settings = get_settings();
        let size = settings
            .get(&category)
            .and_then(|cat| cat.get(&name))
            .map(|v| v.len() as u64)
            .unwrap_or(0);

        if size == 0 {
            log::warn!(
                "GetSettingsItemValueSize: unknown setting {}.{}",
                category,
                name
            );
            // Return ResultUnknown (module 105 = SET, error 1)
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(ResultCode::from_module_description(
                ErrorModule::Settings,
                1,
            ));
            return;
        }

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(size);
    }

    fn get_settings_item_value_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        // Upstream reads category/name from HipcPointer buffers, writes value to HipcMapAlias
        let category_buf = ctx.read_buffer(0);
        let name_buf = ctx.read_buffer(1);
        let category = std::str::from_utf8(&category_buf)
            .unwrap_or("")
            .trim_end_matches('\0')
            .to_string();
        let name = std::str::from_utf8(&name_buf)
            .unwrap_or("")
            .trim_end_matches('\0')
            .to_string();
        log::info!(
            "ISystemSettingsServer::GetSettingsItemValue(category={}, name={})",
            category,
            name
        );

        let settings = get_settings();
        if let Some(value) = settings.get(&category).and_then(|cat| cat.get(&name)) {
            let written = ctx.write_buffer(value, 0);
            let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
            rb.push_result(RESULT_SUCCESS);
            rb.push_u64(written as u64);
        } else {
            log::warn!(
                "GetSettingsItemValue: unknown setting {}.{}",
                category,
                name
            );
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(ResultCode::from_module_description(
                ErrorModule::Settings,
                1,
            ));
        }
    }

    fn get_tv_settings_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let settings = svc.inner.lock().unwrap().get_tv_settings();
        log::info!("ISystemSettingsServer::GetTvSettings called");
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &settings as *const TvSettings as *const u8,
                std::mem::size_of::<TvSettings>(),
            )
        };
        // TvSettings is 0x20 bytes = 8 u32s
        let mut rb = ResponseBuilder::new(ctx, 2 + 8, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw_bytes(bytes);
    }

    fn set_tv_settings_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let settings: TvSettings = rp.pop_raw();
        log::info!("ISystemSettingsServer::SetTvSettings called");
        svc.inner.lock().unwrap().set_tv_settings(settings);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_audio_output_mode_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let target_val = rp.pop_u32();
        let mode = svc.inner.lock().unwrap().get_audio_output_mode(target_val);
        log::info!(
            "ISystemSettingsServer::GetAudioOutputMode(target={}) -> {}",
            target_val,
            mode
        );
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(mode);
    }

    fn set_audio_output_mode_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let target_val = rp.pop_u32();
        let mode_val = rp.pop_u32();
        log::info!(
            "ISystemSettingsServer::SetAudioOutputMode(target={}, mode={})",
            target_val,
            mode_val
        );
        svc.inner
            .lock()
            .unwrap()
            .set_audio_output_mode(target_val, mode_val);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_speaker_auto_mute_flag_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let flag = svc.inner.lock().unwrap().get_speaker_auto_mute_flag();
        log::info!("ISystemSettingsServer::GetSpeakerAutoMuteFlag -> {}", flag);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(flag as u32);
    }

    fn set_speaker_auto_mute_flag_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let flag = rp.pop_bool();
        log::info!("ISystemSettingsServer::SetSpeakerAutoMuteFlag({})", flag);
        svc.inner.lock().unwrap().set_speaker_auto_mute_flag(flag);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_quest_flag_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let flag = svc.inner.lock().unwrap().get_quest_flag();
        log::info!("ISystemSettingsServer::GetQuestFlag -> {:?}", flag);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(flag as u32);
    }

    fn set_quest_flag_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let flag = rp.pop_u32();
        log::info!("ISystemSettingsServer::SetQuestFlag({})", flag);
        svc.inner.lock().unwrap().set_quest_flag(flag as u8);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_region_code_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let region = rp.pop_u32();
        log::info!("ISystemSettingsServer::SetRegionCode({})", region);
        svc.inner.lock().unwrap().set_region_code(region);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_primary_album_storage_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let storage = svc.inner.lock().unwrap().get_primary_album_storage();
        log::info!(
            "ISystemSettingsServer::GetPrimaryAlbumStorage -> {:?}",
            storage
        );
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(storage);
    }

    fn set_primary_album_storage_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let storage = rp.pop_u32();
        log::info!("ISystemSettingsServer::SetPrimaryAlbumStorage({})", storage);
        svc.inner.lock().unwrap().set_primary_album_storage(storage);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_battery_lot_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let lot = svc.inner.lock().unwrap().get_battery_lot();
        log::info!("ISystemSettingsServer::GetBatteryLot called");
        // BatteryLot is 0x18 bytes = 6 u32s
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &lot as *const BatteryLot as *const u8,
                std::mem::size_of::<BatteryLot>(),
            )
        };
        let mut rb = ResponseBuilder::new(ctx, 2 + 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw_bytes(bytes);
    }

    fn get_serial_number_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let serial = svc.inner.lock().unwrap().get_serial_number();
        log::info!("ISystemSettingsServer::GetSerialNumber called");
        // SerialNumber is 0x18 bytes = 6 u32s
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &serial as *const SerialNumber as *const u8,
                std::mem::size_of::<SerialNumber>(),
            )
        };
        let mut rb = ResponseBuilder::new(ctx, 2 + 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw_bytes(bytes);
    }

    fn get_nfc_enable_flag_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let flag = svc.inner.lock().unwrap().get_nfc_enable_flag();
        log::info!("ISystemSettingsServer::GetNfcEnableFlag -> {}", flag);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(flag as u32);
    }

    fn set_nfc_enable_flag_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let flag = rp.pop_bool();
        log::info!("ISystemSettingsServer::SetNfcEnableFlag({})", flag);
        svc.inner.lock().unwrap().set_nfc_enable_flag(flag);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_sleep_settings_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let settings = svc.inner.lock().unwrap().get_sleep_settings();
        log::info!("ISystemSettingsServer::GetSleepSettings called");
        // SleepSettings is 0xC bytes = 3 u32s
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &settings as *const SleepSettings as *const u8,
                std::mem::size_of::<SleepSettings>(),
            )
        };
        let mut rb = ResponseBuilder::new(ctx, 2 + 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw_bytes(bytes);
    }

    fn set_sleep_settings_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let settings: SleepSettings = rp.pop_raw();
        log::info!("ISystemSettingsServer::SetSleepSettings called");
        svc.inner.lock().unwrap().set_sleep_settings(settings);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_wireless_lan_enable_flag_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let flag = svc.inner.lock().unwrap().get_wireless_lan_enable_flag();
        log::info!(
            "ISystemSettingsServer::GetWirelessLanEnableFlag -> {}",
            flag
        );
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(flag as u32);
    }

    fn set_wireless_lan_enable_flag_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let flag = rp.pop_bool();
        log::info!("ISystemSettingsServer::SetWirelessLanEnableFlag({})", flag);
        svc.inner.lock().unwrap().set_wireless_lan_enable_flag(flag);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_initial_launch_settings_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let settings = svc.inner.lock().unwrap().get_initial_launch_settings();
        log::info!("ISystemSettingsServer::GetInitialLaunchSettings called");
        // InitialLaunchSettings is 0x20 bytes = 8 u32s
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &settings as *const InitialLaunchSettings as *const u8,
                std::mem::size_of::<InitialLaunchSettings>(),
            )
        };
        let mut rb = ResponseBuilder::new(ctx, 2 + 8, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw_bytes(bytes);
    }

    fn set_initial_launch_settings_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let settings: InitialLaunchSettings = rp.pop_raw();
        log::info!("ISystemSettingsServer::SetInitialLaunchSettings called");
        svc.inner
            .lock()
            .unwrap()
            .set_initial_launch_settings(settings);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_device_nick_name_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let name = svc.inner.lock().unwrap().get_device_nick_name();
        let name_str = std::str::from_utf8(&name)
            .unwrap_or("")
            .trim_end_matches('\0');
        log::info!(
            "ISystemSettingsServer::GetDeviceNickName -> \"{}\"",
            name_str
        );
        // DeviceNickName is 0x80 bytes, written to output buffer (HipcMapAlias)
        ctx.write_buffer(&name, 0);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn set_device_nick_name_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let buf = ctx.read_buffer(0);
        let mut name = [0u8; 0x80];
        let len = buf.len().min(0x80);
        name[..len].copy_from_slice(&buf[..len]);
        let name_str = std::str::from_utf8(&name)
            .unwrap_or("")
            .trim_end_matches('\0');
        log::info!("ISystemSettingsServer::SetDeviceNickName(\"{}\")", name_str);
        svc.inner.lock().unwrap().set_device_nick_name(name);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_bluetooth_enable_flag_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let flag = svc.inner.lock().unwrap().get_bluetooth_enable_flag();
        log::info!("ISystemSettingsServer::GetBluetoothEnableFlag -> {}", flag);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(flag as u32);
    }

    fn set_bluetooth_enable_flag_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let flag = rp.pop_bool();
        log::info!("ISystemSettingsServer::SetBluetoothEnableFlag({})", flag);
        svc.inner.lock().unwrap().set_bluetooth_enable_flag(flag);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_mii_author_id_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let id = svc.inner.lock().unwrap().get_mii_author_id();
        let id_str = format!("{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            id[0],id[1],id[2],id[3], id[4],id[5], id[6],id[7], id[8],id[9], id[10],id[11],id[12],id[13],id[14],id[15]);
        log::info!("ISystemSettingsServer::GetMiiAuthorId -> {}", id_str);
        // UUID is 16 bytes = 4 u32s
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        for i in 0..4 {
            let word = u32::from_le_bytes([id[i * 4], id[i * 4 + 1], id[i * 4 + 2], id[i * 4 + 3]]);
            rb.push_u32(word);
        }
    }

    fn get_auto_update_enable_flag_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let flag = svc.inner.lock().unwrap().get_auto_update_enable_flag();
        log::info!("ISystemSettingsServer::GetAutoUpdateEnableFlag -> {}", flag);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(flag as u32);
    }

    fn set_auto_update_enable_flag_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let flag = rp.pop_bool();
        log::info!("ISystemSettingsServer::SetAutoUpdateEnableFlag({})", flag);
        svc.inner.lock().unwrap().set_auto_update_enable_flag(flag);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_battery_percentage_flag_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let flag = svc.inner.lock().unwrap().get_battery_percentage_flag();
        log::info!(
            "ISystemSettingsServer::GetBatteryPercentageFlag -> {}",
            flag
        );
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(flag as u32);
    }

    fn set_battery_percentage_flag_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let flag = rp.pop_bool();
        log::info!("ISystemSettingsServer::SetBatteryPercentageFlag({})", flag);
        svc.inner.lock().unwrap().set_battery_percentage_flag(flag);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_headphone_volume_update_flag_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let flag = svc.inner.lock().unwrap().get_headphone_volume_update_flag();
        log::info!(
            "ISystemSettingsServer::GetHeadphoneVolumeUpdateFlag -> {}",
            flag
        );
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(flag as u32);
    }

    fn set_headphone_volume_update_flag_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let flag = rp.pop_bool();
        log::info!(
            "ISystemSettingsServer::SetHeadphoneVolumeUpdateFlag({})",
            flag
        );
        svc.inner
            .lock()
            .unwrap()
            .set_headphone_volume_update_flag(flag);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_push_notification_activity_mode_on_sleep_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mode = svc
            .inner
            .lock()
            .unwrap()
            .get_push_notification_activity_mode_on_sleep();
        log::info!(
            "ISystemSettingsServer::GetPushNotificationActivityModeOnSleep -> {}",
            mode
        );
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(mode);
    }

    fn set_push_notification_activity_mode_on_sleep_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let mode = rp.pop_i32();
        log::info!(
            "ISystemSettingsServer::SetPushNotificationActivityModeOnSleep({})",
            mode
        );
        svc.inner
            .lock()
            .unwrap()
            .set_push_notification_activity_mode_on_sleep(mode);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_error_report_share_permission_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let perm = svc
            .inner
            .lock()
            .unwrap()
            .get_error_report_share_permission();
        log::info!(
            "ISystemSettingsServer::GetErrorReportSharePermission -> {:?}",
            perm
        );
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(perm);
    }

    fn set_error_report_share_permission_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let perm = rp.pop_u32();
        log::info!(
            "ISystemSettingsServer::SetErrorReportSharePermission({})",
            perm
        );
        svc.inner
            .lock()
            .unwrap()
            .set_error_report_share_permission(perm);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_applet_launch_flags_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let flags = svc.inner.lock().unwrap().get_applet_launch_flags();
        log::info!("ISystemSettingsServer::GetAppletLaunchFlags -> {}", flags);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(flags);
    }

    fn set_applet_launch_flags_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let flags = rp.pop_u32();
        log::info!("ISystemSettingsServer::SetAppletLaunchFlags({})", flags);
        svc.inner.lock().unwrap().set_applet_launch_flags(flags);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_keyboard_layout_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let layout = svc.inner.lock().unwrap().get_keyboard_layout();
        log::info!("ISystemSettingsServer::GetKeyboardLayout -> {:?}", layout);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(layout);
    }

    fn set_keyboard_layout_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let layout = rp.pop_u32();
        log::info!("ISystemSettingsServer::SetKeyboardLayout({})", layout);
        svc.inner.lock().unwrap().set_keyboard_layout(layout);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_chinese_traditional_input_method_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let method = svc
            .inner
            .lock()
            .unwrap()
            .get_chinese_traditional_input_method();
        log::info!(
            "ISystemSettingsServer::GetChineseTraditionalInputMethod -> {:?}",
            method
        );
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(method);
    }

    fn get_home_menu_scheme_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let scheme = svc.inner.lock().unwrap().get_home_menu_scheme();
        log::info!("ISystemSettingsServer::GetHomeMenuScheme called");
        // HomeMenuScheme is 0x14 bytes = 5 u32s
        let mut rb = ResponseBuilder::new(ctx, 2 + 5, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(scheme.main);
        rb.push_u32(scheme.back);
        rb.push_u32(scheme.sub);
        rb.push_u32(scheme.bezel);
        rb.push_u32(scheme.extra);
    }

    fn get_platform_region_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let region = svc.inner.lock().unwrap().get_platform_region();
        log::info!("ISystemSettingsServer::GetPlatformRegion -> {:?}", region);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(region as u32);
    }

    fn set_platform_region_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let region = rp.pop_u32();
        log::info!("ISystemSettingsServer::SetPlatformRegion({})", region);
        svc.inner.lock().unwrap().set_platform_region(region as i32);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_home_menu_scheme_model_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let svc = Self::as_self(this);
        let model = svc.inner.lock().unwrap().get_home_menu_scheme_model();
        log::info!("ISystemSettingsServer::GetHomeMenuSchemeModel -> {}", model);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(model);
    }

    fn get_field_testing_flag_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let flag = svc.inner.lock().unwrap().get_field_testing_flag();
        log::info!("ISystemSettingsServer::GetFieldTestingFlag -> {}", flag);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(flag as u32);
    }

    fn get_panel_crc_mode_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mode = svc.inner.lock().unwrap().get_panel_crc_mode();
        log::info!("ISystemSettingsServer::GetPanelCrcMode -> {}", mode);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(mode);
    }

    fn set_panel_crc_mode_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let svc = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let mode = rp.pop_i32();
        log::info!("ISystemSettingsServer::SetPanelCrcMode({})", mode);
        svc.inner.lock().unwrap().set_panel_crc_mode(mode);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }
}

impl SessionRequestHandler for SystemSettingsService {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str {
        ServiceFramework::get_service_name(self)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl ServiceFramework for SystemSettingsService {
    fn get_service_name(&self) -> &str {
        "set:sys"
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn settings_test_dir(name: &str) -> std::path::PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "ruzu-set-sys-{}-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed),
            name
        ))
    }

    #[test]
    fn test_system_settings_default() {
        let server = ISystemSettingsServer::new_for_test();
        assert_eq!(server.get_color_set_id(), ColorSet::BasicWhite as u32);
        assert_eq!(server.get_vibration_master_volume(), 1.0);
        assert_eq!(server.get_bluetooth_enable_flag(), true);
        assert_eq!(server.get_nfc_enable_flag(), true);
        assert_eq!(server.get_wireless_lan_enable_flag(), true);
        assert_eq!(server.get_product_model(), 1);
        assert_eq!(
            server.get_touch_screen_mode(),
            TouchScreenMode::Standard as u32
        );
    }

    #[test]
    fn test_system_settings_set_get_color() {
        let mut server = ISystemSettingsServer::new_for_test();
        server.set_color_set_id(ColorSet::BasicBlack as u32);
        assert_eq!(server.get_color_set_id(), ColorSet::BasicBlack as u32);
    }

    #[test]
    fn direct_timezone_setters_preserve_typed_payloads() {
        let service = SystemSettingsService::new_for_test();
        let mut name = [0u8; 0x24];
        name[..7].copy_from_slice(b"Etc/GMT");
        let time_point = SteadyClockTimePoint {
            time_point: -0x1234_5678,
            clock_source_id: *b"homebrew-clock!!",
        };

        service.set_device_time_zone_location_name(name);
        service.set_device_time_zone_location_updated_time(time_point);

        assert_eq!(service.get_device_time_zone_location_name(), name);
        assert_eq!(
            service.get_device_time_zone_location_updated_time(),
            time_point
        );
    }

    #[test]
    fn persisted_enum_values_keep_unknown_bit_patterns() {
        let mut server = ISystemSettingsServer::new_for_test();
        let unknown = 0xFFFF_FFFE;

        server.set_color_set_id(unknown);
        server.set_keyboard_layout(unknown);
        server.set_touch_screen_mode(unknown);

        assert_eq!(server.get_color_set_id(), unknown);
        assert_eq!(server.get_keyboard_layout(), unknown);
        assert_eq!(server.get_touch_screen_mode(), unknown);
    }

    #[test]
    fn server_owns_each_upstream_settings_payload() {
        let server = ISystemSettingsServer::new_for_test();

        assert_eq!(server.system_settings.eula_version_count, 1);
        assert_eq!(server.private_settings.external_clock_source_id, [0; 16]);
        assert_eq!(server.device_settings.ptm_battery_version, 0);
        assert_eq!(
            server.appln_settings.mii_author_id,
            UUID::make_default().uuid
        );
    }

    #[test]
    fn test_system_settings_vibration_volume() {
        let mut server = ISystemSettingsServer::new_for_test();
        server.set_vibration_master_volume(0.5);
        assert!((server.get_vibration_master_volume() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_firmware_version() {
        let fw = get_firmware_version_impl(GetFirmwareVersionType::Version1);
        assert_eq!(fw.major, 18);
        assert_eq!(fw.minor, 0);
    }

    #[test]
    fn get_mii_author_id_initializes_invalid_uuid() {
        let mut server = ISystemSettingsServer::new_for_test();
        server.system_settings.mii_author_id = [0; 16];
        assert_eq!(server.system_settings.mii_author_id, [0; 16]);

        assert_eq!(server.get_mii_author_id(), *b"Eden Default UID");
    }

    #[test]
    fn get_mii_author_id_preserves_valid_uuid() {
        let mut server = ISystemSettingsServer::new_for_test();
        server.system_settings.mii_author_id = *b"custom author id";

        assert_eq!(server.get_mii_author_id(), *b"custom author id");
    }

    #[test]
    fn missing_settings_file_is_created_with_upstream_header_and_default() {
        let path = settings_test_dir("missing");
        let mut loaded = ApplnSettings::default();

        assert!(ISystemSettingsServer::load_settings_file(
            &path,
            &mut loaded,
            || {
                let mut settings = default_appln_settings();
                settings.service_discovery_control_settings = 0x1234_5678;
                settings
            },
        ));
        assert_eq!(loaded.service_discovery_control_settings, 0x1234_5678);

        let bytes = fs::read(path.join("settings.dat")).unwrap();
        assert_eq!(
            bytes.len(),
            core::mem::size_of::<SettingsHeader>() + core::mem::size_of::<ApplnSettings>()
        );
        assert_eq!(
            u64::from_ne_bytes(bytes[0..8].try_into().unwrap()),
            SETTINGS_MAGIC
        );
        assert_eq!(
            u32::from_ne_bytes(bytes[8..12].try_into().unwrap()),
            SETTINGS_VERSION
        );
        assert_eq!(u32::from_ne_bytes(bytes[12..16].try_into().unwrap()), 0);

        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn invalid_header_is_reset_before_loading_payload() {
        let path = settings_test_dir("invalid-header");
        let mut loaded = default_private_settings();
        assert!(ISystemSettingsServer::load_settings_file(
            &path,
            &mut loaded,
            default_private_settings,
        ));

        let settings_file = path.join("settings.dat");
        let mut bytes = fs::read(&settings_file).unwrap();
        bytes[0..8].fill(0);
        fs::write(&settings_file, bytes).unwrap();
        loaded.external_steady_clock_internal_offset = 99;

        assert!(ISystemSettingsServer::load_settings_file(
            &path,
            &mut loaded,
            default_private_settings,
        ));
        assert_eq!(loaded.external_steady_clock_internal_offset, 0);
        let repaired = fs::read(&settings_file).unwrap();
        assert_eq!(
            u64::from_ne_bytes(repaired[0..8].try_into().unwrap()),
            SETTINGS_MAGIC
        );

        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn store_uses_temporary_file_then_replaces_settings_dat() {
        let path = settings_test_dir("store");
        assert!(common::fs::fs::create_dirs(&path));
        let mut settings = default_appln_settings();
        settings.service_discovery_control_settings = 0xDEAD_BEEF;

        assert!(ISystemSettingsServer::store_settings_file(&path, &settings));
        settings.service_discovery_control_settings = 0xA5A5_5A5A;
        assert!(ISystemSettingsServer::store_settings_file(&path, &settings));
        assert!(!path.join("settings.tmp").exists());

        let mut loaded = default_appln_settings();
        assert!(ISystemSettingsServer::load_settings_file(
            &path,
            &mut loaded,
            default_appln_settings,
        ));
        assert_eq!(loaded.service_discovery_control_settings, 0xA5A5_5A5A);

        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn newer_settings_header_version_is_accepted() {
        let path = settings_test_dir("newer-version");
        assert!(common::fs::fs::create_dirs(&path));
        let mut settings = default_appln_settings();
        settings.service_discovery_control_settings = 0xCAFEBABE;
        assert!(ISystemSettingsServer::store_settings_file(&path, &settings));

        let settings_file = path.join("settings.dat");
        let mut bytes = fs::read(&settings_file).unwrap();
        bytes[8..12].copy_from_slice(&(SETTINGS_VERSION + 1).to_ne_bytes());
        fs::write(&settings_file, bytes).unwrap();

        let mut loaded = default_appln_settings();
        assert!(ISystemSettingsServer::load_settings_file(
            &path,
            &mut loaded,
            default_appln_settings,
        ));
        assert_eq!(loaded.service_discovery_control_settings, 0xCAFEBABE);

        fs::remove_dir_all(path).unwrap();
    }
}
