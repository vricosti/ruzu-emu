// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ns/application_manager_interface.cpp/.h
//!
//! IApplicationManagerInterface - the largest NS service interface.
//! Most commands are stubbed; the implemented ones delegate to other NS interfaces.

use super::ns_types::*;
use crate::core::SystemRef;
use crate::file_sys::romfs_factory::StorageId;
use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::os::event::Event;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use std::collections::BTreeMap;

/// IPC command table for IApplicationManagerInterface.
/// Entries with `true` are implemented upstream; `false` means nullptr/stub.
pub const IAPPLICATION_MANAGER_INTERFACE_COMMANDS: &[(u32, bool, &str)] = &[
    (0, true, "ListApplicationRecord"),
    (1, false, "GenerateApplicationRecordCount"),
    (2, true, "GetApplicationRecordUpdateSystemEvent"),
    (3, false, "GetApplicationViewDeprecated"),
    (4, false, "DeleteApplicationEntity"),
    (5, false, "DeleteApplicationCompletely"),
    (6, false, "IsAnyApplicationEntityRedundant"),
    (7, false, "DeleteRedundantApplicationEntity"),
    (8, false, "IsApplicationEntityMovable"),
    (9, false, "MoveApplicationEntity"),
    (11, false, "CalculateApplicationOccupiedSize"),
    (16, false, "PushApplicationRecord"),
    (17, false, "ListApplicationRecordContentMeta"),
    (19, false, "LaunchApplicationOld"),
    (21, false, "GetApplicationContentPath"),
    (22, false, "TerminateApplication"),
    (23, false, "ResolveApplicationContentPath"),
    (26, false, "BeginInstallApplication"),
    (27, false, "DeleteApplicationRecord"),
    (30, false, "RequestApplicationUpdateInfo"),
    (31, false, "Unknown31"),
    (32, false, "CancelApplicationDownload"),
    (33, false, "ResumeApplicationDownload"),
    (35, false, "UpdateVersionList"),
    (36, false, "PushLaunchVersion"),
    (37, false, "ListRequiredVersion"),
    (38, true, "CheckApplicationLaunchVersion"),
    (39, false, "CheckApplicationLaunchRights"),
    (40, false, "GetApplicationLogoData"),
    (41, false, "CalculateApplicationDownloadRequiredSize"),
    (42, false, "CleanupSdCard"),
    (43, true, "CheckSdCardMountStatus"),
    (44, true, "GetSdCardMountStatusChangedEvent"),
    (45, false, "GetGameCardAttachmentEvent"),
    (46, false, "GetGameCardAttachmentInfo"),
    (47, false, "GetTotalSpaceSize"),
    (48, true, "GetFreeSpaceSize"),
    (49, false, "GetSdCardRemovedEvent"),
    (52, true, "GetGameCardUpdateDetectionEvent"),
    (53, false, "DisableApplicationAutoDelete"),
    (54, false, "EnableApplicationAutoDelete"),
    (55, true, "GetApplicationDesiredLanguage"),
    (56, false, "SetApplicationTerminateResult"),
    (57, false, "ClearApplicationTerminateResult"),
    (58, false, "GetLastSdCardMountUnexpectedResult"),
    (59, true, "ConvertApplicationLanguageToLanguageCode"),
    (60, false, "ConvertLanguageCodeToApplicationLanguage"),
    (61, false, "GetBackgroundDownloadStressTaskInfo"),
    (62, false, "GetGameCardStopper"),
    (63, false, "IsSystemProgramInstalled"),
    (64, false, "StartApplyDeltaTask"),
    (65, false, "GetRequestServerStopper"),
    (66, false, "GetBackgroundApplyDeltaStressTaskInfo"),
    (67, false, "CancelApplicationApplyDelta"),
    (68, false, "ResumeApplicationApplyDelta"),
    (69, false, "CalculateApplicationApplyDeltaRequiredSize"),
    (70, true, "ResumeAll"),
    (71, true, "GetStorageSize"),
    (80, false, "RequestDownloadApplication"),
    (81, false, "RequestDownloadAddOnContent"),
    (82, false, "DownloadApplication"),
    (83, false, "CheckApplicationResumeRights"),
    (84, false, "GetDynamicCommitEvent"),
    (85, false, "RequestUpdateApplication2"),
    (86, false, "EnableApplicationCrashReport"),
    (87, false, "IsApplicationCrashReportEnabled"),
    (90, false, "BoostSystemMemoryResourceLimit"),
    (91, false, "DeprecatedLaunchApplication"),
    (92, false, "GetRunningApplicationProgramId"),
    (93, false, "GetMainApplicationProgramIndex"),
    (94, false, "LaunchApplication"),
    (95, false, "GetApplicationLaunchInfo"),
    (96, false, "AcquireApplicationLaunchInfo"),
    (
        97,
        false,
        "GetMainApplicationProgramIndexByApplicationLaunchInfo",
    ),
    (98, false, "EnableApplicationAllThreadDumpOnCrash"),
    (99, false, "LaunchDevMenu"),
    (100, false, "ResetToFactorySettings"),
    (101, false, "ResetToFactorySettingsWithoutUserSaveData"),
    (102, false, "ResetToFactorySettingsForRefurbishment"),
    (103, false, "ResetToFactorySettingsWithPlatformRegion"),
    (
        104,
        false,
        "ResetToFactorySettingsWithPlatformRegionAuthentication",
    ),
    (105, false, "RequestResetToFactorySettingsSecurely"),
    (
        106,
        false,
        "RequestResetToFactorySettingsWithPlatformRegionAuthenticationSecurely",
    ),
    (200, false, "CalculateUserSaveDataStatistics"),
    (201, false, "DeleteUserSaveDataAll"),
    (210, false, "DeleteUserSystemSaveData"),
    (211, false, "DeleteSaveData"),
    (220, false, "UnregisterNetworkServiceAccount"),
    (
        221,
        false,
        "UnregisterNetworkServiceAccountWithUserSaveDataDeletion",
    ),
    (300, false, "GetApplicationShellEvent"),
    (301, false, "PopApplicationShellEventInfo"),
    (302, false, "LaunchLibraryApplet"),
    (303, false, "TerminateLibraryApplet"),
    (304, false, "LaunchSystemApplet"),
    (305, false, "TerminateSystemApplet"),
    (306, false, "LaunchOverlayApplet"),
    (307, false, "TerminateOverlayApplet"),
    (400, true, "GetApplicationControlData"),
    (401, false, "InvalidateAllApplicationControlCache"),
    (402, false, "RequestDownloadApplicationControlData"),
    (403, false, "GetMaxApplicationControlCacheCount"),
    (404, false, "InvalidateApplicationControlCache"),
    (405, false, "ListApplicationControlCacheEntryInfo"),
    (406, false, "GetApplicationControlProperty"),
    (407, false, "ListApplicationTitle"),
    (408, false, "ListApplicationIcon"),
    (502, false, "RequestCheckGameCardRegistration"),
    (503, false, "RequestGameCardRegistrationGoldPoint"),
    (504, false, "RequestRegisterGameCard"),
    (505, true, "GetGameCardMountFailureEvent"),
    (506, false, "IsGameCardInserted"),
    (507, false, "EnsureGameCardAccess"),
    (508, false, "GetLastGameCardMountFailureResult"),
    (509, false, "ListApplicationIdOnGameCard"),
    (510, false, "GetGameCardPlatformRegion"),
    (511, true, "GetGameCardWakenReadyEvent"),
    (600, false, "CountApplicationContentMeta"),
    (601, false, "ListApplicationContentMetaStatus"),
    (602, false, "ListAvailableAddOnContent"),
    (603, false, "GetOwnedApplicationContentMetaStatus"),
    (604, false, "RegisterContentsExternalKey"),
    (
        605,
        false,
        "ListApplicationContentMetaStatusWithRightsCheck",
    ),
    (606, false, "GetContentMetaStorage"),
    (607, false, "ListAvailableAddOnContent"),
    (609, false, "ListAvailabilityAssuredAddOnContent"),
    (610, false, "GetInstalledContentMetaStorage"),
    (611, false, "PrepareAddOnContent"),
    (700, false, "PushDownloadTaskList"),
    (701, false, "ClearTaskStatusList"),
    (702, false, "RequestDownloadTaskList"),
    (703, false, "RequestEnsureDownloadTask"),
    (704, false, "ListDownloadTaskStatus"),
    (705, false, "RequestDownloadTaskListData"),
    (800, false, "RequestVersionList"),
    (801, false, "ListVersionList"),
    (802, false, "RequestVersionListData"),
    (900, false, "GetApplicationRecord"),
    (901, false, "GetApplicationRecordProperty"),
    (902, false, "EnableApplicationAutoUpdate"),
    (903, false, "DisableApplicationAutoUpdate"),
    (904, false, "TouchApplication"),
    (905, false, "RequestApplicationUpdate"),
    (906, true, "IsApplicationUpdateRequested"),
    (907, false, "WithdrawApplicationUpdateRequest"),
    (908, false, "ListApplicationRecordInstalledContentMeta"),
    (
        909,
        false,
        "WithdrawCleanupAddOnContentsWithNoRightsRecommendation",
    ),
    (910, false, "HasApplicationRecord"),
    (911, false, "SetPreInstalledApplication"),
    (912, false, "ClearPreInstalledApplicationFlag"),
    (913, false, "ListAllApplicationRecord"),
    (914, false, "HideApplicationRecord"),
    (915, false, "ShowApplicationRecord"),
    (916, false, "IsApplicationAutoDeleteDisabled"),
    (1000, false, "RequestVerifyApplicationDeprecated"),
    (1001, false, "CorruptApplicationForDebug"),
    (1002, false, "RequestVerifyAddOnContentsRights"),
    (1003, false, "RequestVerifyApplication"),
    (1004, false, "CorruptContentForDebug"),
    (1200, false, "NeedsUpdateVulnerability"),
    (1300, true, "IsAnyApplicationEntityInstalled"),
    (1301, false, "DeleteApplicationContentEntities"),
    (1302, false, "CleanupUnrecordedApplicationEntity"),
    (1303, false, "CleanupAddOnContentsWithNoRights"),
    (1304, false, "DeleteApplicationContentEntity"),
    (1305, false, "TryDeleteRunningApplicationEntity"),
    (1306, false, "TryDeleteRunningApplicationCompletely"),
    (1307, false, "TryDeleteRunningApplicationContentEntities"),
    (1308, false, "DeleteApplicationCompletelyForDebug"),
    (1309, false, "CleanupUnavailableAddOnContents"),
    (1310, false, "RequestMoveApplicationEntity"),
    (1311, false, "EstimateSizeToMove"),
    (1312, false, "HasMovableEntity"),
    (1313, false, "CleanupOrphanContents"),
    (1314, false, "CheckPreconditionSatisfiedToMove"),
    (1400, false, "PrepareShutdown"),
    (1500, false, "FormatSdCard"),
    (1501, false, "NeedsSystemUpdateToFormatSdCard"),
    (1502, false, "GetLastSdCardFormatUnexpectedResult"),
    (1504, false, "InsertSdCard"),
    (1505, false, "RemoveSdCard"),
    (1506, false, "GetSdCardStartupStatus"),
    (1600, false, "GetSystemSeedForPseudoDeviceId"),
    (1601, false, "ResetSystemSeedForPseudoDeviceId"),
    (1700, false, "ListApplicationDownloadingContentMeta"),
    (1701, true, "GetApplicationView"),
    (1702, false, "GetApplicationDownloadTaskStatus"),
    (1703, false, "GetApplicationViewDownloadErrorContext"),
    (1704, true, "GetApplicationViewWithPromotionInfo"),
    (1705, false, "IsPatchAutoDeletableApplication"),
    (1800, false, "IsNotificationSetupCompleted"),
    (1801, false, "GetLastNotificationInfoCount"),
    (1802, false, "ListLastNotificationInfo"),
    (1803, false, "ListNotificationTask"),
    (1900, false, "IsActiveAccount"),
    (1901, false, "RequestDownloadApplicationPrepurchasedRights"),
    (1902, false, "GetApplicationTicketInfo"),
    (
        1903,
        false,
        "RequestDownloadApplicationPrepurchasedRightsForAccount",
    ),
    (2000, false, "GetSystemDeliveryInfo"),
    (2001, false, "SelectLatestSystemDeliveryInfo"),
    (2002, false, "VerifyDeliveryProtocolVersion"),
    (2003, false, "GetApplicationDeliveryInfo"),
    (2004, false, "HasAllContentsToDeliver"),
    (2005, false, "CompareApplicationDeliveryInfo"),
    (2006, false, "CanDeliverApplication"),
    (2007, false, "ListContentMetaKeyToDeliverApplication"),
    (2008, false, "NeedsSystemUpdateToDeliverApplication"),
    (2009, false, "EstimateRequiredSize"),
    (2010, false, "RequestReceiveApplication"),
    (2011, false, "CommitReceiveApplication"),
    (2012, false, "GetReceiveApplicationProgress"),
    (2013, false, "RequestSendApplication"),
    (2014, false, "GetSendApplicationProgress"),
    (2015, false, "CompareSystemDeliveryInfo"),
    (2016, false, "ListNotCommittedContentMeta"),
    (2017, false, "CreateDownloadTask"),
    (2018, false, "GetApplicationDeliveryInfoHash"),
    (2050, true, "GetApplicationRightsOnClient"),
    (2051, false, "InvalidateRightsIdCache"),
    (2100, true, "GetApplicationTerminateResult"),
    (2101, false, "GetRawApplicationTerminateResult"),
    (2150, false, "CreateRightsEnvironment"),
    (2151, false, "DestroyRightsEnvironment"),
    (2152, false, "ActivateRightsEnvironment"),
    (2153, false, "DeactivateRightsEnvironment"),
    (2154, false, "ForceActivateRightsContextForExit"),
    (2155, false, "UpdateRightsEnvironmentStatus"),
    (2156, false, "CreateRightsEnvironmentForMicroApplication"),
    (2160, false, "AddTargetApplicationToRightsEnvironment"),
    (2161, false, "SetUsersToRightsEnvironment"),
    (2170, false, "GetRightsEnvironmentStatus"),
    (2171, false, "GetRightsEnvironmentStatusChangedEvent"),
    (2180, false, "RequestExtendRightsInRightsEnvironment"),
    (2181, false, "GetResultOfExtendRightsInRightsEnvironment"),
    (
        2182,
        false,
        "SetActiveRightsContextUsingStateToRightsEnvironment",
    ),
    (2190, false, "GetRightsEnvironmentHandleForApplication"),
    (2199, false, "GetRightsEnvironmentCountForDebug"),
    (2200, false, "GetGameCardApplicationCopyIdentifier"),
    (2201, false, "GetInstalledApplicationCopyIdentifier"),
    (2250, false, "RequestReportActiveELicence"),
    (2300, false, "ListEventLog"),
    (2350, false, "PerformAutoUpdateByApplicationId"),
    (2351, false, "RequestNoDownloadRightsErrorResolution"),
    (2352, false, "RequestResolveNoDownloadRightsError"),
    (2353, false, "GetApplicationDownloadTaskInfo"),
    (2354, false, "PrioritizeApplicationBackgroundTask"),
    (2355, false, "PreferStorageEfficientUpdate"),
    (2356, false, "RequestStorageEfficientUpdatePreferable"),
    (2357, false, "EnableMultiCoreDownload"),
    (2358, false, "DisableMultiCoreDownload"),
    (2359, false, "IsMultiCoreDownloadEnabled"),
    (2400, false, "GetPromotionInfo"),
    (2401, false, "CountPromotionInfo"),
    (2402, false, "ListPromotionInfo"),
    (2403, false, "ImportPromotionJsonForDebug"),
    (2404, false, "ClearPromotionInfoForDebug"),
    (2500, false, "ConfirmAvailableTime"),
    (2510, false, "CreateApplicationResource"),
    (2511, false, "GetApplicationResource"),
    (2513, false, "LaunchMicroApplication"),
    (2514, false, "ClearTaskOfAsyncTaskManager"),
    (2515, false, "CleanupAllPlaceHolderAndFragmentsIfNoTask"),
    (2516, false, "EnsureApplicationCertificate"),
    (2517, false, "CreateApplicationInstance"),
    (2518, false, "UpdateQualificationForDebug"),
    (2519, false, "IsQualificationTransitionSupported"),
    (2520, true, "IsQualificationTransitionSupportedByProcessId"),
    (2521, false, "GetRightsUserChangedEvent"),
    (2522, false, "IsRomRedirectionAvailable"),
    (2800, false, "GetApplicationIdOfPreomia"),
    (3000, false, "RegisterDeviceLockKey"),
    (3001, false, "UnregisterDeviceLockKey"),
    (3002, false, "VerifyDeviceLockKey"),
    (3003, false, "HideApplicationIcon"),
    (3004, false, "ShowApplicationIcon"),
    (3005, false, "HideApplicationTitle"),
    (3006, false, "ShowApplicationTitle"),
    (3007, false, "EnableGameCard"),
    (3008, false, "DisableGameCard"),
    (3009, false, "EnableLocalContentShare"),
    (3010, false, "DisableLocalContentShare"),
    (3011, false, "IsApplicationIconHidden"),
    (3012, false, "IsApplicationTitleHidden"),
    (3013, false, "IsGameCardEnabled"),
    (3014, false, "IsLocalContentShareEnabled"),
    (3050, false, "ListAssignELicenseTaskResult"),
    (4022, true, "Unknown4022"),
    (9999, false, "GetApplicationCertificate"),
];

/// Port of upstream `IApplicationManagerInterface` ownership and command table.
pub struct IApplicationManagerInterface {
    #[allow(dead_code)]
    system: SystemRef,
    // The Event bridge lazily creates the kernel endpoint; ownership remains
    // with this interface, matching upstream's record_update_system_event.
    record_update_system_event: Event,
    sd_card_mount_status_event: Event,
    gamecard_update_detection_event: Event,
    gamecard_mount_failure_event: Event,
    gamecard_waken_ready_event: Event,
    unknown_event: Event,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IApplicationManagerInterface {
    pub fn new(system: SystemRef) -> Self {
        let functions = IAPPLICATION_MANAGER_INTERFACE_COMMANDS
            .iter()
            .map(|&(command_id, _, name)| {
                let handler = match command_id {
                    0 => Some(Self::list_application_record_handler as _),
                    44 => Some(Self::get_sd_card_mount_status_changed_event_handler as _),
                    52 => Some(Self::get_game_card_update_detection_event_handler as _),
                    505 => Some(Self::get_game_card_mount_failure_event_handler as _),
                    511 => Some(Self::get_game_card_waken_ready_event_handler as _),
                    4022 => Some(Self::unknown4022_handler as _),
                    2 => Some(Self::get_application_record_update_system_event_handler as _),
                    70 => Some(Self::resume_all_handler as _),
                    71 => Some(Self::get_storage_size_handler as _),
                    2520 => {
                        Some(Self::is_qualification_transition_supported_by_process_id_handler as _)
                    }
                    _ => None,
                };
                (command_id, handler, name)
            })
            .collect::<Vec<_>>();
        Self {
            system,
            record_update_system_event: Event::new(),
            sd_card_mount_status_event: Event::new(),
            gamecard_update_detection_event: Event::new(),
            gamecard_mount_failure_event: Event::new(),
            gamecard_waken_ready_event: Event::new(),
            unknown_event: Event::new(),
            handlers: build_handler_map(&functions),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn list_application_record_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        use crate::file_sys::nca_metadata::{ContentRecordType, TitleType};
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let offset = RequestParser::new(ctx).pop_u32() as i32;
        let provider = service.system.get().get_content_provider()
            .expect("application listing requires the content provider");
        let entries = provider.lock().unwrap().list_entries_filter_origin(
            None, Some(TitleType::Application), Some(ContentRecordType::Program), None);
        let bytes = Self::list_application_record(
            entries.iter().map(|(_, entry)| entry.title_id),
            crate::launch_timestamp_cache::get_launch_timestamp,
            offset,
            ctx.get_write_buffer_size(0) / 0x18,
        );
        let count = (bytes.len() / 0x18) as u32;
        ctx.write_buffer(&bytes, 0);
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(count);
    }

    // Mechanical separation of upstream ListApplicationRecord's record-building
    // body from its IPC adapter; injected timestamp lookup permits isolated tests.
    fn list_application_record(
        installed: impl IntoIterator<Item = u64>,
        timestamp: impl Fn(u64) -> i64,
        offset: i32,
        limit: usize,
    ) -> Vec<u8> {
        let mut records = Vec::new();
        for application_id in installed {
            if application_id < 0x0100000000001FFF || application_id & 0xFFF != 0 {
                continue;
            }
            records.push(ApplicationRecord {
                application_id,
                last_event: ApplicationEvent::Installed,
                attributes: 0,
                _padding0: [0; 6],
                last_updated: timestamp(application_id),
            });
        }
        records.sort_unstable_by(|a, b| b.last_updated.cmp(&a.last_updated)
            .then_with(|| a.application_id.cmp(&b.application_id)));
        let mut bytes = Vec::new();
        for record in records.iter().skip(offset.max(0) as usize).take(limit) {
            bytes.extend_from_slice(&record.application_id.to_le_bytes());
            bytes.push(record.last_event as u8);
            bytes.push(record.attributes);
            bytes.extend_from_slice(&record._padding0);
            bytes.extend_from_slice(&record.last_updated.to_le_bytes());
        }
        bytes
    }

    fn get_application_record_update_system_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        log::warn!("(STUBBED) GetApplicationRecordUpdateSystemEvent called");
        service.record_update_system_event.signal();
        let Some(object_id) = service.record_update_system_event.copy_object_id(ctx) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    fn get_sd_card_mount_status_changed_event_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        log::warn!("(STUBBED) get_sd_card_mount_status_changed_event called");
        let Some(object_id) = service.sd_card_mount_status_event.copy_object_id(ctx) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    fn get_game_card_update_detection_event_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        log::warn!("(STUBBED) get_game_card_update_detection_event called");
        let Some(object_id) = service.gamecard_update_detection_event.copy_object_id(ctx) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    fn get_game_card_mount_failure_event_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        log::warn!("(STUBBED) get_game_card_mount_failure_event called");
        let Some(object_id) = service.gamecard_mount_failure_event.copy_object_id(ctx) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    fn get_game_card_waken_ready_event_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        log::warn!("(STUBBED) get_game_card_waken_ready_event called");
        let Some(object_id) = service.gamecard_waken_ready_event.copy_object_id(ctx) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    fn unknown4022_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        log::warn!("(STUBBED) unknown4022 called");
        service.unknown_event.signal();
        let Some(object_id) = service.unknown_event.copy_object_id(ctx) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    fn parse_storage_id(raw: u8) -> Option<StorageId> {
        match raw {
            0 => Some(StorageId::None),
            1 => Some(StorageId::Host),
            2 => Some(StorageId::GameCard),
            3 => Some(StorageId::NandSystem),
            4 => Some(StorageId::NandUser),
            5 => Some(StorageId::SdCard),
            _ => None,
        }
    }

    /// Port of `IApplicationManagerInterface::GetStorageSize`.
    fn get_storage_size(&self, storage_id: StorageId) -> (i64, i64) {
        let controller = self.system.get().get_filesystem_controller();
        let controller = controller.lock().unwrap();
        (
            controller.get_total_space_size(storage_id) as i64,
            controller.get_free_space_size(storage_id) as i64,
        )
    }

    fn get_storage_size_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let this = unsafe {
            &*(this as *const dyn ServiceFramework as *const IApplicationManagerInterface)
        };
        let mut rp = RequestParser::new(ctx);
        let raw_storage_id = rp.pop_u8();
        let storage_id = Self::parse_storage_id(raw_storage_id)
            .unwrap_or_else(|| panic!("invalid StorageId={raw_storage_id}"));
        log::info!("GetStorageSize called, storage_id={storage_id:?}");

        let (total_space_size, free_space_size) = this.get_storage_size(storage_id);
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i64(total_space_size);
        rb.push_i64(free_space_size);
    }

    fn is_qualification_transition_supported_by_process_id_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let process_id = rp.pop_u64();
        let is_supported = is_qualification_transition_supported_by_process_id(process_id);

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(is_supported);
    }

    fn resume_all_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        resume_all();
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }
}

impl SessionRequestHandler for IApplicationManagerInterface {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "IApplicationManagerInterface"
    }
}

impl ServiceFramework for IApplicationManagerInterface {
    fn get_service_name(&self) -> &str {
        "IApplicationManagerInterface"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Stub: ResumeAll does nothing upstream.
pub fn resume_all() {
    log::warn!("(STUBBED) IApplicationManagerInterface::ResumeAll called");
}

/// Stub: IsAnyApplicationEntityInstalled always returns true upstream.
pub fn is_any_application_entity_installed() -> bool {
    log::warn!("(STUBBED) IApplicationManagerInterface::IsAnyApplicationEntityInstalled called");
    true
}

/// Stub: IsApplicationUpdateRequested always returns false upstream.
pub fn is_application_update_requested(_application_id: u64) -> (bool, u32) {
    log::warn!("(STUBBED) IApplicationManagerInterface::IsApplicationUpdateRequested called");
    (false, 0)
}

/// Stub: CheckApplicationLaunchVersion does nothing upstream.
pub fn check_application_launch_version(_application_id: u64) {
    log::warn!("(STUBBED) IApplicationManagerInterface::CheckApplicationLaunchVersion called");
}

/// Stub: upstream reports qualification transitions as supported for every process.
pub fn is_qualification_transition_supported_by_process_id(process_id: u64) -> bool {
    log::warn!(
        "(STUBBED) IApplicationManagerInterface::IsQualificationTransitionSupportedByProcessId called, process_id={process_id}"
    );
    true
}

/// Stub: GetApplicationView fills stub data upstream.
pub fn get_application_view(application_ids: &[u64], out_views: &mut [ApplicationView]) {
    let size = core::cmp::min(application_ids.len(), out_views.len());
    log::warn!(
        "(STUBBED) IApplicationManagerInterface::GetApplicationView called, size={}",
        application_ids.len()
    );

    for i in 0..size {
        let mut view = ApplicationView::default();
        view.application_id = application_ids[i];
        view.unk = 0x70000;
        view.flags = 0x401f17;
        out_views[i] = view;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_all_command_returns_upstream_success() {
        let service = IApplicationManagerInterface::new(SystemRef::null());
        let mut ctx = HLERequestContext::new();
        service.handlers[&70].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(ctx.command_buffer()[6], 0);
        assert!(ctx.outgoing_copy_objects.is_empty());
    }

    #[test]
    fn application_listing_filters_sorts_and_paginates_records() {
        // Synthetic application IDs, unrelated to any title.
        let a = 0x0100_0000_0000_2000;
        let b = a + 0x1000;
        let c = b + 0x1000;
        let inputs = [0, a - 1, a + 1, c, b, a];
        let timestamp = |id| if id == c { -7 } else { 42 };
        let bytes = IApplicationManagerInterface::list_application_record(inputs, timestamp, -1, 9);
        assert_eq!(bytes.len(), 3 * 24);
        for (record, id) in bytes.chunks_exact(24).zip([a, b, c]) {
            assert_eq!(&record[..8], &id.to_le_bytes());
            assert_eq!(record[8], 3);
            assert_eq!(&record[9..16], &[0; 7]);
            assert_eq!(&record[16..24], &timestamp(id).to_le_bytes());
        }
        assert_eq!(IApplicationManagerInterface::list_application_record(inputs, timestamp, 1, 1), bytes[24..48]);
        assert!(IApplicationManagerInterface::list_application_record(inputs, timestamp, 3, 9).is_empty());
        assert!(IApplicationManagerInterface::list_application_record(inputs, timestamp, 0, 0).is_empty());
        assert_eq!(std::mem::offset_of!(ApplicationRecord, last_updated), 16);
        assert_eq!(std::mem::size_of::<ApplicationRecord>(), 24);
    }
    #[test]
    fn application_events_preserve_identity_and_upstream_signal_state() {
        use crate::hle::kernel::k_process::{KProcess, ProcessLock};
        use crate::hle::kernel::k_readable_event::KReadableEvent;
        use crate::hle::kernel::k_thread::{KThread, KThreadLock};
        use crate::hle::service::hle_ipc::KAutoObjectRef;
        use std::sync::{Arc, Mutex};

        let service = IApplicationManagerInterface::new(SystemRef::null());
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        for (command, event, expected_signal) in [
            (44, &service.sd_card_mount_status_event, false),
            (52, &service.gamecard_update_detection_event, false),
            (505, &service.gamecard_mount_failure_event, false),
            (511, &service.gamecard_waken_ready_event, false),
            (4022, &service.unknown_event, true),
        ] {
            let object_id = command as u64 + 100;
            let readable = Arc::new(Mutex::new(KReadableEvent::new()));
            readable.lock().unwrap().initialize(1, object_id);
            process.lock().unwrap().register_readable_event_object(object_id, readable.clone());
            event.attach_kernel_event(readable.clone(), process.clone());
            for _ in 0..2 {
                let mut ctx = HLERequestContext::new_with_thread(thread.clone(), 0);
                service.handlers[&command].handler_callback.unwrap()(&service, &mut ctx);
                assert!(matches!(ctx.outgoing_copy_objects.as_slice(),
                    [KAutoObjectRef::ObjectId(id)] if *id == object_id));
                assert_eq!(readable.lock().unwrap().is_signaled(), expected_signal);
                event.clear();
                assert!(!readable.lock().unwrap().is_signaled());
            }
        }
    }



    #[test]
    fn record_update_requests_copy_and_signal_the_same_event() {
        use crate::hle::kernel::k_process::{KProcess, ProcessLock};
        use crate::hle::kernel::k_readable_event::KReadableEvent;
        use crate::hle::kernel::k_thread::{KThread, KThreadLock};
        use crate::hle::service::hle_ipc::KAutoObjectRef;
        use std::sync::{Arc, Mutex};

        let service = IApplicationManagerInterface::new(SystemRef::null());
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let readable = Arc::new(Mutex::new(KReadableEvent::new()));
        readable.lock().unwrap().initialize(1, 2);
        process
            .lock()
            .unwrap()
            .register_readable_event_object(2, readable.clone());
        service
            .record_update_system_event
            .attach_kernel_event(readable.clone(), process.clone());
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        assert!(!readable.lock().unwrap().is_signaled());
        for _ in 0..2 {
            let mut ctx = HLERequestContext::new_with_thread(thread.clone(), 0);
            service.handlers[&2].handler_callback.unwrap()(&service, &mut ctx);
            assert!(matches!(
                ctx.outgoing_copy_objects.as_slice(),
                [KAutoObjectRef::ObjectId(2)]
            ));
            assert!(readable.lock().unwrap().is_signaled());
            service.record_update_system_event.clear();
            assert!(!readable.lock().unwrap().is_signaled());
        }
    }

    #[test]
    fn qualification_transition_command_matches_upstream_registration_and_result() {
        let service = IApplicationManagerInterface::new(SystemRef::null());
        assert!(service
            .handlers()
            .get(&2520)
            .and_then(|info| info.handler_callback)
            .is_some());
        assert!(is_qualification_transition_supported_by_process_id(0x51));
    }

    #[test]
    fn storage_size_command_matches_upstream_registration_and_output_order() {
        let system = crate::core::System::new();
        let service = IApplicationManagerInterface::new(SystemRef::from_ref(&system));

        assert!(service
            .handlers()
            .get(&71)
            .and_then(|info| info.handler_callback)
            .is_some());

        let (total, free) = service.get_storage_size(StorageId::None);
        assert_eq!((total, free), (0, 0));
    }
}
