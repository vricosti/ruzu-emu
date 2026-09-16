// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ns/content_management_interface.h
//! Port of zuyu/src/core/hle/service/ns/content_management_interface.cpp
//!
//! IContentManagementInterface — content management operations for NS.

use std::collections::BTreeMap;

use super::ns_types::{ApplicationOccupiedSize, ApplicationOccupiedSizeEntity};
use crate::file_sys::romfs_factory::StorageId;
use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for IContentManagementInterface.
///
/// Corresponds to the function table in upstream content_management_interface.cpp.
pub mod commands {
    pub const CALCULATE_APPLICATION_OCCUPIED_SIZE: u32 = 11;
    pub const CHECK_SD_CARD_MOUNT_STATUS: u32 = 43;
    pub const GET_TOTAL_SPACE_SIZE: u32 = 47;
    pub const GET_FREE_SPACE_SIZE: u32 = 48;
    pub const COUNT_APPLICATION_CONTENT_META: u32 = 600;
    pub const LIST_APPLICATION_CONTENT_META_STATUS: u32 = 601;
    pub const LIST_APPLICATION_CONTENT_META_STATUS_WITH_RIGHTS_CHECK: u32 = 605;
    pub const IS_ANY_APPLICATION_RUNNING: u32 = 607;
}

/// IContentManagementInterface.
///
/// Corresponds to `IContentManagementInterface` in upstream.
pub struct IContentManagementInterface {
    system: crate::core::SystemRef,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IContentManagementInterface {
    pub fn new(system: crate::core::SystemRef) -> Self {
        let handlers = build_handler_map(&[
            (
                commands::CALCULATE_APPLICATION_OCCUPIED_SIZE,
                Some(Self::calculate_application_occupied_size_handler),
                "CalculateApplicationOccupiedSize",
            ),
            (
                commands::CHECK_SD_CARD_MOUNT_STATUS,
                Some(Self::check_sd_card_mount_status_handler),
                "CheckSdCardMountStatus",
            ),
            (
                commands::GET_TOTAL_SPACE_SIZE,
                Some(Self::get_total_space_size_handler),
                "GetTotalSpaceSize",
            ),
            (
                commands::GET_FREE_SPACE_SIZE,
                Some(Self::get_free_space_size_handler),
                "GetFreeSpaceSize",
            ),
            (
                commands::COUNT_APPLICATION_CONTENT_META,
                None,
                "CountApplicationContentMeta",
            ),
            (
                commands::LIST_APPLICATION_CONTENT_META_STATUS,
                None,
                "ListApplicationContentMetaStatus",
            ),
            (
                commands::LIST_APPLICATION_CONTENT_META_STATUS_WITH_RIGHTS_CHECK,
                None,
                "ListApplicationContentMetaStatusWithRightsCheck",
            ),
            (
                commands::IS_ANY_APPLICATION_RUNNING,
                None,
                "IsAnyApplicationRunning",
            ),
        ]);
        Self {
            system,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
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

    fn calculate_application_occupied_size_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let application_id = rp.pop_u64();
        match service.calculate_application_occupied_size(application_id) {
            Ok(size) => {
                let mut rb = ResponseBuilder::new(
                    ctx,
                    2 + (core::mem::size_of::<ApplicationOccupiedSize>() / 4) as u32,
                    0,
                    0,
                );
                rb.push_result(RESULT_SUCCESS);
                rb.push_raw(&size);
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }

    pub(super) fn check_sd_card_mount_status_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(match service.check_sd_card_mount_status() {
            Ok(()) => RESULT_SUCCESS,
            Err(result) => result,
        });
    }

    fn get_total_space_size_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let Some(storage_id) = Self::parse_storage_id(rp.pop_u8()) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };
        match service.get_total_space_size(storage_id) {
            Ok(size) => {
                let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_i64(size);
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }

    pub(super) fn get_free_space_size_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut rp = RequestParser::new(ctx);
        let Some(storage_id) = Self::parse_storage_id(rp.pop_u8()) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };
        match service.get_free_space_size(storage_id) {
            Ok(size) => {
                let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_i64(size);
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }

    /// CalculateApplicationOccupiedSize (cmd 11).
    ///
    /// Corresponds to upstream `IContentManagementInterface::CalculateApplicationOccupiedSize`.
    pub fn calculate_application_occupied_size(
        &self,
        application_id: u64,
    ) -> Result<ApplicationOccupiedSize, ResultCode> {
        log::warn!(
            "(STUBBED) CalculateApplicationOccupiedSize called, application_id={:016x}",
            application_id,
        );

        let stub_entity = ApplicationOccupiedSizeEntity {
            storage_id: StorageId::SdCard as u8,
            _padding: [0; 7],
            app_size: 8 * 1024 * 1024 * 1024,   // 8 GiB
            patch_size: 2 * 1024 * 1024 * 1024, // 2 GiB
            aoc_size: 12 * 1024 * 1024,         // 12 MiB
        };

        Ok(ApplicationOccupiedSize {
            entities: [stub_entity; 4],
        })
    }

    /// CheckSdCardMountStatus (cmd 43).
    ///
    /// Corresponds to upstream `IContentManagementInterface::CheckSdCardMountStatus`.
    pub fn check_sd_card_mount_status(&self) -> Result<(), ResultCode> {
        log::warn!("(STUBBED) CheckSdCardMountStatus called");
        Ok(())
    }

    /// GetTotalSpaceSize (cmd 47).
    ///
    /// Corresponds to upstream `IContentManagementInterface::GetTotalSpaceSize`.
    pub fn get_total_space_size(&self, storage_id: StorageId) -> Result<i64, ResultCode> {
        log::info!(
            "(STUBBED) GetTotalSpaceSize called, storage_id={:?}",
            storage_id,
        );
        let controller = self.system.get().get_filesystem_controller();
        let size = controller.lock().unwrap().get_total_space_size(storage_id);
        Ok(size as i64)
    }

    /// GetFreeSpaceSize (cmd 48).
    ///
    /// Corresponds to upstream `IContentManagementInterface::GetFreeSpaceSize`.
    pub fn get_free_space_size(&self, storage_id: StorageId) -> Result<i64, ResultCode> {
        log::info!(
            "(STUBBED) GetFreeSpaceSize called, storage_id={:?}",
            storage_id,
        );
        let controller = self.system.get().get_filesystem_controller();
        let size = controller.lock().unwrap().get_free_space_size(storage_id);
        Ok(size as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_implemented_commands_have_handlers() {
        let service = IContentManagementInterface::new(crate::core::SystemRef::null());
        for command in [11, 43, 47, 48] {
            assert!(service
                .handlers()
                .get(&command)
                .and_then(|info| info.handler_callback)
                .is_some());
        }
        for command in [600, 601, 605, 607] {
            assert!(service
                .handlers()
                .get(&command)
                .and_then(|info| info.handler_callback)
                .is_none());
        }
    }

    #[test]
    fn occupied_size_matches_upstream_stub_layout_and_values() {
        let service = IContentManagementInterface::new(crate::core::SystemRef::null());
        let size = service.calculate_application_occupied_size(0).unwrap();
        for entity in size.entities {
            assert_eq!(entity.storage_id, StorageId::SdCard as u8);
            assert_eq!(entity._padding, [0; 7]);
            assert_eq!(entity.app_size, 8 * 1024 * 1024 * 1024);
            assert_eq!(entity.patch_size, 2 * 1024 * 1024 * 1024);
            assert_eq!(entity.aoc_size, 12 * 1024 * 1024);
        }
    }

    #[test]
    fn storage_id_parser_accepts_only_upstream_values() {
        assert_eq!(
            IContentManagementInterface::parse_storage_id(0),
            Some(StorageId::None)
        );
        assert_eq!(
            IContentManagementInterface::parse_storage_id(5),
            Some(StorageId::SdCard)
        );
        assert_eq!(IContentManagementInterface::parse_storage_id(6), None);
    }
}

impl SessionRequestHandler for IContentManagementInterface {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ns::IContentManagementInterface"
    }
}

impl ServiceFramework for IContentManagementInterface {
    fn get_service_name(&self) -> &str {
        "ns::IContentManagementInterface"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
