// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden src/core/hle/service/psc/time/power_state_service.h/.cpp
//!
//! IPowerStateRequestHandler: handles power state requests for "time:p".

use std::collections::BTreeMap;
use std::sync::Arc;

use super::power_state_request_manager::PowerStateRequestManager;
use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command IDs for IPowerStateRequestHandler.
///
/// Corresponds to the function table in upstream power_state_service.cpp constructor.
pub mod commands {
    pub const GET_POWER_STATE_REQUEST_EVENT_READABLE_HANDLE: u32 = 0;
    pub const GET_AND_CLEAR_POWER_STATE_REQUEST: u32 = 1;
}

/// IPowerStateRequestHandler service.
///
/// Corresponds to `IPowerStateRequestHandler` in upstream power_state_service.h.
/// Upstream holds a reference to `PowerStateRequestManager&
/// m_power_state_request_manager` and delegates all operations to it.
pub struct PowerStateRequestHandler {
    /// Reference to the power state request manager.
    /// Corresponds to `PowerStateRequestManager& m_power_state_request_manager`
    /// in upstream.
    power_state_request_manager: Arc<PowerStateRequestManager>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl PowerStateRequestHandler {
    pub fn new(power_state_request_manager: Arc<PowerStateRequestManager>) -> Self {
        let handlers = build_handler_map(&[
            (
                commands::GET_POWER_STATE_REQUEST_EVENT_READABLE_HANDLE,
                Some(Self::get_power_state_request_event_readable_handle_handler),
                "GetPowerStateRequestEventReadableHandle",
            ),
            (
                commands::GET_AND_CLEAR_POWER_STATE_REQUEST,
                Some(Self::get_and_clear_power_state_request_handler),
                "GetAndClearPowerStateRequest",
            ),
        ]);
        Self {
            power_state_request_manager,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// GetPowerStateRequestEventReadableHandle (cmd 0).
    ///
    /// Corresponds to `IPowerStateRequestHandler::GetPowerStateRequestEventReadableHandle`
    /// in upstream power_state_service.cpp.
    /// Upstream returns `&m_power_state_request_manager.GetReadableEvent()`.
    /// The Rust event owner lazily materializes the equivalent copy handle.
    pub fn get_power_state_request_event_readable_handle(
        &self,
        ctx: &HLERequestContext,
    ) -> Option<u32> {
        log::debug!("IPowerStateRequestHandler::GetPowerStateRequestEventReadableHandle called");
        self.power_state_request_manager
            .get_event()
            .copy_handle(ctx)
    }

    /// GetAndClearPowerStateRequest (cmd 1).
    ///
    /// Corresponds to `IPowerStateRequestHandler::GetAndClearPowerStateRequest`
    /// in upstream power_state_service.cpp.
    /// Delegates to the PowerStateRequestManager.
    pub fn get_and_clear_power_state_request(&self) -> (bool, u32) {
        log::debug!("IPowerStateRequestHandler::GetAndClearPowerStateRequest called");
        self.power_state_request_manager
            .get_and_clear_power_state_request()
    }

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    fn get_power_state_request_event_readable_handle_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        match service.get_power_state_request_event_readable_handle(ctx) {
            Some(handle) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
                rb.push_result(crate::hle::result::RESULT_SUCCESS);
                rb.push_copy_objects(handle);
            }
            None => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(crate::hle::result::RESULT_SUCCESS);
            }
        }
    }

    fn get_and_clear_power_state_request_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let (cleared, priority) = service.get_and_clear_power_state_request();
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(crate::hle::result::RESULT_SUCCESS);
        rb.push_bool(cleared);
        if cleared {
            rb.push_u32(priority);
        } else {
            rb.push_u32(0);
        }
    }
}

impl SessionRequestHandler for PowerStateRequestHandler {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "time:p"
    }
}

impl ServiceFramework for PowerStateRequestHandler {
    fn get_service_name(&self) -> &str {
        "time:p"
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
    fn delegates_get_and_clear_to_the_shared_manager() {
        let manager = Arc::new(PowerStateRequestManager::new());
        let service = PowerStateRequestHandler::new(Arc::clone(&manager));

        assert_eq!(service.get_and_clear_power_state_request(), (false, 0));
        manager.update_pending_power_state_request_priority(3);
        manager.update_pending_power_state_request_priority(8);
        manager.signal_power_state_request_availability();
        assert_eq!(service.get_and_clear_power_state_request(), (true, 8));
        assert_eq!(service.get_and_clear_power_state_request(), (false, 0));
    }

    #[test]
    fn command_table_matches_upstream() {
        let service = PowerStateRequestHandler::new(Arc::new(PowerStateRequestManager::new()));
        let entries = service
            .handlers
            .iter()
            .map(|(id, info)| (*id, info.name, info.handler_callback.is_some()))
            .collect::<Vec<_>>();
        assert_eq!(
            entries,
            [
                (0, "GetPowerStateRequestEventReadableHandle", true),
                (1, "GetAndClearPowerStateRequest", true),
            ]
        );
    }
}
