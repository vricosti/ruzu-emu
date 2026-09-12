// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden `src/core/hle/service/wlan/wlan.{h,cpp}`.

use std::collections::BTreeMap;

use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{
    HLERequestContext, SessionRequestHandler, SessionRequestHandlerPtr,
};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

pub struct ILocalManager {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ILocalManager {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "OpenMasterMode"),
                (0, None, "OpenMode_2"),
                (1, None, "CloseMasterMode"),
                (1, None, "CloseMode_2"),
                (2, None, "OpenClientMode"),
                (2, None, "GetMacAddress_2"),
                (3, None, "CloseClientMode"),
                (3, None, "CreateBss"),
                (4, None, "OpenSpectatorMode"),
                (4, None, "DestroyBss"),
                (5, None, "CloseSpectatorMode"),
                (5, None, "StartScan_2"),
                (6, None, "GetMacAddress_2"),
                (6, None, "StopScan_2"),
                (7, None, "CreateBss"),
                (7, None, "Connect_2"),
                (8, None, "DestroyBss"),
                (8, None, "CancelConnect_2"),
                (9, None, "StartScan_2"),
                (9, None, "Join"),
                (10, None, "StopScan_2"),
                (10, None, "CancelJoin"),
                (11, None, "Connect_2"),
                (11, None, "Disconnect_2"),
                (12, None, "CancelConnect_2"),
                (12, None, "SetBeaconLostCount"),
                (13, None, "Join"),
                (13, None, "GetSystemEvent_2"),
                (14, None, "CancelJoin"),
                (14, None, "GetConnectionStatus_2"),
                (15, None, "Disconnect_2"),
                (15, None, "GetClientStatus"),
                (16, None, "SetBeaconLostCount"),
                (16, None, "GetBssIndicationEvent"),
                (17, None, "GetSystemEvent_2"),
                (17, None, "GetBssIndicationInfo"),
                (18, None, "GetConnectionStatus_2"),
                (18, None, "GetState_2"),
                (19, None, "GetClientStatus"),
                (19, None, "GetAllowedChannels"),
                (20, None, "GetBssIndicationEvent"),
                (20, None, "AddIe"),
                (21, None, "GetBssIndicationInfo"),
                (21, None, "DeleteIe"),
                (22, None, "GetState_2"),
                (22, None, "PutFrameRaw"),
                (23, None, "GetAllowedChannels"),
                (23, None, "CancelGetFrame"),
                (24, None, "AddIe"),
                (24, None, "CreateRxEntry"),
                (25, None, "DeleteIe"),
                (25, None, "DeleteRxEntry"),
                (26, None, "PutFrameRaw"),
                (26, None, "AddEthertypeToRxEntry"),
                (27, None, "CancelGetFrame"),
                (27, None, "DeleteEthertypeFromRxEntry"),
                (28, None, "CreateRxEntry"),
                (28, None, "AddMatchingDataToRxEntry"),
                (29, None, "DeleteRxEntry"),
                (29, None, "RemoveMatchingDataFromRxEntry"),
                (30, None, "AddEthertypeToRxEntry"),
                (30, None, "GetScanResult_2"),
                (31, None, "DeleteEthertypeFromRxEntry"),
                (31, None, "PutActionFrameOneShot"),
                (32, None, "AddMatchingDataToRxEntry"),
                (32, None, "SetActionFrameWithBeacon"),
                (33, None, "RemoveMatchingDataFromRxEntry"),
                (33, None, "CancelActionFrameWithBeacon"),
                (34, None, "GetScanResult_2"),
                (34, None, "CreateRxEntryForActionFrame"),
                (35, None, "PutActionFrameOneShot"),
                (35, None, "DeleteRxEntryForActionFrame"),
                (36, None, "SetActionFrameWithBeacon"),
                (36, None, "AddSubtypeToRxEntryForActionFrame"),
                (37, None, "CancelActionFrameWithBeacon"),
                (37, None, "DeleteSubtypeFromRxEntryForActionFrame"),
                (38, None, "CreateRxEntryForActionFrame"),
                (38, None, "CancelGetActionFrame"),
                (39, None, "DeleteRxEntryForActionFrame"),
                (39, None, "GetRssi_2"),
                (40, None, "AddSubtypeToRxEntryForActionFrame"),
                (40, None, "SetMaxAssociationNumber"),
                (41, None, "DeleteSubtypeFromRxEntryForActionFrame"),
                (41, None, "Cmd41"),
                (42, None, "CancelGetActionFrame"),
                (42, None, "Cmd42"),
                (43, None, "GetRssi_2"),
                (43, None, "Cmd43"),
                (44, None, "SetMaxAssociationNumber"),
                (45, None, "OpenLcsMasterMode"),
                (46, None, "CloseLcsMasterMode"),
                (47, None, "OpenLcsClientMode"),
                (48, None, "CloseLcsClientMode"),
                (49, None, "GetChannelStats"),
                (50, None, "Cmd50"),
                (51, None, "Cmd51"),
                (52, None, "Cmd52"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for ILocalManager {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "wlan:lcl"
    }
}

impl ServiceFramework for ILocalManager {
    fn get_service_name(&self) -> &str {
        "wlan:lcl"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct ILocalGetFrame {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ILocalGetFrame {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(0, None, "GetFrameRaw")]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for ILocalGetFrame {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "wlan:lg"
    }
}

impl ServiceFramework for ILocalGetFrame {
    fn get_service_name(&self) -> &str {
        "wlan:lg"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct ILocalGetActionFrame {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ILocalGetActionFrame {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(0, None, "GetActionFrame")]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for ILocalGetActionFrame {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "wlan:lga"
    }
}

impl ServiceFramework for ILocalGetActionFrame {
    fn get_service_name(&self) -> &str {
        "wlan:lga"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct ISocketGetFrame {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISocketGetFrame {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(0, None, "GetFrameRaw")]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for ISocketGetFrame {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "wlan:sg"
    }
}

impl ServiceFramework for ISocketGetFrame {
    fn get_service_name(&self) -> &str {
        "wlan:sg"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct ISocketManager {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISocketManager {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "PutFrameRaw_2"),
                (1, None, "CancelGetFrame_2"),
                (2, None, "CreateRxEntry_2"),
                (3, None, "DeleteRxEntry_2"),
                (4, None, "AddEthertypeToRxEntry_2"),
                (5, None, "DeleteEthertypeFromRxEntry_2"),
                (6, None, "GetMacAddress_3"),
                (7, None, "SwitchTsfTimerFunction"),
                (8, None, "GetDeltaTimeBetweenSystemAndTsf"),
                (9, None, "RegisterSharedMemory"),
                (10, None, "UnregisterSharedMemory"),
                (11, None, "EnableSharedMemory"),
                (12, None, "SetMulticastFilter"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for ISocketManager {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "wlan:soc"
    }
}

impl ServiceFramework for ISocketManager {
    fn get_service_name(&self) -> &str {
        "wlan:soc"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct IDetectManager {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IDetectManager {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "Cmd0"),
                (1, None, "Cmd1"),
                (2, None, "Cmd2"),
                (3, None, "Cmd3"),
                (4, None, "Cmd4"),
                (5, None, "Cmd5"),
                (6, None, "Cmd6"),
                (7, None, "Cmd7"),
                (8, None, "Cmd8"),
                (9, None, "Cmd9"),
                (10, None, "Cmd10"),
                (11, None, "Cmd11"),
                (12, None, "Cmd12"),
                (13, None, "Cmd13"),
                (14, None, "Cmd14"),
                (15, None, "Cmd15"),
                (16, None, "Cmd16"),
                (17, None, "Cmd17"),
                (18, None, "Cmd18"),
                (19, None, "Cmd19"),
                (20, None, "Cmd20"),
                (21, None, "Cmd21"),
                (22, None, "Cmd22"),
                (23, None, "Cmd23"),
                (24, None, "Cmd24"),
                (25, None, "Cmd25"),
                (26, None, "Cmd26"),
                (27, None, "Cmd27"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IDetectManager {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "wlan:dtc"
    }
}

impl ServiceFramework for IDetectManager {
    fn get_service_name(&self) -> &str {
        "wlan:dtc"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct IPrivateServiceCreator {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IPrivateServiceCreator {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "CreateWirelessCommunicationService"),
                (1, None, "CreatePrivateWirelessCommunicationService"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IPrivateServiceCreator {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "wlan:p"
    }
}

impl ServiceFramework for IPrivateServiceCreator {
    fn get_service_name(&self) -> &str {
        "wlan:p"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct ISfDriverServiceCreator {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISfDriverServiceCreator {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(0, None, "CreateDriverService")]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for ISfDriverServiceCreator {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "wlan:nd"
    }
}

impl ServiceFramework for ISfDriverServiceCreator {
    fn get_service_name(&self) -> &str {
        "wlan:nd"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub fn loop_process(system: crate::core::SystemRef) {
    use crate::hle::service::server_manager::ServerManager;
    let server_manager = ServerManager::new_shared(system);
    {
        let mut server_manager = server_manager.lock().unwrap();
        server_manager.register_named_service(
            "wlan:lcl",
            Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(ILocalManager::new()) }),
            10,
        );
        server_manager.register_named_service(
            "wlan:lg",
            Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(ILocalGetFrame::new()) }),
            10,
        );
        server_manager.register_named_service(
            "wlan:lga",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(ILocalGetActionFrame::new())
            }),
            10,
        );
        server_manager.register_named_service(
            "wlan:sg",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(ISocketGetFrame::new())
            }),
            10,
        );
        server_manager.register_named_service(
            "wlan:soc",
            Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(ISocketManager::new()) }),
            10,
        );
        server_manager.register_named_service(
            "wlan:dtc",
            Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(IDetectManager::new()) }),
            4,
        );
        server_manager.register_named_service(
            "wlan:p",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(IPrivateServiceCreator::new())
            }),
            30,
        );
        server_manager.register_named_service(
            "wlan:nd",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(ISfDriverServiceCreator::new())
            }),
            5,
        );
    }
    ServerManager::run_server_shared(server_manager);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_tables_match_upstream() {
        assert_eq!(ILocalManager::new().handlers().len(), 53);
        assert_eq!(ISocketManager::new().handlers().len(), 13);
        assert_eq!(IDetectManager::new().handlers().len(), 28);
    }
}
