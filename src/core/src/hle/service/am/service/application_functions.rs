// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/service/application_functions.h
//! Port of zuyu/src/core/hle/service/am/service/application_functions.cpp

use crate::hle::service::am::am_types::{
    GamePlayRecordingState, LaunchParameterKind, ProgramSpecifyKind,
};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::file_sys::fs_save_data_types::{
    SaveDataAttribute, SaveDataSize, SaveDataSpaceId, SaveDataType,
};
use crate::file_sys::patch_manager::PatchManager;
use crate::file_sys::registered_cache::get_update_title_id;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::am::am_results;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::ns::read_only_application_control_data_interface::IReadOnlyApplicationControlDataInterface;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

fn copy_display_version(version: Option<&str>) -> [u8; 16] {
    let mut display_version = [0u8; 16];

    if let Some(version) = version {
        let version = version.as_bytes();
        let copy_size = version.len().min(display_version.len());
        display_version[..copy_size].copy_from_slice(&version[..copy_size]);
    } else {
        const DEFAULT_VERSION: &[u8] = b"1.0.0\0";
        display_version[..DEFAULT_VERSION.len()].copy_from_slice(DEFAULT_VERSION);
    }

    display_version[15] = 0;
    display_version
}

/// IPC command table for IApplicationFunctions:
/// - 1: PopLaunchParameter
/// - 10: CreateApplicationAndPushAndRequestToStart (unimplemented)
/// - 11: CreateApplicationAndPushAndRequestToStartForQuest (unimplemented)
/// - 12: CreateApplicationAndRequestToStart (unimplemented)
/// - 13: CreateApplicationAndRequestToStartForQuest (unimplemented)
/// - 14: CreateApplicationWithAttributeAndPushAndRequestToStartForQuest (unimplemented)
/// - 15: CreateApplicationWithAttributeAndRequestToStartForQuest (unimplemented)
/// - 20: EnsureSaveData
/// - 21: GetDesiredLanguage
/// - 22: SetTerminateResult
/// - 23: GetDisplayVersion
/// - 24: GetLaunchStorageInfoForDebug (unimplemented)
/// - 25: ExtendSaveData
/// - 26: GetSaveDataSize
/// - 27: CreateCacheStorage
/// - 28: GetSaveDataSizeMax
/// - 29: GetCacheStorageMax
/// - 30: BeginBlockingHomeButtonShortAndLongPressed
/// - 31: EndBlockingHomeButtonShortAndLongPressed
/// - 32: BeginBlockingHomeButton
/// - 33: EndBlockingHomeButton
/// - 34: SelectApplicationLicense (unimplemented)
/// - 35: GetDeviceSaveDataSizeMax (unimplemented)
/// - 36: GetLimitedApplicationLicense (unimplemented)
/// - 37: GetLimitedApplicationLicenseUpgradableEvent (unimplemented)
/// - 40: NotifyRunning
/// - 50: GetPseudoDeviceId
/// - 60: SetMediaPlaybackStateForApplication (unimplemented)
/// - 65: IsGamePlayRecordingSupported
/// - 66: InitializeGamePlayRecording
/// - 67: SetGamePlayRecordingState
/// - 68: RequestFlushGamePlayingMovieForDebug (unimplemented)
/// - 70: RequestToShutdown (unimplemented)
/// - 71: RequestToReboot (unimplemented)
/// - 72: RequestToSleep (unimplemented)
/// - 80: ExitAndRequestToShowThanksMessage (unimplemented)
/// - 90: EnableApplicationCrashReport
/// - 100: InitializeApplicationCopyrightFrameBuffer
/// - 101: SetApplicationCopyrightImage
/// - 102: SetApplicationCopyrightVisibility
/// - 110: QueryApplicationPlayStatistics
/// - 111: QueryApplicationPlayStatisticsByUid
/// - 120: ExecuteProgram (unimplemented)
/// - 121: ClearUserChannel (unimplemented)
/// - 122: UnpopToUserChannel (unimplemented)
/// - 123: GetPreviousProgramIndex
/// - 124: EnableApplicationAllThreadDumpOnCrash (unimplemented)
/// - 130: GetGpuErrorDetectedSystemEvent
/// - 131: SetDelayTimeToAbortOnGpuError (unimplemented)
/// - 140: GetFriendInvitationStorageChannelEvent
/// - 141: TryPopFromFriendInvitationStorageChannel
/// - 150: GetNotificationStorageChannelEvent (unimplemented)
/// - 151: TryPopFromNotificationStorageChannel (unimplemented)
/// - 160: GetHealthWarningDisappearedSystemEvent
/// - 170: SetHdcpAuthenticationActivated (unimplemented)
/// - 180: GetLaunchRequiredVersion (unimplemented)
/// - 181: UpgradeLaunchRequiredVersion (unimplemented)
/// - 190: SendServerMaintenanceOverlayNotification (unimplemented)
/// - 200: GetLastApplicationExitReason (unimplemented)
/// - 210: Unknown210
/// - 330: Unknown330
/// - 500: StartContinuousRecordingFlushForDebug (unimplemented)
/// - 1000: CreateMovieMaker (unimplemented)
/// - 1001: PrepareForJit
pub struct IApplicationFunctions {
    /// Reference to the applet.
    /// Matches upstream `const std::shared_ptr<Applet> m_applet`.
    applet: std::sync::Arc<std::sync::Mutex<crate::hle::service::am::applet::Applet>>,
    /// Reference to the System, matching upstream `Core::System& m_system`.
    system: crate::core::SystemRef,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IApplicationFunctions {
    pub fn new(
        system: crate::core::SystemRef,
        applet: std::sync::Arc<std::sync::Mutex<crate::hle::service::am::applet::Applet>>,
    ) -> Self {
        let handlers = build_handler_map(&[
            (12, Some(Self::create_application_and_request_to_start_handler), "CreateApplicationAndRequestToStart"),
            (120, Some(Self::execute_program_handler), "ExecuteProgram"),
            (29, Some(Self::get_cache_storage_max_handler), "GetCacheStorageMax"),
            (30, Some(Self::begin_blocking_home_button_short_and_long_pressed_handler), "BeginBlockingHomeButtonShortAndLongPressed"),
            (31, Some(Self::end_blocking_home_button_short_and_long_pressed_handler), "EndBlockingHomeButtonShortAndLongPressed"),
            (32, Some(Self::begin_blocking_home_button_handler), "BeginBlockingHomeButton"),
            (33, Some(Self::end_blocking_home_button_handler), "EndBlockingHomeButton"),
            (60, Some(Self::set_media_playback_state_for_application_handler), "SetMediaPlaybackStateForApplication"),
            (101, Some(Self::set_application_copyright_image_handler), "SetApplicationCopyrightImage"),
            (102, Some(Self::set_application_copyright_visibility_handler), "SetApplicationCopyrightVisibility"),
            (121, Some(Self::clear_user_channel_handler), "ClearUserChannel"),
            (122, Some(Self::unpop_to_user_channel_handler), "UnpopToUserChannel"),
            (150, Some(Self::get_notification_storage_channel_event_handler), "GetNotificationStorageChannelEvent"),
            (
                1,
                Some(Self::pop_launch_parameter_handler),
                "PopLaunchParameter",
            ),
            (20, Some(Self::ensure_save_data_handler), "EnsureSaveData"),
            (
                21,
                Some(Self::get_desired_language_handler),
                "GetDesiredLanguage",
            ),
            (
                22,
                Some(Self::set_terminate_result_handler),
                "SetTerminateResult",
            ),
            (
                23,
                Some(Self::get_display_version_handler),
                "GetDisplayVersion",
            ),
            (
                27,
                Some(Self::create_cache_storage_handler),
                "CreateCacheStorage",
            ),
            (25, Some(Self::extend_save_data_handler), "ExtendSaveData"),
            (
                26,
                Some(Self::get_save_data_size_handler),
                "GetSaveDataSize",
            ),
            (
                28,
                Some(Self::get_save_data_size_max_handler),
                "GetSaveDataSizeMax",
            ),
            (40, Some(Self::notify_running_handler), "NotifyRunning"),
            (
                50,
                Some(Self::get_pseudo_device_id_handler),
                "GetPseudoDeviceId",
            ),
            (
                65,
                Some(Self::is_game_play_recording_supported_handler),
                "IsGamePlayRecordingSupported",
            ),
            (
                66,
                Some(Self::initialize_game_play_recording_handler),
                "InitializeGamePlayRecording",
            ),
            (
                67,
                Some(Self::set_game_play_recording_state_handler),
                "SetGamePlayRecordingState",
            ),
            (
                90,
                Some(Self::enable_application_crash_report_handler),
                "EnableApplicationCrashReport",
            ),
            (
                100,
                Some(Self::initialize_application_copyright_frame_buffer_handler),
                "InitializeApplicationCopyrightFrameBuffer",
            ),
            (
                110,
                Some(Self::query_application_play_statistics_handler),
                "QueryApplicationPlayStatistics",
            ),
            (
                111,
                Some(Self::query_application_play_statistics_by_uid_handler),
                "QueryApplicationPlayStatisticsByUid",
            ),
            (
                123,
                Some(Self::get_previous_program_index_handler),
                "GetPreviousProgramIndex",
            ),
            (
                130,
                Some(Self::get_gpu_error_detected_system_event_handler),
                "GetGpuErrorDetectedSystemEvent",
            ),
            (
                140,
                Some(Self::get_friend_invitation_storage_channel_event_handler),
                "GetFriendInvitationStorageChannelEvent",
            ),
            (
                160,
                Some(Self::get_health_warning_disappeared_system_event_handler),
                "GetHealthWarningDisappearedSystemEvent",
            ),
            (
                141,
                Some(Self::try_pop_from_friend_invitation_storage_channel_handler),
                "TryPopFromFriendInvitationStorageChannel",
            ),
            (210, Some(Self::get_unknown_event_210_handler), "Unknown210"),
            (330, Some(Self::unknown_330_handler), "Unknown330"),
            (1001, Some(Self::prepare_for_jit_handler), "PrepareForJit"),
        ]);
        Self {
            applet,
            system,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// Port of IApplicationFunctions::NotifyRunning
    pub fn notify_running(&self) -> bool {
        log::warn!("(STUBBED) NotifyRunning called");
        true
    }

    /// Port of IApplicationFunctions::GetSaveDataSizeMax
    pub fn get_save_data_size_max(&self) -> (u64, u64) {
        log::warn!("(STUBBED) GetSaveDataSizeMax called");
        (0xFFFFFFF, 0xFFFFFFF)
    }

    /// Port of IApplicationFunctions::CreateCacheStorage
    pub fn create_cache_storage(
        &self,
        _index: u16,
        _normal_size: u64,
        _journal_size: u64,
    ) -> (u32, u64) {
        log::warn!("(STUBBED) CreateCacheStorage called");
        (1, 0) // target_media=Nand, required_size=0
    }

    /// Port of IApplicationFunctions::BeginBlockingHomeButtonShortAndLongPressed
    pub fn begin_blocking_home_button_short_and_long_pressed(&self, _unused: i64) {
        log::debug!("BeginBlockingHomeButtonShortAndLongPressed called");
        let mut applet = self.applet.lock().unwrap();
        applet.home_button_long_pressed_blocked = true;
        applet.home_button_short_pressed_blocked = true;
    }

    /// Port of IApplicationFunctions::EndBlockingHomeButtonShortAndLongPressed
    pub fn end_blocking_home_button_short_and_long_pressed(&self) {
        log::debug!("EndBlockingHomeButtonShortAndLongPressed called");
        let mut applet = self.applet.lock().unwrap();
        applet.home_button_long_pressed_blocked = false;
        applet.home_button_short_pressed_blocked = false;
    }

    fn get_cache_storage_max_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        use crate::file_sys::control_metadata::RawNACP;
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let program_id = service.applet.lock().unwrap().program_id;
        let (result, data) = service.system.get().arp_manager().lock().unwrap().get_control_property(program_id);
        let mut index = 0;
        let mut size = 0;
        if result.is_success() {
            let data = data.expect("successful ARP control property");
            let mut raw = vec![0; std::mem::size_of::<RawNACP>()];
            let count = raw.len().min(data.len());
            raw[..count].copy_from_slice(&data[..count]);
            let offset = std::mem::offset_of!(RawNACP, cache_storage_max_index);
            index = u16::from_le_bytes(raw[offset..offset + 2].try_into().unwrap()) as u32;
            let offset = std::mem::offset_of!(RawNACP, cache_storage_data_and_journal_max_size);
            size = u64::from_le_bytes(raw[offset..offset + 8].try_into().unwrap());
        }
        // CMIF aligns the u64 output after the u32 output.
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(result);
        rb.push_u32(index);
        rb.push_u32(0);
        rb.push_u64(size);
    }

    fn begin_blocking_home_button_short_and_long_pressed_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let unused = RequestParser::new(ctx).pop_i64();
        service.begin_blocking_home_button_short_and_long_pressed(unused);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn end_blocking_home_button_short_and_long_pressed_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        service.end_blocking_home_button_short_and_long_pressed();
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn begin_blocking_home_button_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let timeout_ns = RequestParser::new(ctx).pop_i64();
        log::warn!("(STUBBED) BeginBlockingHomeButton timeout_ns={}", timeout_ns);
        {
            let mut applet = service.applet.lock().unwrap();
            applet.home_button_long_pressed_blocked = true;
            applet.home_button_short_pressed_blocked = true;
            applet.home_button_double_click_enabled = true;
        }
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn end_blocking_home_button_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        {
            let mut applet = service.applet.lock().unwrap();
            applet.home_button_long_pressed_blocked = false;
            applet.home_button_short_pressed_blocked = false;
            applet.home_button_double_click_enabled = false;
        }
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn set_media_playback_state_for_application_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let enabled = RequestParser::new(ctx).pop_raw::<u8>() != 0;
        service.applet.lock().unwrap().media_playback_state = enabled;
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn set_application_copyright_image_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) SetApplicationCopyrightImage called");
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn set_application_copyright_visibility_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let visible = RequestParser::new(ctx).pop_raw::<u8>() != 0;
        service.set_application_copyright_visibility(visible);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn clear_user_channel_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        service.applet.lock().unwrap().user_channel_launch_parameter.clear();
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn unpop_to_user_channel_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        use crate::hle::service::am::service::storage::IStorage;
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        // Upstream CMIF SharedPointer input requires a domain object ID.
        assert!(ctx.get_domain_message_header().is_some_and(|header| header.input_object_count() > 0));
        let id = RequestParser::new(ctx).pop_u32();
        let handler = {
            let manager = ctx.get_manager().expect("input interface manager");
            let manager = manager.lock().unwrap();
            assert!(manager.is_domain());
            manager.domain_handler(id.checked_sub(1).expect("input interface ID") as usize)
                .expect("input storage object").clone()
        };
        let storage = handler.as_any().downcast_ref::<IStorage>().expect("IStorage input interface");
        service.applet.lock().unwrap().user_channel_launch_parameter.push_back(storage.get_data());
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn get_notification_storage_channel_event_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let id = service.applet.lock().unwrap().ensure_notification_storage_channel_event_object_id(ctx).unwrap_or(0);
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(id);
    }

    /// Port of IApplicationFunctions::IsGamePlayRecordingSupported
    pub fn is_game_play_recording_supported(&self) -> bool {
        log::warn!("(STUBBED) IsGamePlayRecordingSupported called");
        false
    }

    /// Port of IApplicationFunctions::SetGamePlayRecordingState
    pub fn set_game_play_recording_state(&self, _state: GamePlayRecordingState) {
        log::warn!("(STUBBED) SetGamePlayRecordingState called");
    }

    /// Port of IApplicationFunctions::EnableApplicationCrashReport
    pub fn enable_application_crash_report(&self, _enabled: bool) {
        log::warn!("(STUBBED) EnableApplicationCrashReport called");
    }

    /// Port of IApplicationFunctions::SetApplicationCopyrightVisibility
    pub fn set_application_copyright_visibility(&self, _visible: bool) {
        log::warn!("(STUBBED) SetApplicationCopyrightVisibility called");
    }

    /// Port of IApplicationFunctions::ExecuteProgram
    pub fn execute_program(&self, _kind: ProgramSpecifyKind, value: u64) {
        assert!(matches!(_kind, ProgramSpecifyKind::ExecuteProgram | ProgramSpecifyKind::RestartProgram));
        log::info!(
            "ExecuteProgram called with kind={:?}, value={}",
            _kind,
            value
        );
        if !self.system.is_null() {
            let channel = self.applet.lock().unwrap().user_channel_launch_parameter.clone();
            *self.system.get().get_user_channel() = channel;
            self.system.get().execute_program(value as usize);
        } else {
            log::error!("ExecuteProgram: no System reference");
        }
    }

    fn execute_program_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let mut rp = RequestParser::new(ctx);
        let kind = match rp.pop_u32() {
            0 => ProgramSpecifyKind::ExecuteProgram,
            2 => ProgramSpecifyKind::RestartProgram,
            value => panic!("invalid ExecuteProgram kind {value}"),
        };
        rp.pop_u32(); // CMIF u64 alignment
        let value = rp.pop_u64();
        service.execute_program(kind, value);
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn create_application_and_request_to_start_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        use crate::file_sys::registered_cache::get_base_title_id;
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let id = RequestParser::new(ctx).pop_u64();
        let current = service.applet.lock().unwrap().program_id;
        let result = if id == 0 || get_base_title_id(id) == get_base_title_id(current) {
            let index = if id == 0 { 0 } else { id - get_base_title_id(id) };
            service.execute_program(ProgramSpecifyKind::ExecuteProgram, index);
            RESULT_SUCCESS
        } else {
            log::error!("Launching a different application is not implemented");
            ResultCode::new(u32::MAX)
        };
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    /// Port of IApplicationFunctions::GetPreviousProgramIndex
    pub fn get_previous_program_index(&self) -> i32 {
        log::warn!("(STUBBED) GetPreviousProgramIndex called");
        0
    }

    fn get_desired_language(&self) -> Result<u64, ResultCode> {
        let program_id = {
            let applet = self.applet.lock().unwrap();
            applet.program_id
        };

        let fs_controller = self.system.get().get_filesystem_controller();
        let fs_controller = fs_controller.lock().unwrap();

        let mut supported_languages = 0u32;
        if let Some(provider) = self.system.get().get_content_provider() {
            let provider = provider.lock().unwrap();

            let patch_manager = PatchManager::new(program_id, &fs_controller, &*provider);
            let metadata = patch_manager.get_control_metadata();
            if let Some(nacp) = metadata.0 {
                supported_languages = nacp.get_supported_languages();
            } else {
                let update_patch_manager =
                    PatchManager::new(get_update_title_id(program_id), &fs_controller, &*provider);
                let update_metadata = update_patch_manager.get_control_metadata();
                if let Some(nacp) = update_metadata.0 {
                    supported_languages = nacp.get_supported_languages();
                }
            }
        }

        let read_only = IReadOnlyApplicationControlDataInterface::new(self.system);
        let application_language =
            read_only.get_application_desired_language(supported_languages)?;
        read_only.convert_application_language_to_language_code(application_language)
    }

    /// Port of `IApplicationFunctions::GetDisplayVersion`.
    fn get_display_version(&self) -> [u8; 16] {
        let program_id = self.applet.lock().unwrap().program_id;
        let metadata =
            PatchManager::get_metadata_from_base_or_update(self.system.get(), program_id).0;
        let version = metadata.as_ref().map(|nacp| nacp.get_version_string());
        copy_display_version(version.as_deref())
    }

    /// Port of `IApplicationFunctions::EnsureSaveData`.
    fn ensure_save_data(&self, user_id: u128) -> Result<u64, ResultCode> {
        let program_id = self.applet.lock().unwrap().program_id;
        let uuid = common::uuid::UUID::from_bytes(user_id.to_le_bytes());
        log::info!("EnsureSaveData called, uid={}", uuid.formatted_string());

        let attribute = SaveDataAttribute::make_default(
            program_id,
            SaveDataType::Account,
            [user_id as u64, (user_id >> 64) as u64],
            0,
        );

        let file_system_controller = self.system.get().get_filesystem_controller();
        let save_data = file_system_controller
            .lock()
            .unwrap()
            .open_save_data_controller()
            .create_save_data(SaveDataSpaceId::User, &attribute);

        save_data
            .map(|_| 0)
            .ok_or_else(|| ResultCode::new(crate::file_sys::errors::RESULT_TARGET_NOT_FOUND.raw()))
    }

    /// Port of IApplicationFunctions::PrepareForJit
    pub fn prepare_for_jit(&self) {
        log::debug!("PrepareForJit called");
        let mut applet = self.applet.lock().unwrap();
        applet.jit_service_launched = true;
    }

    fn get_save_data_size(&self, save_type: SaveDataType, user_id: [u64; 2]) -> SaveDataSize {
        let program_id = self.applet.lock().unwrap().program_id;
        let filesystem = self.system.get().get_filesystem_controller();
        let controller = filesystem.lock().unwrap().open_save_data_controller();
        controller.read_save_data_size(self.system.get(), save_type, program_id, user_id)
    }

    fn extend_save_data(
        &self,
        save_type: SaveDataType,
        user_id: [u64; 2],
        size: SaveDataSize,
    ) -> u64 {
        let program_id = self.applet.lock().unwrap().program_id;
        let filesystem = self.system.get().get_filesystem_controller();
        let controller = filesystem.lock().unwrap().open_save_data_controller();
        controller.write_save_data_size(save_type, program_id, user_id, size);
        // Upstream reports zero required space after persisting the size pair.
        0
    }

    /// CMIF packs the byte-sized type followed immediately by UUID's byte array.
    /// Reading these separately with the word-based parser would skip UUID bytes.
    fn parse_save_data_identity(rp: &mut RequestParser<'_>) -> Option<(SaveDataType, [u64; 2])> {
        let raw = rp.pop_raw::<[u8; 17]>();
        let save_type = match raw[0] {
            0 => SaveDataType::System,
            1 => SaveDataType::Account,
            2 => SaveDataType::Bcat,
            3 => SaveDataType::Device,
            4 => SaveDataType::Temporary,
            5 => SaveDataType::Cache,
            6 => SaveDataType::SystemBcat,
            _ => return None, // Never construct an invalid Rust enum from guest bytes.
        };
        Some((
            save_type,
            [
                u64::from_le_bytes(raw[1..9].try_into().unwrap()),
                u64::from_le_bytes(raw[9..17].try_into().unwrap()),
            ],
        ))
    }

    fn get_save_data_size_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let Some((save_type, user_id)) =
            Self::parse_save_data_identity(&mut RequestParser::new(ctx))
        else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(ResultCode::new(
                crate::file_sys::errors::RESULT_INVALID_ARGUMENT.raw(),
            ));
            return;
        };
        let size = service.get_save_data_size(save_type, user_id);
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(size.normal);
        rb.push_u64(size.journal);
    }

    fn extend_save_data_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let mut rp = RequestParser::new(ctx);
        let Some((save_type, user_id)) = Self::parse_save_data_identity(&mut rp) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(ResultCode::new(
                crate::file_sys::errors::RESULT_INVALID_ARGUMENT.raw(),
            ));
            return;
        };
        rp.align_for::<u64>();
        let size = SaveDataSize {
            normal: rp.pop_u64(),
            journal: rp.pop_u64(),
        };
        let required_size = service.extend_save_data(save_type, user_id, size);
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(required_size);
    }

    fn notify_running_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationFunctions) };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(service.notify_running());
    }

    fn is_game_play_recording_supported_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationFunctions) };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(service.is_game_play_recording_supported());
    }

    fn enable_application_crash_report_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationFunctions) };
        let mut rp = RequestParser::new(ctx);
        service.enable_application_crash_report(rp.pop_bool());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_previous_program_index_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationFunctions) };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(service.get_previous_program_index());
    }

    fn prepare_for_jit_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationFunctions) };
        service.prepare_for_jit();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn push_interface_response(
        ctx: &mut HLERequestContext,
        object: Arc<dyn SessionRequestHandler>,
    ) {
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(object);
    }

    /// Port of IApplicationFunctions::PopLaunchParameter.
    fn pop_launch_parameter(
        &self,
        launch_parameter_kind: LaunchParameterKind,
    ) -> Result<Vec<u8>, ResultCode> {
        log::info!(
            "PopLaunchParameter called, kind={:?}",
            launch_parameter_kind
        );

        let mut applet = self.applet.lock().unwrap();
        let channel = if launch_parameter_kind == LaunchParameterKind::UserChannel {
            &mut applet.user_channel_launch_parameter
        } else {
            &mut applet.preselected_user_launch_parameter
        };

        let Some(data) = channel.pop_back() else {
            log::warn!(
                "Attempted to pop launch parameter {:?} but none was found",
                launch_parameter_kind
            );
            return Err(am_results::RESULT_NO_DATA_IN_CHANNEL);
        };

        Ok(data)
    }

    fn pop_launch_parameter_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationFunctions) };
        let mut rp = RequestParser::new(ctx);
        let launch_parameter_kind = match rp.pop_u32() {
            1 => LaunchParameterKind::UserChannel,
            2 => LaunchParameterKind::AccountPreselectedUser,
            _ => LaunchParameterKind::UserChannel,
        };

        match service.pop_launch_parameter(launch_parameter_kind) {
            Ok(data) => {
                let storage = Arc::new(super::storage::IStorage::new_with_system(
                    service.system,
                    data,
                ));
                Self::push_interface_response(ctx, storage);
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }

    fn try_pop_from_friend_invitation_storage_channel(
        &self,
    ) -> Result<Arc<super::storage::IStorage>, ResultCode> {
        log::debug!("TryPopFromFriendInvitationStorageChannel called");
        let mut applet = self.applet.lock().unwrap();
        let data = applet
            .friend_invitation_storage_channel
            .pop()
            .ok_or(am_results::RESULT_NO_DATA_IN_CHANNEL)?;
        Ok(Arc::new(super::storage::IStorage::new_with_system(
            self.system,
            data,
        )))
    }

    fn try_pop_from_friend_invitation_storage_channel_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        match service.try_pop_from_friend_invitation_storage_channel() {
            Ok(storage) => Self::push_interface_response(ctx, storage),
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }

    /// EnsureSaveData (cmd 20): ensures save data exists for the given user.
    fn ensure_save_data_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationFunctions) };
        let mut rp = RequestParser::new(ctx);
        let user_id = rp.pop_raw::<u128>();

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        match service.ensure_save_data(user_id) {
            Ok(size) => {
                rb.push_result(RESULT_SUCCESS);
                rb.push_u64(size);
            }
            Err(result) => {
                rb.push_result(result);
                rb.push_u64(0);
            }
        }
    }

    /// GetDesiredLanguage (cmd 21): returns the desired language code.
    fn get_desired_language_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationFunctions) };
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        match service.get_desired_language() {
            Ok(language_code) => {
                rb.push_result(RESULT_SUCCESS);
                rb.push_u64(language_code);
            }
            Err(result) => {
                rb.push_result(result);
                rb.push_u64(0);
            }
        }
    }

    /// SetTerminateResult (cmd 22).
    /// Matches upstream: locks applet, stores result.
    fn set_terminate_result_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationFunctions) };
        let mut rp = RequestParser::new(ctx);
        let result = rp.pop_u32();
        let result_code = crate::hle::result::ResultCode::new(result);
        log::info!(
            "SetTerminateResult: result={:#x} module={:?} description={}",
            result,
            result_code.get_module(),
            result_code.get_description()
        );

        let mut applet = service.applet.lock().unwrap();
        applet.terminate_result = result;

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// GetDisplayVersion (cmd 23): returns the application display version string.
    ///
    /// Port of upstream IApplicationFunctions::GetDisplayVersion.
    /// Upstream reads version from NACP metadata via PatchManager, falls back to "1.0.0".
    /// The result is a 16-byte null-terminated string (DisplayVersion struct).
    fn get_display_version_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationFunctions) };
        let version_bytes = service.get_display_version();

        log::debug!(
            "GetDisplayVersion: returning '{}'",
            std::str::from_utf8(&version_bytes)
                .unwrap_or("?")
                .trim_end_matches('\0')
        );

        // Response: header (2 words) + 16 bytes = 4 u32 words = 2 u64
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        // Push 16-byte DisplayVersion as two u64
        let lo = u64::from_le_bytes(version_bytes[0..8].try_into().unwrap());
        let hi = u64::from_le_bytes(version_bytes[8..16].try_into().unwrap());
        rb.push_u64(lo);
        rb.push_u64(hi);
    }

    /// CreateCacheStorage (cmd 27).
    ///
    /// CMIF lays the `u16` input at offset 0 and aligns the following `u64`
    /// values to offset 8. The outputs are likewise `u32` at offset 0 and
    /// `u64` at offset 8, including one zero padding word between them.
    fn create_cache_storage_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationFunctions) };
        let mut rp = RequestParser::new(ctx);
        let index = rp.pop_u16();
        rp.align_for::<u64>();
        let normal_size = rp.pop_u64();
        let journal_size = rp.pop_u64();

        let (target_media, required_size) =
            service.create_cache_storage(index, normal_size, journal_size);

        // Result (2 words) + 16 bytes of naturally-aligned output data.
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(target_media);
        rb.push_u32(0);
        rb.push_u64(required_size);
    }

    /// GetSaveDataSizeMax (cmd 28).
    fn get_save_data_size_max_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationFunctions) };
        let (max_normal_size, max_journal_size) = service.get_save_data_size_max();
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(max_normal_size);
        rb.push_u64(max_journal_size);
    }

    /// GetPseudoDeviceId (cmd 50): returns a pseudo device ID (UUID).
    fn get_pseudo_device_id_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) GetPseudoDeviceId called");
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        // Push 128-bit UUID (all zeros for stub)
        rb.push_u64(0);
        rb.push_u64(0);
    }

    /// InitializeGamePlayRecording (cmd 66)
    fn initialize_game_play_recording_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) InitializeGamePlayRecording called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// SetGamePlayRecordingState (cmd 67).
    /// Matches upstream: locks applet, stores state.
    fn set_game_play_recording_state_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationFunctions) };
        let mut rp = RequestParser::new(ctx);
        let state = rp.pop_u32();
        log::warn!("(STUBBED) SetGamePlayRecordingState: state={}", state);

        let mut applet = service.applet.lock().unwrap();
        applet.game_play_recording_state = match state {
            0 => crate::hle::service::am::am_types::GamePlayRecordingState::Disabled,
            1 => crate::hle::service::am::am_types::GamePlayRecordingState::Enabled,
            _ => crate::hle::service::am::am_types::GamePlayRecordingState::Disabled,
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// InitializeApplicationCopyrightFrameBuffer (cmd 100)
    fn initialize_application_copyright_frame_buffer_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) InitializeApplicationCopyrightFrameBuffer called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    /// Port of QueryApplicationPlayStatistics (cmd 110).
    fn query_application_play_statistics_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) QueryApplicationPlayStatistics called");
        // Eden returns no entries but CMIF still writes its output scratch
        // buffer. Initialize those unused bytes rather than exposing scratch.
        let output = vec![0u8; ctx.get_write_buffer_size(0)];
        ctx.write_buffer_b(&output, 0);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(0);
    }

    /// Port of QueryApplicationPlayStatisticsByUid (cmd 111).
    fn query_application_play_statistics_by_uid_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let _user_id = rp.pop_raw::<[u8; 16]>();
        log::warn!("(STUBBED) QueryApplicationPlayStatisticsByUid called");
        // No statistics, matching upstream for every UID/application list.
        let output = vec![0u8; ctx.get_write_buffer_size(0)];
        ctx.write_buffer_b(&output, 0);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(0);
    }

    /// GetGpuErrorDetectedSystemEvent (cmd 130): returns an event handle
    fn get_gpu_error_detected_system_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) GetGpuErrorDetectedSystemEvent called");
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationFunctions) };
        let object_id = service
            .applet
            .lock()
            .unwrap()
            .ensure_gpu_error_detected_system_event_object_id(ctx)
            .unwrap_or(0);
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    /// GetFriendInvitationStorageChannelEvent (cmd 120): returns an event handle
    fn get_friend_invitation_storage_channel_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) GetFriendInvitationStorageChannelEvent called");
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationFunctions) };
        let object_id = service
            .applet
            .lock()
            .unwrap()
            .ensure_friend_invitation_storage_channel_event_object_id(ctx)
            .unwrap_or(0);
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    /// GetHealthWarningDisappearedSystemEvent (cmd 160): returns an event handle
    fn get_health_warning_disappeared_system_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) GetHealthWarningDisappearedSystemEvent called");
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationFunctions) };
        let object_id = service
            .applet
            .lock()
            .unwrap()
            .ensure_health_warning_disappeared_system_event_object_id(ctx)
            .unwrap_or(0);
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    /// GetUnknownEvent210 (cmd 210): returns the applet's persistent event handle.
    fn get_unknown_event_210_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("Unknown210 called");
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationFunctions) };
        let object_id = service
            .applet
            .lock()
            .unwrap()
            .ensure_unknown_event_object_id(ctx)
            .unwrap_or(0);
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    fn unknown_330_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::debug!("Unknown330 called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u8(0);
    }
}

impl SessionRequestHandler for IApplicationFunctions {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }
}

impl ServiceFramework for IApplicationFunctions {
    fn get_service_name(&self) -> &str {
        "am::IApplicationFunctions"
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
    use crate::hle::kernel::k_process::{KProcess, ProcessLock};
    use crate::hle::kernel::k_thread::{KThread, KThreadLock};
    use crate::hle::service::am::applet::Applet;
    use crate::hle::service::hle_ipc::KAutoObjectRef;
    use crate::hle::service::os::process::Process;
    use std::sync::Mutex;

    fn make_service() -> IApplicationFunctions {
        let system = crate::core::SystemRef::null();
        IApplicationFunctions::new(
            system,
            Arc::new(Mutex::new(Applet::new(system, Process::new(), false))),
        )
    }

    #[test]
    fn friend_invitation_channel_returns_no_data_then_pops_back() {
        let service = make_service();
        let entry = service.handlers().get(&141).unwrap();
        assert_eq!(entry.name, "TryPopFromFriendInvitationStorageChannel");
        assert!(entry.handler_callback.is_some());
        assert_eq!(
            service.try_pop_from_friend_invitation_storage_channel().err(),
            Some(am_results::RESULT_NO_DATA_IN_CHANNEL),
        );
        service.applet.lock().unwrap().friend_invitation_storage_channel
            .extend([vec![1, 2], vec![3, 4]]);
        for expected in [vec![3, 4], vec![1, 2]] {
            let storage = service.try_pop_from_friend_invitation_storage_channel().unwrap();
            assert_eq!(storage.get_data(), expected);
        }
        let mut ctx = HLERequestContext::new();
        IApplicationFunctions::try_pop_from_friend_invitation_storage_channel_handler(
            &service, &mut ctx,
        );
        assert_eq!(ctx.cmd_buf[6], am_results::RESULT_NO_DATA_IN_CHANNEL.get_inner_value());
    }

    #[test]
    fn cache_storage_handlers_match_upstream_command_table() {
        let service = make_service();
        let create = service.handlers().get(&27).unwrap();
        let max = service.handlers().get(&28).unwrap();

        assert!(create.handler_callback.is_some());
        assert_eq!(create.name, "CreateCacheStorage");
        assert!(max.handler_callback.is_some());
        assert_eq!(max.name, "GetSaveDataSizeMax");
    }

    #[test]
    fn play_statistics_commands_return_success_and_zero_entries() {
        let service = make_service();
        for (id, name) in [
            (110, "QueryApplicationPlayStatistics"),
            (111, "QueryApplicationPlayStatisticsByUid"),
        ] {
            let entry = service.handlers().get(&id).expect("registered statistics command");
            assert_eq!(entry.name, name);
            for user_word in [0, 0x1234_5678, u32::MAX] {
                let mut ctx = HLERequestContext::new();
                ctx.cmd_buf[2..6].fill(user_word);
                ctx.cmd_buf[6..10].fill(0xCCCC_CCCC);
                entry.handler_callback.unwrap()(&service, &mut ctx);
                // CMIF success (u64), followed by a signed 32-bit entry count.
                assert_eq!(ctx.cmd_buf[6], RESULT_SUCCESS.get_inner_value());
                assert_eq!(ctx.cmd_buf[7], 0);
                assert_eq!(ctx.cmd_buf[8], 0);
                assert!(ctx.outgoing_copy_objects.is_empty());
            }
        }
    }

    #[test]
    fn home_button_commands_preserve_double_click_ownership() {
        let service = make_service();
        for (command, blocked, double_click) in [
            (32, true, true), (31, false, true), (30, true, true), (33, false, false),
        ] {
            let mut ctx = HLERequestContext::new();
            ctx.cmd_buf[2] = u32::MAX;
            ctx.cmd_buf[3] = u32::MAX;
            service.handlers()[&command].handler_callback.unwrap()(&service, &mut ctx);
            assert_eq!(ctx.cmd_buf[6], 0);
            let applet = service.applet.lock().unwrap();
            assert_eq!(applet.home_button_short_pressed_blocked, blocked);
            assert_eq!(applet.home_button_long_pressed_blocked, blocked);
            assert_eq!(applet.home_button_double_click_enabled, double_click);
        }
    }

    #[test]
    fn media_playback_and_user_channel_commands_update_applet_state() {
        let service = make_service();
        for enabled in [true, false] {
            let mut ctx = HLERequestContext::new();
            ctx.cmd_buf[2] = u32::from(enabled);
            service.handlers()[&60].handler_callback.unwrap()(&service, &mut ctx);
            assert_eq!(service.applet.lock().unwrap().media_playback_state, enabled);
        }
        service.applet.lock().unwrap().user_channel_launch_parameter.push_back(vec![1, 2, 3]);
        let mut ctx = HLERequestContext::new();
        service.handlers()[&121].handler_callback.unwrap()(&service, &mut ctx);
        assert!(service.applet.lock().unwrap().user_channel_launch_parameter.is_empty());
        for id in [101, 102] {
            let mut ctx = HLERequestContext::new();
            service.handlers()[&id].handler_callback.unwrap()(&service, &mut ctx);
            assert_eq!(ctx.cmd_buf[6], 0);
        }
    }

    #[test]
    fn notification_event_is_persistent_and_initially_unsignaled() {
        let service = make_service();
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        for _ in 0..2 {
            let mut ctx = HLERequestContext::new_with_thread(thread.clone(), 0x2000);
            service.handlers()[&150].handler_callback.unwrap()(&service, &mut ctx);
            let id = match ctx.outgoing_copy_objects.as_slice() {
                [KAutoObjectRef::ObjectId(id)] => *id,
                _ => panic!("expected one copied event"),
            };
            assert_ne!(id, 0);
            let applet = service.applet.lock().unwrap();
            let event = applet.notification_storage_channel_event.as_ref().unwrap().lock().unwrap();
            assert_eq!(id, event.object_id);
            assert!(!event.is_signaled.load(std::sync::atomic::Ordering::Relaxed));
        }
    }

    #[test]
    fn unpop_user_channel_copies_domain_storage() {
        use crate::hle::service::hle_ipc::SessionRequestManager;
        use crate::hle::service::am::service::storage::IStorage;
        let service = make_service();
        let manager = Arc::new(Mutex::new(SessionRequestManager::new()));
        {
            let mut manager = manager.lock().unwrap();
            manager.set_session_handler(Arc::new(IStorage::new(vec![7, 8, 9])));
            manager.convert_to_domain();
        }
        let mut ctx = HLERequestContext::new();
        ctx.set_session_request_manager(manager);
        let mut request = [0u32; crate::hle::ipc::COMMAND_BUFFER_LENGTH];
        request[0] = crate::hle::ipc::CommandType::Request as u32;
        request[1] = 16;
        request[4] = 1 | (1 << 8); // SendMessage, one input object
        request[5] = 16;
        request[6] = 1;
        request[8] = 0x4943_4653;
        request[10] = 122;
        request[12] = 1;
        ctx.populate_from_incoming_command_buffer(&request);
        service.handlers()[&122].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(service.applet.lock().unwrap().user_channel_launch_parameter.front(), Some(&vec![7, 8, 9]));
    }

    #[test]
    fn cache_storage_max_reads_control_metadata_and_preserves_arp_error() {
        use crate::file_sys::control_metadata::RawNACP;
        use crate::hle::service::glue::glue_manager::ApplicationLaunchProperty;
        let system = crate::core::System::new();
        let system_ref = crate::core::SystemRef::from_ref(&system);
        let applet = Arc::new(Mutex::new(Applet::new(system_ref, Process::new(), false)));
        applet.lock().unwrap().program_id = 0x100;
        let service = IApplicationFunctions::new(system_ref, applet);
        let mut ctx = HLERequestContext::new();
        service.handlers()[&29].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(ctx.cmd_buf[6], crate::hle::service::glue::errors::RESULT_PROCESS_ID_NOT_REGISTERED.get_inner_value());
        let mut nacp = vec![0; std::mem::size_of::<RawNACP>()];
        let index = std::mem::offset_of!(RawNACP, cache_storage_max_index);
        nacp[index..index + 2].copy_from_slice(&42u16.to_le_bytes());
        let size = std::mem::offset_of!(RawNACP, cache_storage_data_and_journal_max_size);
        nacp[size..size + 8].copy_from_slice(&0x1234_5678_9abc_def0u64.to_le_bytes());
        assert!(system.arp_manager().lock().unwrap().register(0x100, ApplicationLaunchProperty::default(), nacp).is_success());
        let mut ctx = HLERequestContext::new();
        service.handlers()[&29].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(&ctx.cmd_buf[6..12], &[0, 0, 42, 0, 0x9abc_def0, 0x1234_5678]);
    }

    #[test]
    fn execute_program_preserves_user_channel_before_frontend_restart() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_cb = seen.clone();
        let mut system = crate::core::System::new();
        system.register_execute_program_callback(Box::new(move |index| seen_cb.lock().unwrap().push(index)));
        let system_ref = crate::core::SystemRef::from_ref(&system);
        let applet = Arc::new(Mutex::new(Applet::new(system_ref, Process::new(), false)));
        applet.lock().unwrap().user_channel_launch_parameter.push_back(vec![4, 5]);
        applet.lock().unwrap().program_id = 0x2000;
        let service = IApplicationFunctions::new(system_ref, applet);
        let mut ctx = HLERequestContext::new();
        ctx.cmd_buf[2] = 2; // RestartProgram
        ctx.cmd_buf[4] = 3;
        service.handlers()[&120].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(*seen.lock().unwrap(), vec![3]);
        assert_eq!(system.get_user_channel_snapshot().front(), Some(&vec![4, 5]));
        let mut ctx = HLERequestContext::new();
        ctx.cmd_buf[2] = 0x2002;
        service.handlers()[&12].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(*seen.lock().unwrap(), vec![3, 2]);
        let mut ctx = HLERequestContext::new();
        ctx.cmd_buf[2] = 0x4000;
        service.handlers()[&12].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(ctx.cmd_buf[6], u32::MAX);
        assert_eq!(*seen.lock().unwrap(), vec![3, 2]);
    }

    #[test]
    fn play_statistics_zeroes_only_the_map_alias_output_buffer() {
        use crate::device_memory::DeviceMemory;
        use crate::memory::memory::Memory;
        use common::page_table::{PageTable, PageType};
        use crate::hle::ipc;

        // Keep both backing objects alive until the IPC memory bridge is dropped.
        let backing = Box::new(DeviceMemory::new());
        let mut table = Box::new(PageTable::new());
        table.resize(32, 12);
        table.entries.get_and_fault(3).store(
            false, PageType::Memory, 1, backing.buffer.backing_base_pointer() as usize,
        );
        let memory = Arc::new(Mutex::new(unsafe {
            Memory::new(crate::core::SystemRef::null(), backing.as_ref(), &backing.buffer)
        }));
        memory.lock().unwrap().set_current_page_table(table.as_mut(), true);
        let service = make_service();
        for id in [110, 111] {
            for size in [0, 0x18, 0x30, 0x31] {
                memory.lock().unwrap().write_block(0x3000, &[0xCC; 128]);
                let mut ctx = HLERequestContext::new();
                let mut request = [0u32; ipc::COMMAND_BUFFER_LENGTH];
                request[0] = ipc::CommandType::Request as u32 | (1 << 24);
                request[1] = 15 | (3 << 10);
                request[2] = size;
                request[3] = 0x3010;
                request[8] = 0x4943_4653;
                request[10] = id;
                // Alias input/output selection must not overwrite the C buffer.
                request[20] = 0x3060;
                request[21] = 16 << 16;
                ctx.populate_from_incoming_command_buffer(&request);
                ctx.set_memory(memory.clone());
                service.handlers()[&id].handler_callback.unwrap()(&service, &mut ctx);
                let mut bytes = [0; 128];
                memory.lock().unwrap().read_block(0x3000, &mut bytes);
                let mut expected = [0xCC; 128];
                expected[16..16 + size as usize].fill(0);
                assert_eq!(bytes, expected, "command {id}, buffer size {size}");
                assert_eq!(ctx.cmd_buf[8], 0);
            }
        }
    }

    #[test]
    fn save_size_commands_and_packed_identity_match_cmif() {
        let service = make_service();
        for (id, name) in [(25, "ExtendSaveData"), (26, "GetSaveDataSize")] {
            let handler = service.handlers().get(&id).unwrap();
            assert_eq!(handler.name, name);
            assert!(handler.handler_callback.is_some());
        }
        let mut ctx = HLERequestContext::new();
        let mut raw = [0xCCu8; 40];
        raw[0] = SaveDataType::Account as u8;
        let uuid = *b"synthetic-user!!";
        raw[1..17].copy_from_slice(&uuid);
        raw[24..32].copy_from_slice(&(1u64 << 40).to_le_bytes());
        raw[32..40].copy_from_slice(&8192u64.to_le_bytes());
        for (slot, bytes) in ctx.cmd_buf[2..12].iter_mut().zip(raw.chunks_exact(4)) {
            *slot = u32::from_le_bytes(bytes.try_into().unwrap());
        }
        let mut rp = RequestParser::new(&mut ctx);
        let (kind, user) = IApplicationFunctions::parse_save_data_identity(&mut rp).unwrap();
        assert_eq!(kind, SaveDataType::Account);
        assert_eq!(
            user,
            [
                u64::from_le_bytes(uuid[..8].try_into().unwrap()),
                u64::from_le_bytes(uuid[8..].try_into().unwrap())
            ]
        );
        rp.align_for::<u64>();
        assert_eq!(rp.pop_u64(), 1 << 40);
        assert_eq!(rp.pop_u64(), 8192);
    }

    #[test]
    fn ensure_save_data_propagates_filesystem_creation_failure() {
        let system = crate::core::System::new();
        let system_ref = crate::core::SystemRef::from_ref(&system);
        let applet = Arc::new(Mutex::new(Applet::new(system_ref, Process::new(), false)));
        applet.lock().unwrap().program_id = 0x0100_DCA0_064A_6000;
        let service = IApplicationFunctions::new(system_ref, applet);

        // A fresh System has no VFS-backed SaveDataFactory. Eden propagates
        // CreateSaveData's ResultTargetNotFound instead of reporting success.
        let result = service
            .ensure_save_data(u128::from_le_bytes(*b"Eden Default UID"))
            .unwrap_err();

        assert_eq!(
            result.get_inner_value(),
            crate::file_sys::errors::RESULT_TARGET_NOT_FOUND.raw()
        );
    }

    #[test]
    fn unknown_210_handler_matches_upstream_command_table() {
        let service = make_service();
        let unknown = service.handlers().get(&210).unwrap();

        assert!(unknown.handler_callback.is_some());
        assert_eq!(unknown.name, "Unknown210");
    }

    #[test]
    fn unknown_330_matches_upstream_command_and_output() {
        let service = make_service();
        let unknown = service.handlers().get(&330).unwrap();
        assert!(unknown.handler_callback.is_some());
        assert_eq!(unknown.name, "Unknown330");

        let thread = Arc::new(KThreadLock::new(KThread::new()));
        let mut ctx = HLERequestContext::new_with_thread(thread, 0x2000);
        IApplicationFunctions::unknown_330_handler(&service, &mut ctx);

        assert_eq!(ctx.cmd_buf[6], RESULT_SUCCESS.get_inner_value());
        assert_eq!(ctx.cmd_buf[8] & 0xff, 0);
    }

    #[test]
    fn unknown_210_returns_the_applets_unsignaled_persistent_event() {
        let service = make_service();
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        let mut ctx = HLERequestContext::new_with_thread(thread, 0x2000);

        IApplicationFunctions::get_unknown_event_210_handler(&service, &mut ctx);

        assert_eq!(ctx.cmd_buf[6], RESULT_SUCCESS.get_inner_value());
        let object_id = match ctx.outgoing_copy_objects.as_slice() {
            [KAutoObjectRef::ObjectId(object_id)] => *object_id,
            _ => panic!("expected one object-backed copy handle"),
        };
        assert_ne!(object_id, 0);

        let applet = service.applet.lock().unwrap();
        let event = applet.unknown_event.as_ref().unwrap().lock().unwrap();
        assert_eq!(event.object_id, object_id);
        assert!(!event.is_signaled.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn create_cache_storage_matches_upstream_stub_outputs() {
        let service = make_service();
        assert_eq!(service.create_cache_storage(3, 0x1000, 0x2000), (1, 0));
    }

    #[test]
    fn create_cache_storage_reply_preserves_cmif_output_alignment() {
        let service = make_service();
        let mut ctx = HLERequestContext::new();
        // Raw inputs: u16 index at +0, six bytes padding, then two u64 values.
        ctx.cmd_buf[2] = 3;
        ctx.cmd_buf[4] = 0x1000;
        ctx.cmd_buf[6] = 0x2000;

        IApplicationFunctions::create_cache_storage_handler(&service, &mut ctx);

        // Non-domain reply payload begins at word 6. Result occupies 6..8;
        // output data is u32 at +0, zero padding at +4, u64 at +8.
        assert_eq!(ctx.cmd_buf[6], RESULT_SUCCESS.get_inner_value());
        assert_eq!(ctx.cmd_buf[7], 0);
        assert_eq!(ctx.cmd_buf[8], 1);
        assert_eq!(ctx.cmd_buf[9], 0);
        assert_eq!(ctx.cmd_buf[10], 0);
        assert_eq!(ctx.cmd_buf[11], 0);
    }

    #[test]
    fn display_version_defaults_when_control_metadata_is_absent() {
        assert_eq!(
            copy_display_version(None),
            [b'1', b'.', b'0', b'.', b'0', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn display_version_is_bounded_and_null_terminated() {
        assert_eq!(
            copy_display_version(Some("1234567890abcdef-more")),
            *b"1234567890abcde\0"
        );
    }
}
