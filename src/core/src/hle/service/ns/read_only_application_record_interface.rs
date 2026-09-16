// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ns/read_only_application_record_interface.h
//! Port of zuyu/src/core/hle/service/ns/read_only_application_record_interface.cpp
//!
//! IReadOnlyApplicationRecordInterface — read-only access to application records.

use std::collections::BTreeMap;

use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::result::RESULT_SUCCESS;

/// IPC command table for IReadOnlyApplicationRecordInterface.
///
/// Corresponds to the function table in upstream read_only_application_record_interface.cpp.
pub mod commands {
    pub const HAS_APPLICATION_RECORD: u32 = 0;
    pub const NOTIFY_APPLICATION_FAILURE: u32 = 1;
    pub const IS_DATA_CORRUPTED_RESULT: u32 = 2;
    pub const LIST_APPLICATION_RECORD: u32 = 3;
}

/// IReadOnlyApplicationRecordInterface.
///
/// Corresponds to `IReadOnlyApplicationRecordInterface` in upstream.
pub struct IReadOnlyApplicationRecordInterface {
    system: crate::core::SystemRef,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IReadOnlyApplicationRecordInterface {
    pub fn new(system: crate::core::SystemRef) -> Self {
        let handlers = build_handler_map(&[
            (
                commands::HAS_APPLICATION_RECORD,
                Some(Self::has_application_record_handler),
                "HasApplicationRecord",
            ),
            (
                commands::NOTIFY_APPLICATION_FAILURE,
                None,
                "NotifyApplicationFailure",
            ),
            (
                commands::IS_DATA_CORRUPTED_RESULT,
                Some(Self::is_data_corrupted_result_handler),
                "IsDataCorruptedResult",
            ),
            (commands::LIST_APPLICATION_RECORD, Some(Self::list_application_record), "ListApplicationRecord"),
        ]);
        Self {
            system,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// HasApplicationRecord (cmd 0).
    ///
    /// Corresponds to upstream `IReadOnlyApplicationRecordInterface::HasApplicationRecord`.
    pub fn has_application_record(&self, program_id: u64) -> Result<bool, ResultCode> {
        log::warn!(
            "(STUBBED) HasApplicationRecord called, program_id={:016x}",
            program_id,
        );
        Ok(true)
    }

    /// IsDataCorruptedResult (cmd 2).
    ///
    /// Corresponds to upstream `IReadOnlyApplicationRecordInterface::IsDataCorruptedResult`.
    pub fn is_data_corrupted_result(&self, result: u32) -> Result<bool, ResultCode> {
        log::warn!(
            "(STUBBED) IsDataCorruptedResult called, result={:#x}",
            result,
        );
        Ok(false)
    }

    fn has_application_record_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = this.as_any().downcast_ref::<Self>().unwrap();
        let id = RequestParser::new(ctx).pop_u64();
        let exists = service.has_application_record(id).unwrap();
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(exists);
    }

    fn is_data_corrupted_result_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = this.as_any().downcast_ref::<Self>().unwrap();
        let result = RequestParser::new(ctx).pop_u32();
        let corrupted = service.is_data_corrupted_result(result).unwrap();
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(corrupted);
    }

    fn list_application_record(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = this.as_any().downcast_ref::<Self>().unwrap();
        use super::application_manager_interface::IApplicationManagerInterface;
        IApplicationManagerInterface::list_application_record_handler(
            &IApplicationManagerInterface::new(service.system), ctx);
    }
}

impl SessionRequestHandler for IReadOnlyApplicationRecordInterface {
    fn as_any(&self) -> &dyn std::any::Any { self }
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "ns::IReadOnlyApplicationRecordInterface"
    }
}

impl ServiceFramework for IReadOnlyApplicationRecordInterface {
    fn get_service_name(&self) -> &str {
        "ns::IReadOnlyApplicationRecordInterface"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
