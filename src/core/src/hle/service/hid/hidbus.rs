//! Port of zuyu/src/core/hle/service/hid/hidbus.h and hidbus.cpp
//!
//! Hidbus service ("hidbus").

use std::collections::BTreeMap;

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// Hidbus service for HID bus communication (e.g., Ring-Con).
pub struct Hidbus {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl Hidbus {
    fn stub_success_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let cmd = ctx.get_command();
        log::debug!("(STUBBED) hidbus command {}", cmd);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    pub fn new() -> Self {
        let handlers = build_handler_map(&[
            (1, Some(Self::stub_success_handler), "GetBusHandle"),
            (
                2,
                Some(Self::stub_success_handler),
                "IsExternalDeviceConnected",
            ),
            (3, Some(Self::stub_success_handler), "Initialize"),
            (4, Some(Self::stub_success_handler), "Finalize"),
            (5, Some(Self::stub_success_handler), "EnableExternalDevice"),
            (6, Some(Self::stub_success_handler), "GetExternalDeviceId"),
            (7, Some(Self::stub_success_handler), "SendCommandAsync"),
            (
                8,
                Some(Self::stub_success_handler),
                "GetSendCommandAsynceResult",
            ),
            (
                9,
                Some(Self::stub_success_handler),
                "SetEventForSendCommandAsycResult",
            ),
            (
                10,
                Some(Self::stub_success_handler),
                "GetSharedMemoryHandle",
            ),
            (
                11,
                Some(Self::stub_success_handler),
                "EnableJoyPollingReceiveMode",
            ),
            (
                12,
                Some(Self::stub_success_handler),
                "DisableJoyPollingReceiveMode",
            ),
            (13, None, "GetPollingData"),
            (14, Some(Self::stub_success_handler), "SetStatusManagerType"),
        ]);

        Self {
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for Hidbus {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "hidbus"
    }
}

impl ServiceFramework for Hidbus {
    fn get_service_name(&self) -> &str {
        "hidbus"
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
    #[test]
    fn polling_data_is_registered_without_a_handler() {
        let service = super::Hidbus::new();
        let command = service.handlers.get(&13).unwrap();
        assert_eq!(command.name, "GetPollingData");
        assert!(command.handler_callback.is_none());
    }
}
