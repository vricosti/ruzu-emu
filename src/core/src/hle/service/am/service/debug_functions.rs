// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/service/debug_functions.h
//! Port of zuyu/src/core/hle/service/am/service/debug_functions.cpp

use std::collections::BTreeMap;

use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for IDebugFunctions:
/// - 0: NotifyMessageToHomeMenuForDebug (unimplemented)
/// - 1: OpenMainApplication (unimplemented)
/// - 10: PerformSystemButtonPressing (unimplemented)
/// - 20: InvalidateTransitionLayer (unimplemented)
/// - 30: RequestLaunchApplicationWithUserAndArgumentForDebug (unimplemented)
/// - 31: RequestLaunchApplicationByApplicationLaunchInfoForDebug (unimplemented)
/// - 40: GetAppletResourceUsageInfo (unimplemented)
/// - 50: AddSystemProgramIdAndAppletIdForDebug (unimplemented)
/// - 51: AddOperationConfirmedLibraryAppletIdForDebug (unimplemented)
/// - 100: SetCpuBoostModeForApplet (unimplemented)
/// - 101: CancelCpuBoostModeForApplet (unimplemented)
/// - 110: PushToAppletBoundChannelForDebug (unimplemented)
/// - 111: TryPopFromAppletBoundChannelForDebug (unimplemented)
/// - 120: AlarmSettingNotificationEnableAppEventReserve (unimplemented)
/// - 121: AlarmSettingNotificationDisableAppEventReserve (unimplemented)
/// - 122: AlarmSettingNotificationPushAppEventNotify (unimplemented)
/// - 130: FriendInvitationSetApplicationParameter (unimplemented)
/// - 131: FriendInvitationClearApplicationParameter (unimplemented)
/// - 132: FriendInvitationPushApplicationParameter (unimplemented)
/// - 140: RestrictPowerOperationForSecureLaunchModeForDebug (unimplemented)
/// - 200: CreateFloatingLibraryAppletAccepterForDebug (unimplemented)
/// - 300: TerminateAllRunningApplicationsForDebug (unimplemented)
/// - 900: GetGrcProcessLaunchedSystemEvent (unimplemented)
pub struct IDebugFunctions {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IDebugFunctions {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "NotifyMessageToHomeMenuForDebug"),
                (1, None, "OpenMainApplication"),
                (10, None, "PerformSystemButtonPressing"),
                (20, None, "InvalidateTransitionLayer"),
                (30, None, "RequestLaunchApplicationWithUserAndArgumentForDebug"),
                (31, None, "RequestLaunchApplicationByApplicationLaunchInfoForDebug"),
                (40, None, "GetAppletResourceUsageInfo"),
                (50, None, "AddSystemProgramIdAndAppletIdForDebug"),
                (51, None, "AddOperationConfirmedLibraryAppletIdForDebug"),
                (100, None, "SetCpuBoostModeForApplet"),
                (101, None, "CancelCpuBoostModeForApplet"),
                (110, None, "PushToAppletBoundChannelForDebug"),
                (111, None, "TryPopFromAppletBoundChannelForDebug"),
                (120, None, "AlarmSettingNotificationEnableAppEventReserve"),
                (121, None, "AlarmSettingNotificationDisableAppEventReserve"),
                (122, None, "AlarmSettingNotificationPushAppEventNotify"),
                (130, None, "FriendInvitationSetApplicationParameter"),
                (131, None, "FriendInvitationClearApplicationParameter"),
                (132, None, "FriendInvitationPushApplicationParameter"),
                (140, None, "RestrictPowerOperationForSecureLaunchModeForDebug"),
                (200, None, "CreateFloatingLibraryAppletAccepterForDebug"),
                (300, None, "TerminateAllRunningApplicationsForDebug"),
                (900, None, "GetGrcProcessLaunchedSystemEvent"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IDebugFunctions {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_commands_are_explicitly_unimplemented_not_success_stubs() {
        let service = IDebugFunctions::new();
        let ids: Vec<_> = service.handlers().keys().copied().collect();
        assert_eq!(ids, [0, 1, 10, 20, 30, 31, 40, 50, 51, 100, 101, 110,
            111, 120, 121, 122, 130, 131, 132, 140, 200, 300, 900]);
        assert!(service.handlers().values().all(|entry| entry.handler_callback.is_none()));
    }
}

impl ServiceFramework for IDebugFunctions {
    fn get_service_name(&self) -> &str {
        "am::IDebugFunctions"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
