// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ns/dynamic_rights_interface.cpp/.h

use std::collections::BTreeMap;

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

pub const IDYNAMIC_RIGHTS_INTERFACE_COMMANDS: &[(u32, bool, &str)] = &[
    (0, false, "RequestApplicationRightsOnServer"),
    (1, false, "RequestAssignRights"),
    (4, false, "DeprecatedRequestAssignRightsToResume"),
    (5, true, "VerifyActivatedRightsOwners"),
    (6, false, "DeprecatedGetApplicationRightsStatus"),
    (7, false, "RequestPrefetchForDynamicRights"),
    (8, false, "GetDynamicRightsState"),
    (9, false, "RequestApplicationRightsOnServerToResume"),
    (10, false, "RequestAssignRightsToResume"),
    (11, false, "GetActivatedRightsUsers"),
    (12, false, "GetApplicationRightsStatus"),
    (13, true, "GetRunningApplicationStatus"),
    (14, false, "SelectApplicationLicense"),
    (15, false, "RequestContentsAuthorizationToken"),
    (16, false, "QualifyUser"),
    (17, false, "QualifyUserWithProcessId"),
    (18, true, "NotifyApplicationRightsCheckStart"),
    (19, false, "UpdateUserList"),
    (20, false, "IsRightsLostUser"),
    (
        21,
        false,
        "SetRequiredAddOnContentsOnContentsAvailabilityTransition",
    ),
    (22, false, "GetLimitedApplicationLicense"),
    (23, false, "GetLimitedApplicationLicenseUpgradableEvent"),
    (
        24,
        false,
        "NotifyLimitedApplicationLicenseUpgradableEventForDebug",
    ),
    (25, false, "RequestProceedDynamicRightsState"),
    (26, true, "HasAccountRestrictedRightsInRunningApplications"),
    (27, false, "Unknown27"),
    (28, false, "Unknown28"),
    (29, false, "Unknown29"),
    (30, false, "Unknown30"),
];

pub struct IDynamicRightsInterface {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IDynamicRightsInterface {
    pub fn new() -> Self {
        let entries: Vec<_> = IDYNAMIC_RIGHTS_INTERFACE_COMMANDS
            .iter()
            .map(|&(id, _, name)| {
                let handler: Option<fn(&dyn ServiceFramework, &mut HLERequestContext)> = match id {
                    5 => Some(Self::verify_activated_rights_owners),
                    13 => Some(Self::get_running_application_status),
                    18 => Some(Self::notify_application_rights_check_start),
                    26 => Some(Self::has_account_restricted_rights_in_running_applications),
                    _ => None,
                };
                (id, handler, name)
            })
            .collect();
        Self {
            handlers: build_handler_map(&entries),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn notify_application_rights_check_start(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) NotifyApplicationRightsCheckStart called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_running_application_status(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let rights_handle = RequestParser::new(ctx).pop_u64();
        log::warn!(
            "(STUBBED) GetRunningApplicationStatus called, rights_handle={:#x}",
            rights_handle
        );
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(0);
    }

    fn verify_activated_rights_owners(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let rights_handle = RequestParser::new(ctx).pop_u64();
        log::warn!(
            "(STUBBED) VerifyActivatedRightsOwners called, rights_handle={:#x}",
            rights_handle
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn has_account_restricted_rights_in_running_applications(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) HasAccountRestrictedRightsInRunningApplications called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(false);
    }
}

impl SessionRequestHandler for IDynamicRightsInterface {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str {
        "DynamicRightsInterface"
    }
}

impl ServiceFramework for IDynamicRightsInterface {
    fn get_service_name(&self) -> &str {
        "DynamicRightsInterface"
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

    #[test]
    fn command_table_and_stub_payloads_match_upstream() {
        let service = IDynamicRightsInterface::new();
        assert_eq!(service.handlers.len(), 29);
        for &(id, implemented, _) in IDYNAMIC_RIGHTS_INTERFACE_COMMANDS {
            let callback = service.handlers[&id].handler_callback;
            assert_eq!(callback.is_some(), implemented);
            if let Some(callback) = callback {
                let mut ctx = HLERequestContext::new();
                ctx.cmd_buf[0] = 0x89ab_cdef;
                ctx.cmd_buf[1] = 0x1234_5678;
                callback(&service, &mut ctx);
                assert_eq!(&ctx.cmd_buf[6..8], &[0, 0]);
                assert_eq!(ctx.write_size, if matches!(id, 13 | 26) { 9 } else { 8 });
                if matches!(id, 13 | 26) {
                    assert_eq!(ctx.cmd_buf[8], 0);
                }
            }
        }
    }
}
