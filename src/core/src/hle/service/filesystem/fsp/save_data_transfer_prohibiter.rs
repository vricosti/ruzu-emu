//! Port of zuyu/src/core/hle/service/filesystem/fsp/save_data_transfer_prohibiter.h and .cpp
//!
//! ISaveDataTransferProhibiter service.

/// ISaveDataTransferProhibiter has no IPC commands.
/// It exists solely as an object reference.
use std::collections::BTreeMap;
use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{FunctionInfo, ServiceFramework};

pub struct ISaveDataTransferProhibiter {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISaveDataTransferProhibiter {
    pub fn new() -> Self {
        Self { handlers: BTreeMap::new(), handlers_tipc: BTreeMap::new() }
    }
}

impl SessionRequestHandler for ISaveDataTransferProhibiter {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str { "ISaveDataTransferProhibiter" }
}

impl ServiceFramework for ISaveDataTransferProhibiter {
    fn get_service_name(&self) -> &str { "ISaveDataTransferProhibiter" }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> { &self.handlers }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> { &self.handlers_tipc }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prohibiter_is_a_commandless_ipc_object() {
        let service = ISaveDataTransferProhibiter::new();
        assert_eq!(service.service_name(), "ISaveDataTransferProhibiter");
        assert!(service.handlers().is_empty());
        assert!(service.handlers_tipc().is_empty());
    }
}
