// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden src/core/hle/service/ns/ecommerce_interface.h/.cpp
//!
//! IECommerceInterface — e-commerce operations for NS.

use std::collections::BTreeMap;

use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for IECommerceInterface.
///
/// Corresponds to the function table in upstream ecommerce_interface.cpp.
pub mod commands {
    pub const REQUEST_LINK_DEVICE: u32 = 0;
    pub const REQUEST_CLEANUP_ALL_PRE_INSTALLED_APPLICATIONS: u32 = 1;
    pub const REQUEST_CLEANUP_PRE_INSTALLED_APPLICATION: u32 = 2;
    pub const REQUEST_SYNC_RIGHTS: u32 = 3;
    pub const REQUEST_UNLINK_DEVICE: u32 = 4;
    pub const REQUEST_REVOKE_ALL_E_LICENSE: u32 = 5;
    pub const REQUEST_SYNC_RIGHTS_BASED_ON_ASSIGNED_E_LICENSES: u32 = 6;
}

/// IECommerceInterface.
///
/// Corresponds to `IECommerceInterface` in upstream.
pub struct IECommerceInterface {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IECommerceInterface {
    pub fn new() -> Self {
        let handlers = build_handler_map(&[
            (commands::REQUEST_LINK_DEVICE, None, "RequestLinkDevice"),
            (
                commands::REQUEST_CLEANUP_ALL_PRE_INSTALLED_APPLICATIONS,
                None,
                "RequestCleanupAllPreInstalledApplications",
            ),
            (
                commands::REQUEST_CLEANUP_PRE_INSTALLED_APPLICATION,
                None,
                "RequestCleanupPreInstalledApplication",
            ),
            (commands::REQUEST_SYNC_RIGHTS, None, "RequestSyncRights"),
            (commands::REQUEST_UNLINK_DEVICE, None, "RequestUnlinkDevice"),
            (
                commands::REQUEST_REVOKE_ALL_E_LICENSE,
                None,
                "RequestRevokeAllELicense",
            ),
            (
                commands::REQUEST_SYNC_RIGHTS_BASED_ON_ASSIGNED_E_LICENSES,
                None,
                "RequestSyncRightsBasedOnAssignedELicenses",
            ),
        ]);
        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl Default for IECommerceInterface {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionRequestHandler for IECommerceInterface {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ns::IECommerceInterface"
    }
}

impl ServiceFramework for IECommerceInterface {
    fn get_service_name(&self) -> &str {
        "ns::IECommerceInterface"
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
    fn command_table_matches_upstream_null_handlers() {
        let service = IECommerceInterface::new();
        let entries = service
            .handlers
            .iter()
            .map(|(id, info)| (*id, info.name, info.handler_callback.is_some()))
            .collect::<Vec<_>>();
        assert_eq!(
            entries,
            [
                (0, "RequestLinkDevice", false),
                (1, "RequestCleanupAllPreInstalledApplications", false),
                (2, "RequestCleanupPreInstalledApplication", false),
                (3, "RequestSyncRights", false),
                (4, "RequestUnlinkDevice", false),
                (5, "RequestRevokeAllELicense", false),
                (6, "RequestSyncRightsBasedOnAssignedELicenses", false),
            ]
        );
    }
}
