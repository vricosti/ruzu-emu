// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/pctl/parental_control_service.h
//! Port of zuyu/src/core/hle/service/pctl/parental_control_service.cpp
//!
//! IParentalControlService — actual parental control operations.

use std::collections::BTreeMap;

#[cfg(test)]
mod suspension_event_tests {
    use super::*;
    use crate::hle::kernel::k_process::{KProcess, ProcessLock};
    use crate::hle::kernel::k_readable_event::KReadableEvent;
    use crate::hle::kernel::k_thread::{KThread, KThreadLock};
    use crate::hle::service::hle_ipc::KAutoObjectRef;
    use std::sync::{Arc, Mutex};

    #[test]
    fn suspension_event_returns_stable_unsignaled_handle() {
        let service =
            IParentalControlService::new(crate::core::SystemRef::null(), Capability::SYSTEM);
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let readable = Arc::new(Mutex::new(KReadableEvent::new()));
        readable.lock().unwrap().initialize(1, 2);
        service
            .request_suspension_event
            .attach_kernel_event(readable.clone(), process.clone());
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        for _ in 0..2 {
            let mut ctx = HLERequestContext::new_with_thread(thread.clone(), 0);
            service.handlers[&1457].handler_callback.unwrap()(&service, &mut ctx);
            assert!(matches!(
                ctx.outgoing_copy_objects.as_slice(),
                [KAutoObjectRef::ObjectId(2)]
            ));
            assert!(!readable.lock().unwrap().is_signaled());
        }
    }

    #[test]
    fn firmware_23_play_timer_display_commands_are_registered() {
        let service =
            IParentalControlService::new(crate::core::SystemRef::null(), Capability::SYSTEM);
        assert_eq!(
            service.handlers[&1459].name,
            "GetPlayTimerRemainingTimeDisplayInfo"
        );
        assert_eq!(service.handlers[&1460].name, "Unknown1460");
    }
}

use super::pctl_results::*;
use super::pctl_types::{
    ApplicationInfo, Capability, PlayTimerRemainingTimeDisplayInfo, PlayTimerSettings,
    RestrictionSettings,
};
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::os::event::Event;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for IParentalControlService.
///
/// Corresponds to the function table in upstream parental_control_service.cpp.
pub mod commands {
    pub const INITIALIZE: u32 = 1;
    pub const CHECK_FREE_COMMUNICATION_PERMISSION: u32 = 1001;
    pub const CONFIRM_LAUNCH_APPLICATION_PERMISSION: u32 = 1002;
    pub const CONFIRM_RESUME_APPLICATION_PERMISSION: u32 = 1003;
    pub const CONFIRM_SNS_POST_PERMISSION: u32 = 1004;
    pub const CONFIRM_SYSTEM_SETTINGS_PERMISSION: u32 = 1005;
    pub const IS_RESTRICTION_TEMPORARY_UNLOCKED: u32 = 1006;
    pub const REVERT_RESTRICTION_TEMPORARY_UNLOCKED: u32 = 1007;
    pub const ENTER_RESTRICTED_SYSTEM_SETTINGS: u32 = 1008;
    pub const LEAVE_RESTRICTED_SYSTEM_SETTINGS: u32 = 1009;
    pub const IS_RESTRICTED_SYSTEM_SETTINGS_ENTERED: u32 = 1010;
    pub const REVERT_RESTRICTED_SYSTEM_SETTINGS_ENTERED: u32 = 1011;
    pub const GET_RESTRICTED_FEATURES: u32 = 1012;
    pub const CONFIRM_STEREO_VISION_PERMISSION: u32 = 1013;
    pub const CONFIRM_PLAYABLE_APPLICATION_VIDEO_OLD: u32 = 1014;
    pub const CONFIRM_PLAYABLE_APPLICATION_VIDEO: u32 = 1015;
    pub const CONFIRM_SHOW_NEWS_PERMISSION: u32 = 1016;
    pub const END_FREE_COMMUNICATION: u32 = 1017;
    pub const IS_FREE_COMMUNICATION_AVAILABLE: u32 = 1018;
    pub const IS_RESTRICTION_ENABLED: u32 = 1031;
    pub const GET_SAFETY_LEVEL: u32 = 1032;
    pub const SET_SAFETY_LEVEL: u32 = 1033;
    pub const GET_SAFETY_LEVEL_SETTINGS: u32 = 1034;
    pub const GET_CURRENT_SETTINGS: u32 = 1035;
    pub const SET_CUSTOM_SAFETY_LEVEL_SETTINGS: u32 = 1036;
    pub const GET_DEFAULT_RATING_ORGANIZATION: u32 = 1037;
    pub const SET_DEFAULT_RATING_ORGANIZATION: u32 = 1038;
    pub const GET_FREE_COMMUNICATION_APPLICATION_LIST_COUNT: u32 = 1039;
    pub const ADD_TO_FREE_COMMUNICATION_APPLICATION_LIST: u32 = 1042;
    pub const DELETE_SETTINGS: u32 = 1043;
    pub const GET_FREE_COMMUNICATION_APPLICATION_LIST: u32 = 1044;
    pub const UPDATE_FREE_COMMUNICATION_APPLICATION_LIST: u32 = 1045;
    pub const DISABLE_FEATURES_FOR_RESET: u32 = 1046;
    pub const NOTIFY_APPLICATION_DOWNLOAD_STARTED: u32 = 1047;
    pub const NOTIFY_NETWORK_PROFILE_CREATED: u32 = 1048;
    pub const RESET_FREE_COMMUNICATION_APPLICATION_LIST: u32 = 1049;
    pub const CONFIRM_STEREO_VISION_RESTRICTION_CONFIGURABLE: u32 = 1061;
    pub const GET_STEREO_VISION_RESTRICTION: u32 = 1062;
    pub const SET_STEREO_VISION_RESTRICTION: u32 = 1063;
    pub const RESET_CONFIRMED_STEREO_VISION_PERMISSION: u32 = 1064;
    pub const IS_STEREO_VISION_PERMITTED: u32 = 1065;
    pub const UNLOCK_RESTRICTION_TEMPORARILY: u32 = 1201;
    pub const UNLOCK_SYSTEM_SETTINGS_RESTRICTION: u32 = 1202;
    pub const SET_PIN_CODE: u32 = 1203;
    pub const GENERATE_INQUIRY_CODE: u32 = 1204;
    pub const CHECK_MASTER_KEY: u32 = 1205;
    pub const GET_PIN_CODE_LENGTH: u32 = 1206;
    pub const GET_PIN_CODE_CHANGED_EVENT: u32 = 1207;
    pub const GET_PIN_CODE: u32 = 1208;
    pub const IS_PAIRING_ACTIVE: u32 = 1403;
    pub const GET_SETTINGS_LAST_UPDATED: u32 = 1406;
    pub const GET_PAIRING_ACCOUNT_INFO: u32 = 1411;
    pub const GET_ACCOUNT_NICKNAME: u32 = 1421;
    pub const GET_ACCOUNT_STATE: u32 = 1424;
    pub const REQUEST_POST_EVENTS: u32 = 1425;
    pub const GET_POST_EVENT_INTERVAL: u32 = 1426;
    pub const SET_POST_EVENT_INTERVAL: u32 = 1427;
    pub const GET_SYNCHRONIZATION_EVENT: u32 = 1432;
    pub const START_PLAY_TIMER: u32 = 1451;
    pub const STOP_PLAY_TIMER: u32 = 1452;
    pub const IS_PLAY_TIMER_ENABLED: u32 = 1453;
    pub const GET_PLAY_TIMER_REMAINING_TIME: u32 = 1454;
    pub const IS_RESTRICTED_BY_PLAY_TIMER: u32 = 1455;
    pub const GET_PLAY_TIMER_SETTINGS: u32 = 1456;
    pub const GET_PLAY_TIMER_EVENT_TO_REQUEST_SUSPENSION: u32 = 1457;
    pub const IS_PLAY_TIMER_ALARM_DISABLED: u32 = 1458;
    pub const GET_PLAY_TIMER_REMAINING_TIME_DISPLAY_INFO: u32 = 1459;
    pub const UNKNOWN_1460: u32 = 1460;
    pub const NOTIFY_WRONG_PIN_CODE_INPUT_MANY_TIMES: u32 = 1471;
    pub const CANCEL_NETWORK_REQUEST: u32 = 1472;
    pub const GET_UNLINKED_EVENT: u32 = 1473;
    pub const CLEAR_UNLINKED_EVENT: u32 = 1474;
    pub const DISABLE_ALL_FEATURES: u32 = 1601;
    pub const POST_ENABLE_ALL_FEATURES: u32 = 1602;
    pub const IS_ALL_FEATURES_DISABLED: u32 = 1603;
    pub const DELETE_FROM_FREE_COMMUNICATION_APPLICATION_LIST_FOR_DEBUG: u32 = 1901;
    pub const CLEAR_FREE_COMMUNICATION_APPLICATION_LIST_FOR_DEBUG: u32 = 1902;
    pub const GET_EXEMPT_APPLICATION_LIST_COUNT_FOR_DEBUG: u32 = 1903;
    pub const GET_EXEMPT_APPLICATION_LIST_FOR_DEBUG: u32 = 1904;
    pub const UPDATE_EXEMPT_APPLICATION_LIST_FOR_DEBUG: u32 = 1905;
    pub const ADD_TO_EXEMPT_APPLICATION_LIST_FOR_DEBUG: u32 = 1906;
    pub const DELETE_FROM_EXEMPT_APPLICATION_LIST_FOR_DEBUG: u32 = 1907;
    pub const CLEAR_EXEMPT_APPLICATION_LIST_FOR_DEBUG: u32 = 1908;
    pub const DELETE_PAIRING: u32 = 1941;
    pub const SET_PLAY_TIMER_SETTINGS_FOR_DEBUG: u32 = 1951;
    pub const GET_PLAY_TIMER_SPENT_TIME_FOR_TEST: u32 = 1952;
    pub const SET_PLAY_TIMER_ALARM_DISABLED_FOR_DEBUG: u32 = 1953;
    pub const REQUEST_PAIRING_ASYNC: u32 = 2001;
    pub const FINISH_REQUEST_PAIRING: u32 = 2002;
    pub const AUTHORIZE_PAIRING_ASYNC: u32 = 2003;
    pub const FINISH_AUTHORIZE_PAIRING: u32 = 2004;
    pub const RETRIEVE_PAIRING_INFO_ASYNC: u32 = 2005;
    pub const FINISH_RETRIEVE_PAIRING_INFO: u32 = 2006;
    pub const UNLINK_PAIRING_ASYNC: u32 = 2007;
    pub const FINISH_UNLINK_PAIRING: u32 = 2008;
    pub const GET_ACCOUNT_MII_IMAGE_ASYNC: u32 = 2009;
    pub const FINISH_GET_ACCOUNT_MII_IMAGE: u32 = 2010;
    pub const GET_ACCOUNT_MII_IMAGE_CONTENT_TYPE_ASYNC: u32 = 2011;
    pub const FINISH_GET_ACCOUNT_MII_IMAGE_CONTENT_TYPE: u32 = 2012;
    pub const SYNCHRONIZE_PARENTAL_CONTROL_SETTINGS_ASYNC: u32 = 2013;
    pub const FINISH_SYNCHRONIZE_PARENTAL_CONTROL_SETTINGS: u32 = 2014;
    pub const FINISH_SYNCHRONIZE_PARENTAL_CONTROL_SETTINGS_WITH_LAST_UPDATED: u32 = 2015;
    pub const REQUEST_UPDATE_EXEMPTION_LIST_ASYNC: u32 = 2016;
}

/// Internal states for the parental control service.
///
/// Corresponds to `States` in upstream parental_control_service.h.
#[derive(Debug, Clone, Default)]
struct States {
    application_info: ApplicationInfo,
    tid_from_event: u64,
    launch_time_valid: bool,
    is_suspended: bool,
    temporary_unlocked: bool,
    free_communication: bool,
    stereo_vision: bool,
}

/// Internal settings for parental control.
///
/// Corresponds to `ParentalControlSettings` in upstream parental_control_service.h.
#[derive(Debug, Clone, Default)]
struct ParentalControlSettings {
    is_stero_vision_restricted: bool,
    is_free_communication_default_on: bool,
    disabled: bool,
}

/// IParentalControlService.
///
/// Corresponds to `IParentalControlService` in upstream parental_control_service.h.
pub struct IParentalControlService {
    system: crate::core::SystemRef,
    states: States,
    settings: ParentalControlSettings,
    restriction_settings: RestrictionSettings,
    pin_code: [u8; 8],
    request_suspension_event: Event,
    capability: Capability,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IParentalControlService {
    fn get_play_timer_event_to_request_suspension_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let Some(id) = service.request_suspension_event.copy_object_id(ctx) else {
            ResponseBuilder::new(ctx, 2, 0, 0).push_result(crate::hle::result::RESULT_UNKNOWN);
            return;
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(id);
    }

    /// Stub handler for nullptr entries -- logs STUBBED and returns RESULT_SUCCESS.
    fn stub_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let cmd = ctx.get_command();
        log::warn!("(STUBBED) IParentalControlService command {}", cmd);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    pub fn new(system: crate::core::SystemRef, capability: Capability) -> Self {
        let handlers = build_handler_map(&[
            (
                commands::INITIALIZE,
                Some(Self::initialize_handler),
                "Initialize",
            ),
            (
                commands::CHECK_FREE_COMMUNICATION_PERMISSION,
                Some(Self::check_free_communication_permission_handler),
                "CheckFreeCommunicationPermission",
            ),
            (
                commands::CONFIRM_LAUNCH_APPLICATION_PERMISSION,
                Some(Self::confirm_launch_application_permission_handler),
                "ConfirmLaunchApplicationPermission",
            ),
            (
                commands::CONFIRM_RESUME_APPLICATION_PERMISSION,
                Some(Self::confirm_resume_application_permission_handler),
                "ConfirmResumeApplicationPermission",
            ),
            (
                commands::CONFIRM_SNS_POST_PERMISSION,
                Some(Self::confirm_sns_post_permission_handler),
                "ConfirmSnsPostPermission",
            ),
            (
                commands::CONFIRM_SYSTEM_SETTINGS_PERMISSION,
                Some(Self::stub_handler),
                "ConfirmSystemSettingsPermission",
            ),
            (
                commands::IS_RESTRICTION_TEMPORARY_UNLOCKED,
                Some(Self::is_restriction_temporary_unlocked_handler),
                "IsRestrictionTemporaryUnlocked",
            ),
            (
                commands::REVERT_RESTRICTION_TEMPORARY_UNLOCKED,
                Some(Self::stub_handler),
                "RevertRestrictionTemporaryUnlocked",
            ),
            (
                commands::ENTER_RESTRICTED_SYSTEM_SETTINGS,
                Some(Self::stub_handler),
                "EnterRestrictedSystemSettings",
            ),
            (
                commands::LEAVE_RESTRICTED_SYSTEM_SETTINGS,
                Some(Self::stub_handler),
                "LeaveRestrictedSystemSettings",
            ),
            (
                commands::IS_RESTRICTED_SYSTEM_SETTINGS_ENTERED,
                Some(Self::is_restricted_system_settings_entered_handler),
                "IsRestrictedSystemSettingsEntered",
            ),
            (
                commands::REVERT_RESTRICTED_SYSTEM_SETTINGS_ENTERED,
                Some(Self::stub_handler),
                "RevertRestrictedSystemSettingsEntered",
            ),
            (
                commands::GET_RESTRICTED_FEATURES,
                Some(Self::stub_handler),
                "GetRestrictedFeatures",
            ),
            (
                commands::CONFIRM_STEREO_VISION_PERMISSION,
                Some(Self::confirm_stereo_vision_permission_handler),
                "ConfirmStereoVisionPermission",
            ),
            (
                commands::CONFIRM_PLAYABLE_APPLICATION_VIDEO_OLD,
                Some(Self::stub_handler),
                "ConfirmPlayableApplicationVideoOld",
            ),
            (
                commands::CONFIRM_PLAYABLE_APPLICATION_VIDEO,
                Some(Self::stub_handler),
                "ConfirmPlayableApplicationVideo",
            ),
            (
                commands::CONFIRM_SHOW_NEWS_PERMISSION,
                Some(Self::stub_handler),
                "ConfirmShowNewsPermission",
            ),
            (
                commands::END_FREE_COMMUNICATION,
                Some(Self::end_free_communication_handler),
                "EndFreeCommunication",
            ),
            (
                commands::IS_FREE_COMMUNICATION_AVAILABLE,
                Some(Self::is_free_communication_available_handler),
                "IsFreeCommunicationAvailable",
            ),
            (
                commands::IS_RESTRICTION_ENABLED,
                Some(Self::is_restriction_enabled_handler),
                "IsRestrictionEnabled",
            ),
            (
                commands::GET_SAFETY_LEVEL,
                Some(Self::get_safety_level_handler),
                "GetSafetyLevel",
            ),
            (
                commands::SET_SAFETY_LEVEL,
                Some(Self::stub_handler),
                "SetSafetyLevel",
            ),
            (
                commands::GET_SAFETY_LEVEL_SETTINGS,
                Some(Self::stub_handler),
                "GetSafetyLevelSettings",
            ),
            (
                commands::GET_CURRENT_SETTINGS,
                Some(Self::get_current_settings_handler),
                "GetCurrentSettings",
            ),
            (
                commands::SET_CUSTOM_SAFETY_LEVEL_SETTINGS,
                Some(Self::stub_handler),
                "SetCustomSafetyLevelSettings",
            ),
            (
                commands::GET_DEFAULT_RATING_ORGANIZATION,
                Some(Self::stub_handler),
                "GetDefaultRatingOrganization",
            ),
            (
                commands::SET_DEFAULT_RATING_ORGANIZATION,
                Some(Self::stub_handler),
                "SetDefaultRatingOrganization",
            ),
            (
                commands::GET_FREE_COMMUNICATION_APPLICATION_LIST_COUNT,
                Some(Self::get_free_communication_application_list_count_handler),
                "GetFreeCommunicationApplicationListCount",
            ),
            (
                commands::ADD_TO_FREE_COMMUNICATION_APPLICATION_LIST,
                Some(Self::stub_handler),
                "AddToFreeCommunicationApplicationList",
            ),
            (
                commands::DELETE_SETTINGS,
                Some(Self::stub_handler),
                "DeleteSettings",
            ),
            (
                commands::GET_FREE_COMMUNICATION_APPLICATION_LIST,
                Some(Self::stub_handler),
                "GetFreeCommunicationApplicationList",
            ),
            (
                commands::UPDATE_FREE_COMMUNICATION_APPLICATION_LIST,
                Some(Self::stub_handler),
                "UpdateFreeCommunicationApplicationList",
            ),
            (
                commands::DISABLE_FEATURES_FOR_RESET,
                Some(Self::stub_handler),
                "DisableFeaturesForReset",
            ),
            (
                commands::NOTIFY_APPLICATION_DOWNLOAD_STARTED,
                Some(Self::stub_handler),
                "NotifyApplicationDownloadStarted",
            ),
            (
                commands::NOTIFY_NETWORK_PROFILE_CREATED,
                Some(Self::stub_handler),
                "NotifyNetworkProfileCreated",
            ),
            (
                commands::RESET_FREE_COMMUNICATION_APPLICATION_LIST,
                Some(Self::stub_handler),
                "ResetFreeCommunicationApplicationList",
            ),
            (
                commands::CONFIRM_STEREO_VISION_RESTRICTION_CONFIGURABLE,
                Some(Self::confirm_stereo_vision_restriction_configurable_handler),
                "ConfirmStereoVisionRestrictionConfigurable",
            ),
            (
                commands::GET_STEREO_VISION_RESTRICTION,
                Some(Self::get_stereo_vision_restriction_handler),
                "GetStereoVisionRestriction",
            ),
            (
                commands::SET_STEREO_VISION_RESTRICTION,
                Some(Self::set_stereo_vision_restriction_handler),
                "SetStereoVisionRestriction",
            ),
            (
                commands::RESET_CONFIRMED_STEREO_VISION_PERMISSION,
                Some(Self::reset_confirmed_stereo_vision_permission_handler),
                "ResetConfirmedStereoVisionPermission",
            ),
            (
                commands::IS_STEREO_VISION_PERMITTED,
                Some(Self::is_stereo_vision_permitted_handler),
                "IsStereoVisionPermitted",
            ),
            (
                commands::UNLOCK_RESTRICTION_TEMPORARILY,
                Some(Self::stub_handler),
                "UnlockRestrictionTemporarily",
            ),
            (
                commands::UNLOCK_SYSTEM_SETTINGS_RESTRICTION,
                Some(Self::stub_handler),
                "UnlockSystemSettingsRestriction",
            ),
            (
                commands::SET_PIN_CODE,
                Some(Self::stub_handler),
                "SetPinCode",
            ),
            (
                commands::GENERATE_INQUIRY_CODE,
                Some(Self::stub_handler),
                "GenerateInquiryCode",
            ),
            (
                commands::CHECK_MASTER_KEY,
                Some(Self::stub_handler),
                "CheckMasterKey",
            ),
            (
                commands::GET_PIN_CODE_LENGTH,
                Some(Self::get_pin_code_length_handler),
                "GetPinCodeLength",
            ),
            (
                commands::GET_PIN_CODE_CHANGED_EVENT,
                Some(Self::stub_handler),
                "GetPinCodeChangedEvent",
            ),
            (
                commands::GET_PIN_CODE,
                Some(Self::stub_handler),
                "GetPinCode",
            ),
            (
                commands::IS_PAIRING_ACTIVE,
                Some(Self::is_pairing_active_handler),
                "IsPairingActive",
            ),
            (
                commands::GET_SETTINGS_LAST_UPDATED,
                Some(Self::stub_handler),
                "GetSettingsLastUpdated",
            ),
            (
                commands::GET_PAIRING_ACCOUNT_INFO,
                Some(Self::stub_handler),
                "GetPairingAccountInfo",
            ),
            (
                commands::GET_ACCOUNT_NICKNAME,
                Some(Self::stub_handler),
                "GetAccountNickname",
            ),
            (
                commands::GET_ACCOUNT_STATE,
                Some(Self::stub_handler),
                "GetAccountState",
            ),
            (
                commands::REQUEST_POST_EVENTS,
                Some(Self::stub_handler),
                "RequestPostEvents",
            ),
            (
                commands::GET_POST_EVENT_INTERVAL,
                Some(Self::stub_handler),
                "GetPostEventInterval",
            ),
            (
                commands::SET_POST_EVENT_INTERVAL,
                Some(Self::stub_handler),
                "SetPostEventInterval",
            ),
            (
                commands::GET_SYNCHRONIZATION_EVENT,
                Some(Self::stub_handler),
                "GetSynchronizationEvent",
            ),
            (
                commands::START_PLAY_TIMER,
                Some(Self::start_play_timer_handler),
                "StartPlayTimer",
            ),
            (
                commands::STOP_PLAY_TIMER,
                Some(Self::stop_play_timer_handler),
                "StopPlayTimer",
            ),
            (
                commands::IS_PLAY_TIMER_ENABLED,
                Some(Self::is_play_timer_enabled_handler),
                "IsPlayTimerEnabled",
            ),
            (
                commands::GET_PLAY_TIMER_REMAINING_TIME,
                Some(Self::stub_handler),
                "GetPlayTimerRemainingTime",
            ),
            (
                commands::IS_RESTRICTED_BY_PLAY_TIMER,
                Some(Self::is_restricted_by_play_timer_handler),
                "IsRestrictedByPlayTimer",
            ),
            (
                commands::GET_PLAY_TIMER_SETTINGS,
                Some(Self::get_play_timer_settings_handler),
                "GetPlayTimerSettings",
            ),
            (
                commands::GET_PLAY_TIMER_EVENT_TO_REQUEST_SUSPENSION,
                Some(Self::get_play_timer_event_to_request_suspension_handler),
                "GetPlayTimerEventToRequestSuspension",
            ),
            (
                commands::IS_PLAY_TIMER_ALARM_DISABLED,
                Some(Self::is_play_timer_alarm_disabled_handler),
                "IsPlayTimerAlarmDisabled",
            ),
            (
                commands::GET_PLAY_TIMER_REMAINING_TIME_DISPLAY_INFO,
                Some(Self::get_play_timer_remaining_time_display_info_handler),
                "GetPlayTimerRemainingTimeDisplayInfo",
            ),
            (
                commands::UNKNOWN_1460,
                Some(Self::unknown_1460_handler),
                "Unknown1460",
            ),
            (
                commands::NOTIFY_WRONG_PIN_CODE_INPUT_MANY_TIMES,
                Some(Self::stub_handler),
                "NotifyWrongPinCodeInputManyTimes",
            ),
            (
                commands::CANCEL_NETWORK_REQUEST,
                Some(Self::stub_handler),
                "CancelNetworkRequest",
            ),
            (
                commands::GET_UNLINKED_EVENT,
                Some(Self::stub_handler),
                "GetUnlinkedEvent",
            ),
            (
                commands::CLEAR_UNLINKED_EVENT,
                Some(Self::stub_handler),
                "ClearUnlinkedEvent",
            ),
            (
                commands::DISABLE_ALL_FEATURES,
                Some(Self::stub_handler),
                "DisableAllFeatures",
            ),
            (
                commands::POST_ENABLE_ALL_FEATURES,
                Some(Self::stub_handler),
                "PostEnableAllFeatures",
            ),
            (
                commands::IS_ALL_FEATURES_DISABLED,
                Some(Self::stub_handler),
                "IsAllFeaturesDisabled",
            ),
            (
                commands::DELETE_FROM_FREE_COMMUNICATION_APPLICATION_LIST_FOR_DEBUG,
                Some(Self::stub_handler),
                "DeleteFromFreeCommunicationApplicationListForDebug",
            ),
            (
                commands::CLEAR_FREE_COMMUNICATION_APPLICATION_LIST_FOR_DEBUG,
                Some(Self::stub_handler),
                "ClearFreeCommunicationApplicationListForDebug",
            ),
            (
                commands::GET_EXEMPT_APPLICATION_LIST_COUNT_FOR_DEBUG,
                Some(Self::stub_handler),
                "GetExemptApplicationListCountForDebug",
            ),
            (
                commands::GET_EXEMPT_APPLICATION_LIST_FOR_DEBUG,
                Some(Self::stub_handler),
                "GetExemptApplicationListForDebug",
            ),
            (
                commands::UPDATE_EXEMPT_APPLICATION_LIST_FOR_DEBUG,
                Some(Self::stub_handler),
                "UpdateExemptApplicationListForDebug",
            ),
            (
                commands::ADD_TO_EXEMPT_APPLICATION_LIST_FOR_DEBUG,
                Some(Self::stub_handler),
                "AddToExemptApplicationListForDebug",
            ),
            (
                commands::DELETE_FROM_EXEMPT_APPLICATION_LIST_FOR_DEBUG,
                Some(Self::stub_handler),
                "DeleteFromExemptApplicationListForDebug",
            ),
            (
                commands::CLEAR_EXEMPT_APPLICATION_LIST_FOR_DEBUG,
                Some(Self::stub_handler),
                "ClearExemptApplicationListForDebug",
            ),
            (
                commands::DELETE_PAIRING,
                Some(Self::stub_handler),
                "DeletePairing",
            ),
            (
                commands::SET_PLAY_TIMER_SETTINGS_FOR_DEBUG,
                Some(Self::stub_handler),
                "SetPlayTimerSettingsForDebug",
            ),
            (
                commands::GET_PLAY_TIMER_SPENT_TIME_FOR_TEST,
                Some(Self::stub_handler),
                "GetPlayTimerSpentTimeForTest",
            ),
            (
                commands::SET_PLAY_TIMER_ALARM_DISABLED_FOR_DEBUG,
                Some(Self::stub_handler),
                "SetPlayTimerAlarmDisabledForDebug",
            ),
            (
                commands::REQUEST_PAIRING_ASYNC,
                Some(Self::stub_handler),
                "RequestPairingAsync",
            ),
            (
                commands::FINISH_REQUEST_PAIRING,
                Some(Self::stub_handler),
                "FinishRequestPairing",
            ),
            (
                commands::AUTHORIZE_PAIRING_ASYNC,
                Some(Self::stub_handler),
                "AuthorizePairingAsync",
            ),
            (
                commands::FINISH_AUTHORIZE_PAIRING,
                Some(Self::stub_handler),
                "FinishAuthorizePairing",
            ),
            (
                commands::RETRIEVE_PAIRING_INFO_ASYNC,
                Some(Self::stub_handler),
                "RetrievePairingInfoAsync",
            ),
            (
                commands::FINISH_RETRIEVE_PAIRING_INFO,
                Some(Self::stub_handler),
                "FinishRetrievePairingInfo",
            ),
            (
                commands::UNLINK_PAIRING_ASYNC,
                Some(Self::stub_handler),
                "UnlinkPairingAsync",
            ),
            (
                commands::FINISH_UNLINK_PAIRING,
                Some(Self::stub_handler),
                "FinishUnlinkPairing",
            ),
            (
                commands::GET_ACCOUNT_MII_IMAGE_ASYNC,
                Some(Self::stub_handler),
                "GetAccountMiiImageAsync",
            ),
            (
                commands::FINISH_GET_ACCOUNT_MII_IMAGE,
                Some(Self::stub_handler),
                "FinishGetAccountMiiImage",
            ),
            (
                commands::GET_ACCOUNT_MII_IMAGE_CONTENT_TYPE_ASYNC,
                Some(Self::stub_handler),
                "GetAccountMiiImageContentTypeAsync",
            ),
            (
                commands::FINISH_GET_ACCOUNT_MII_IMAGE_CONTENT_TYPE,
                Some(Self::stub_handler),
                "FinishGetAccountMiiImageContentType",
            ),
            (
                commands::SYNCHRONIZE_PARENTAL_CONTROL_SETTINGS_ASYNC,
                Some(Self::stub_handler),
                "SynchronizeParentalControlSettingsAsync",
            ),
            (
                commands::FINISH_SYNCHRONIZE_PARENTAL_CONTROL_SETTINGS,
                Some(Self::stub_handler),
                "FinishSynchronizeParentalControlSettings",
            ),
            (
                commands::FINISH_SYNCHRONIZE_PARENTAL_CONTROL_SETTINGS_WITH_LAST_UPDATED,
                Some(Self::stub_handler),
                "FinishSynchronizeParentalControlSettingsWithLastUpdated",
            ),
            (
                commands::REQUEST_UPDATE_EXEMPTION_LIST_ASYNC,
                Some(Self::stub_handler),
                "RequestUpdateExemptionListAsync",
            ),
        ]);
        Self {
            system,
            states: States::default(),
            settings: ParentalControlSettings::default(),
            restriction_settings: RestrictionSettings::default(),
            pin_code: [0u8; 8],
            request_suspension_event: Event::new(),
            capability,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// CheckFreeCommunicationPermissionImpl — internal helper.
    ///
    /// Corresponds to upstream `CheckFreeCommunicationPermissionImpl`.
    fn check_free_communication_permission_impl(&self) -> bool {
        if self.states.temporary_unlocked {
            return true;
        }
        if (self.states.application_info.parental_control_flag & 1) == 0 {
            return true;
        }
        if self.pin_code[0] == 0 {
            return true;
        }
        if !self.settings.is_free_communication_default_on {
            return true;
        }
        // Upstream TODO(ogniK): Check for blacklisted/exempted applications. Return false can
        // happen here but as we don't have multiprocess support yet, we can just assume our
        // application is valid for the time being.
        true
    }

    /// ConfirmStereoVisionPermissionImpl — internal helper.
    ///
    /// Corresponds to upstream `ConfirmStereoVisionPermissionImpl`.
    fn confirm_stereo_vision_permission_impl(&self) -> bool {
        if self.states.temporary_unlocked {
            return true;
        }
        if self.pin_code[0] == 0 {
            return true;
        }
        if !self.settings.is_stero_vision_restricted {
            return false;
        }
        true
    }

    /// SetStereoVisionRestrictionImpl — internal helper.
    ///
    /// Corresponds to upstream `SetStereoVisionRestrictionImpl`.
    fn set_stereo_vision_restriction_impl(&mut self, is_restricted: bool) {
        if self.settings.disabled {
            return;
        }
        if self.pin_code[0] == 0 {
            return;
        }
        self.settings.is_stero_vision_restricted = is_restricted;
    }

    /// Initialize (cmd 1).
    ///
    /// Corresponds to upstream `IParentalControlService::Initialize`.
    pub fn initialize(&mut self) -> Result<(), ResultCode> {
        log::debug!("IParentalControlService::Initialize called");

        if !self
            .capability
            .intersects(Capability::APPLICATION | Capability::SYSTEM)
        {
            log::error!(
                "Invalid capability! capability={:#x}",
                self.capability.bits()
            );
            return Err(RESULT_NO_CAPABILITY);
        }

        // Upstream TODO(ogniK): Recovery flag initialization for pctl:r

        // Upstream reads program_id via system.GetApplicationProcessProgramID(), then uses
        // PatchManager::GetControlMetadata() to populate application_info with age_rating and
        // parental_control_flag from the NACP. PatchManager is not yet ported, so we populate
        // application_info with the program_id but leave age_rating/parental_control_flag at
        // defaults (zeroed), which effectively disables parental restrictions.
        let program_id = if !self.system.is_null() {
            self.system.get().runtime_program_id()
        } else {
            0
        };
        if program_id != 0 {
            self.states.application_info = ApplicationInfo {
                application_id: program_id,
                age_rating: [0u8; 32],
                parental_control_flag: 0,
                capability: self.capability.bits(),
            };
        }
        self.states.tid_from_event = 0;
        self.states.launch_time_valid = false;
        self.states.is_suspended = false;
        self.states.free_communication = false;
        self.states.stereo_vision = false;

        Ok(())
    }

    /// CheckFreeCommunicationPermission (cmd 1001).
    ///
    /// Corresponds to upstream `IParentalControlService::CheckFreeCommunicationPermission`.
    pub fn check_free_communication_permission(&mut self) -> Result<(), ResultCode> {
        log::debug!("IParentalControlService::CheckFreeCommunicationPermission called");

        if !self.check_free_communication_permission_impl() {
            return Err(RESULT_NO_FREE_COMMUNICATION);
        }
        self.states.free_communication = true;
        Ok(())
    }

    /// ConfirmLaunchApplicationPermission (cmd 1002).
    ///
    /// Corresponds to upstream `IParentalControlService::ConfirmLaunchApplicationPermission`.
    pub fn confirm_launch_application_permission(
        &self,
        _restriction_bitset: &[u8],
        nacp_flag: u64,
        application_id: u64,
    ) -> Result<(), ResultCode> {
        log::warn!(
            "(STUBBED) ConfirmLaunchApplicationPermission called, nacp_flag={:#x} application_id={:016x}",
            nacp_flag,
            application_id,
        );
        Ok(())
    }

    /// ConfirmResumeApplicationPermission (cmd 1003).
    ///
    /// Corresponds to upstream `IParentalControlService::ConfirmResumeApplicationPermission`.
    pub fn confirm_resume_application_permission(
        &self,
        _restriction_bitset: &[u8],
        nacp_flag: u64,
        application_id: u64,
    ) -> Result<(), ResultCode> {
        log::warn!(
            "(STUBBED) ConfirmResumeApplicationPermission called, nacp_flag={:#x} application_id={:016x}",
            nacp_flag,
            application_id,
        );
        Ok(())
    }

    /// ConfirmSnsPostPermission (cmd 1004).
    ///
    /// Corresponds to upstream `IParentalControlService::ConfirmSnsPostPermission`.
    pub fn confirm_sns_post_permission(&self) -> Result<(), ResultCode> {
        log::warn!("(STUBBED) ConfirmSnsPostPermission called");
        Err(RESULT_NO_FREE_COMMUNICATION)
    }

    /// IsRestrictionTemporaryUnlocked (cmd 1006).
    ///
    /// Corresponds to upstream `IParentalControlService::IsRestrictionTemporaryUnlocked`.
    pub fn is_restriction_temporary_unlocked(&self) -> Result<bool, ResultCode> {
        log::warn!("(STUBBED) IsRestrictionTemporaryUnlocked called");
        Ok(false)
    }

    /// IsRestrictedSystemSettingsEntered (cmd 1010).
    ///
    /// Corresponds to upstream `IParentalControlService::IsRestrictedSystemSettingsEntered`.
    pub fn is_restricted_system_settings_entered(&self) -> Result<bool, ResultCode> {
        log::warn!("(STUBBED) IsRestrictedSystemSettingsEntered called");
        Ok(false)
    }

    /// ConfirmStereoVisionPermission (cmd 1013).
    ///
    /// Corresponds to upstream `IParentalControlService::ConfirmStereoVisionPermission`.
    pub fn confirm_stereo_vision_permission(&mut self) -> Result<(), ResultCode> {
        log::debug!("IParentalControlService::ConfirmStereoVisionPermission called");
        self.states.stereo_vision = true;
        Ok(())
    }

    /// EndFreeCommunication (cmd 1017).
    ///
    /// Corresponds to upstream `IParentalControlService::EndFreeCommunication`.
    pub fn end_free_communication(&self) -> Result<(), ResultCode> {
        log::warn!("(STUBBED) EndFreeCommunication called");
        Ok(())
    }

    /// IsFreeCommunicationAvailable (cmd 1018).
    ///
    /// Corresponds to upstream `IParentalControlService::IsFreeCommunicationAvailable`.
    pub fn is_free_communication_available(&self) -> Result<(), ResultCode> {
        log::warn!("(STUBBED) IsFreeCommunicationAvailable called");
        if !self.check_free_communication_permission_impl() {
            return Err(RESULT_NO_FREE_COMMUNICATION);
        }
        Ok(())
    }

    /// IsRestrictionEnabled (cmd 1031).
    ///
    /// Corresponds to upstream `IParentalControlService::IsRestrictionEnabled`.
    pub fn is_restriction_enabled(&self) -> Result<bool, ResultCode> {
        log::debug!("IParentalControlService::IsRestrictionEnabled called");

        if !self
            .capability
            .intersects(Capability::STATUS | Capability::RECOVERY)
        {
            log::error!("Application does not have Status or Recovery capabilities!");
            return Err(RESULT_NO_CAPABILITY);
        }

        Ok(self.pin_code[0] != 0)
    }

    /// GetSafetyLevel (cmd 1032).
    ///
    /// Corresponds to upstream `IParentalControlService::GetSafetyLevel`.
    pub fn get_safety_level(&self) -> Result<u32, ResultCode> {
        log::warn!("(STUBBED) GetSafetyLevel called");
        Ok(0)
    }

    /// GetCurrentSettings (cmd 1035).
    ///
    /// Corresponds to upstream `IParentalControlService::GetCurrentSettings`.
    pub fn get_current_settings(&self) -> Result<RestrictionSettings, ResultCode> {
        log::info!("IParentalControlService::GetCurrentSettings called");
        Ok(self.restriction_settings)
    }

    /// GetFreeCommunicationApplicationListCount (cmd 1039).
    ///
    /// Corresponds to upstream `IParentalControlService::GetFreeCommunicationApplicationListCount`.
    pub fn get_free_communication_application_list_count(&self) -> Result<i32, ResultCode> {
        log::warn!("(STUBBED) GetFreeCommunicationApplicationListCount called");
        Ok(4)
    }

    /// ConfirmStereoVisionRestrictionConfigurable (cmd 1061).
    ///
    /// Corresponds to upstream `IParentalControlService::ConfirmStereoVisionRestrictionConfigurable`.
    pub fn confirm_stereo_vision_restriction_configurable(&self) -> Result<(), ResultCode> {
        log::debug!("IParentalControlService::ConfirmStereoVisionRestrictionConfigurable called");

        if !self.capability.contains(Capability::STEREO_VISION) {
            log::error!("Application does not have StereoVision capability!");
            return Err(RESULT_NO_CAPABILITY);
        }

        if self.pin_code[0] == 0 {
            return Err(RESULT_NO_RESTRICTION_ENABLED);
        }

        Ok(())
    }

    /// GetStereoVisionRestriction (cmd 1062).
    ///
    /// Corresponds to upstream `IParentalControlService::GetStereoVisionRestriction`.
    pub fn get_stereo_vision_restriction(&self) -> Result<bool, ResultCode> {
        log::debug!("IParentalControlService::GetStereoVisionRestriction called");

        if !self.capability.contains(Capability::STEREO_VISION) {
            log::error!("Application does not have StereoVision capability!");
            return Err(RESULT_NO_CAPABILITY);
        }

        Ok(self.settings.is_stero_vision_restricted)
    }

    /// SetStereoVisionRestriction (cmd 1063).
    ///
    /// Corresponds to upstream `IParentalControlService::SetStereoVisionRestriction`.
    pub fn set_stereo_vision_restriction(
        &mut self,
        stereo_vision_restriction: bool,
    ) -> Result<(), ResultCode> {
        log::debug!(
            "IParentalControlService::SetStereoVisionRestriction called, can_use={}",
            stereo_vision_restriction,
        );

        if !self.capability.contains(Capability::STEREO_VISION) {
            log::error!("Application does not have StereoVision capability!");
            return Err(RESULT_NO_CAPABILITY);
        }

        self.set_stereo_vision_restriction_impl(stereo_vision_restriction);
        Ok(())
    }

    /// ResetConfirmedStereoVisionPermission (cmd 1064).
    ///
    /// Corresponds to upstream `IParentalControlService::ResetConfirmedStereoVisionPermission`.
    pub fn reset_confirmed_stereo_vision_permission(&mut self) -> Result<(), ResultCode> {
        log::debug!("IParentalControlService::ResetConfirmedStereoVisionPermission called");
        self.states.stereo_vision = false;
        Ok(())
    }

    /// IsStereoVisionPermitted (cmd 1065).
    ///
    /// Corresponds to upstream `IParentalControlService::IsStereoVisionPermitted`.
    pub fn is_stereo_vision_permitted(&self) -> Result<bool, ResultCode> {
        log::debug!("IParentalControlService::IsStereoVisionPermitted called");

        if !self.confirm_stereo_vision_permission_impl() {
            return Err(RESULT_STEREO_VISION_RESTRICTED);
        }
        Ok(true)
    }

    /// GetPinCodeLength (cmd 1206).
    ///
    /// Corresponds to upstream `IParentalControlService::GetPinCodeLength`.
    pub fn get_pin_code_length(&self) -> Result<i32, ResultCode> {
        log::warn!("(STUBBED) GetPinCodeLength called");
        Ok(0)
    }

    /// IsPairingActive (cmd 1403).
    ///
    /// Corresponds to upstream `IParentalControlService::IsPairingActive`.
    pub fn is_pairing_active(&self) -> Result<bool, ResultCode> {
        log::warn!("(STUBBED) IsPairingActive called");
        Ok(false)
    }

    /// StartPlayTimer (cmd 1451).
    ///
    /// Corresponds to upstream `IParentalControlService::StartPlayTimer`.
    pub fn start_play_timer(&self) -> Result<(), ResultCode> {
        log::warn!("(STUBBED) StartPlayTimer called");
        Ok(())
    }

    /// StopPlayTimer (cmd 1452).
    ///
    /// Corresponds to upstream `IParentalControlService::StopPlayTimer`.
    pub fn stop_play_timer(&self) -> Result<(), ResultCode> {
        log::warn!("(STUBBED) StopPlayTimer called");
        Ok(())
    }

    /// IsPlayTimerEnabled (cmd 1453).
    ///
    /// Corresponds to upstream `IParentalControlService::IsPlayTimerEnabled`.
    pub fn is_play_timer_enabled(&self) -> Result<bool, ResultCode> {
        log::warn!("(STUBBED) IsPlayTimerEnabled called");
        Ok(false)
    }

    /// IsRestrictedByPlayTimer (cmd 1455).
    ///
    /// Corresponds to upstream `IParentalControlService::IsRestrictedByPlayTimer`.
    pub fn is_restricted_by_play_timer(&self) -> Result<bool, ResultCode> {
        log::warn!("(STUBBED) IsRestrictedByPlayTimer called");
        Ok(false)
    }

    /// GetPlayTimerSettings (cmd 1456).
    ///
    /// Corresponds to upstream `IParentalControlService::GetPlayTimerSettings`.
    pub fn get_play_timer_settings(&self) -> Result<PlayTimerSettings, ResultCode> {
        log::warn!("(STUBBED) GetPlayTimerSettings called");
        Ok(PlayTimerSettings::default())
    }

    /// IsPlayTimerAlarmDisabled (cmd 1458).
    ///
    /// Corresponds to upstream `IParentalControlService::IsPlayTimerAlarmDisabled`.
    pub fn is_play_timer_alarm_disabled(&self) -> Result<bool, ResultCode> {
        log::info!("IsPlayTimerAlarmDisabled called");
        Ok(false)
    }

    fn initialize_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IParentalControlService) };
        let result = if service
            .capability
            .intersects(Capability::APPLICATION | Capability::SYSTEM)
        {
            RESULT_SUCCESS
        } else {
            RESULT_NO_CAPABILITY
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn check_free_communication_permission_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IParentalControlService) };
        let result = if service.check_free_communication_permission_impl() {
            RESULT_SUCCESS
        } else {
            RESULT_NO_FREE_COMMUNICATION
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn confirm_stereo_vision_permission_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IParentalControlService) };
        let result = if service.confirm_stereo_vision_permission_impl() {
            RESULT_SUCCESS
        } else {
            RESULT_STEREO_VISION_RESTRICTED
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn is_restriction_enabled_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IParentalControlService) };
        let (result, value) = match service.is_restriction_enabled() {
            Ok(value) => (RESULT_SUCCESS, value),
            Err(err) => (err, false),
        };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_bool(value);
    }

    fn is_restriction_temporary_unlocked_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IParentalControlService) };
        let (result, value) = match service.is_restriction_temporary_unlocked() {
            Ok(value) => (RESULT_SUCCESS, value),
            Err(err) => (err, false),
        };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_bool(value);
    }

    fn is_restricted_system_settings_entered_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IParentalControlService) };
        let (result, value) = match service.is_restricted_system_settings_entered() {
            Ok(value) => (RESULT_SUCCESS, value),
            Err(err) => (err, false),
        };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_bool(value);
    }

    fn confirm_launch_application_permission_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) IParentalControlService::ConfirmLaunchApplicationPermission called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn confirm_resume_application_permission_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) IParentalControlService::ConfirmResumeApplicationPermission called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn confirm_sns_post_permission_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) IParentalControlService::ConfirmSnsPostPermission called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_NO_FREE_COMMUNICATION);
    }

    fn end_free_communication_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) IParentalControlService::EndFreeCommunication called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn is_free_communication_available_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IParentalControlService) };
        let result = service
            .is_free_communication_available()
            .err()
            .unwrap_or(RESULT_SUCCESS);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn get_safety_level_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) IParentalControlService::GetSafetyLevel called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(0);
    }

    fn get_current_settings_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IParentalControlService) };
        log::info!("IParentalControlService::GetCurrentSettings called");
        let settings = service.restriction_settings;
        // RestrictionSettings is 3 bytes (u8 + bool + bool), padded to 1 data word
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        let raw: u32 = (settings.rating_age as u32)
            | ((settings.sns_post_restriction as u32) << 8)
            | ((settings.free_communication_restriction as u32) << 16);
        rb.push_u32(raw);
    }

    fn get_free_communication_application_list_count_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!(
            "(STUBBED) IParentalControlService::GetFreeCommunicationApplicationListCount called"
        );
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(4);
    }

    fn confirm_stereo_vision_restriction_configurable_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IParentalControlService) };
        let result = match service.confirm_stereo_vision_restriction_configurable() {
            Ok(()) => RESULT_SUCCESS,
            Err(err) => err,
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn get_stereo_vision_restriction_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IParentalControlService) };
        let (result, value) = match service.get_stereo_vision_restriction() {
            Ok(value) => (RESULT_SUCCESS, value),
            Err(err) => (err, false),
        };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_bool(value);
    }

    fn set_stereo_vision_restriction_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &mut *(this as *const dyn ServiceFramework as *mut IParentalControlService) };
        let mut rp = RequestParser::new(ctx);
        let is_restricted = rp.pop_bool();
        let result = match service.set_stereo_vision_restriction(is_restricted) {
            Ok(()) => RESULT_SUCCESS,
            Err(err) => err,
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn reset_confirmed_stereo_vision_permission_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &mut *(this as *const dyn ServiceFramework as *mut IParentalControlService) };
        log::debug!("IParentalControlService::ResetConfirmedStereoVisionPermission called");
        service.states.stereo_vision = false;
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn is_stereo_vision_permitted_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IParentalControlService) };
        let (result, value) = match service.is_stereo_vision_permitted() {
            Ok(value) => (RESULT_SUCCESS, value),
            Err(err) => (err, false),
        };
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(result);
        rb.push_bool(value);
    }

    fn get_pin_code_length_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) IParentalControlService::GetPinCodeLength called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(0);
    }

    fn is_pairing_active_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) IParentalControlService::IsPairingActive called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(false);
    }

    fn start_play_timer_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) IParentalControlService::StartPlayTimer called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn stop_play_timer_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) IParentalControlService::StopPlayTimer called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn is_play_timer_enabled_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) IParentalControlService::IsPlayTimerEnabled called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(false);
    }

    fn is_restricted_by_play_timer_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) IParentalControlService::IsRestrictedByPlayTimer called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(false);
    }

    fn get_play_timer_settings_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) IParentalControlService::GetPlayTimerSettings called");
        let settings = PlayTimerSettings::default();
        // PlayTimerSettings is 0x34 bytes = 13 u32 words. Response: 2 (header) + 13 (data) = 15.
        let mut rb = ResponseBuilder::new(ctx, 15, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw(&settings);
    }

    fn is_play_timer_alarm_disabled_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::info!("IParentalControlService::IsPlayTimerAlarmDisabled called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(false);
    }

    fn get_play_timer_remaining_time_display_info_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::debug!("IParentalControlService::GetPlayTimerRemainingTimeDisplayInfo called");
        let info = PlayTimerRemainingTimeDisplayInfo::default();
        // 0x18 bytes = 6 u32 words. Response: 2 (header) + 6 (data) = 8.
        let mut rb = ResponseBuilder::new(ctx, 8, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw(&info);
    }

    fn unknown_1460_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let in_unk = rp.pop_u8();
        log::debug!("IParentalControlService::Unknown1460 called, in_unk={in_unk}");
        let info = PlayTimerRemainingTimeDisplayInfo::default();
        let mut rb = ResponseBuilder::new(ctx, 8, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw(&info);
    }
}

impl SessionRequestHandler for IParentalControlService {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }
}

impl ServiceFramework for IParentalControlService {
    fn get_service_name(&self) -> &str {
        "IParentalControlService"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
