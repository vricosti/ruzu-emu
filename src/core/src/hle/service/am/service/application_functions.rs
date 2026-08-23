// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/service/application_functions.h
//! Port of zuyu/src/core/hle/service/am/service/application_functions.cpp

use crate::hle::service::am::am_types::{
    GamePlayRecordingState, LaunchParameterKind, ProgramSpecifyKind,
};
use std::collections::BTreeMap;
use std::sync::Arc;

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
/// - 110: QueryApplicationPlayStatistics (unimplemented)
/// - 111: QueryApplicationPlayStatisticsByUid (unimplemented)
/// - 120: ExecuteProgram (unimplemented)
/// - 121: ClearUserChannel (unimplemented)
/// - 122: UnpopToUserChannel (unimplemented)
/// - 123: GetPreviousProgramIndex
/// - 124: EnableApplicationAllThreadDumpOnCrash (unimplemented)
/// - 130: GetGpuErrorDetectedSystemEvent
/// - 131: SetDelayTimeToAbortOnGpuError (unimplemented)
/// - 140: GetFriendInvitationStorageChannelEvent
/// - 141: TryPopFromFriendInvitationStorageChannel (unimplemented)
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
        log::info!(
            "ExecuteProgram called with kind={:?}, value={}",
            _kind,
            value
        );
        if !self.system.is_null() {
            self.system.get().execute_program(value as usize);
        } else {
            log::error!("ExecuteProgram: no System reference");
        }
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

    /// Port of IApplicationFunctions::PrepareForJit
    pub fn prepare_for_jit(&self) {
        log::debug!("PrepareForJit called");
        let mut applet = self.applet.lock().unwrap();
        applet.jit_service_launched = true;
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

    /// EnsureSaveData (cmd 20): ensures save data exists for the given user.
    /// Upstream creates save data via filesystem controller, returns Out<u64> = 0.
    fn ensure_save_data_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) EnsureSaveData called");
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(0); // size = 0
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

        log::info!(
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

    /// GetGpuErrorDetectedSystemEvent (cmd 110): returns an event handle
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
