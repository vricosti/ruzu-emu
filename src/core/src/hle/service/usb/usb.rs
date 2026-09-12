// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/usb/usb.cpp
//!
//! USB services. Most commands are unimplemented stubs.

use std::collections::BTreeMap;

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{
    HLERequestContext, SessionRequestHandler, SessionRequestHandlerPtr,
};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

pub struct IPdManufactureManager {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IPdManufactureManager {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(0, None, "OpenManufactureSession")]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IPdManufactureManager {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "usb:pd:m"
    }
}

impl ServiceFramework for IPdManufactureManager {
    fn get_service_name(&self) -> &str {
        "usb:pd:m"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct IQdbManager {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IQdbManager {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(0, None, "ImportQuirkDevices"), (1, None, "HasQuirk")]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IQdbManager {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "usb:qdb"
    }
}

impl ServiceFramework for IQdbManager {
    fn get_service_name(&self) -> &str {
        "usb:qdb"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct IPmObserverService {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IPmObserverService {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "GetTopologyChangeEvent"),
                (1, None, "GetFlattenedTopology"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IPmObserverService {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "usb:obsv"
    }
}

impl ServiceFramework for IPmObserverService {
    fn get_service_name(&self) -> &str {
        "usb:obsv"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// IPC command IDs for IDsInterface
pub mod ds_interface_commands {
    pub const ADD_ENDPOINT: u32 = 0;
    pub const GET_SETUP_EVENT: u32 = 1;
    pub const GET_SETUP_PACKET: u32 = 2;
    pub const ENABLE: u32 = 3;
    pub const DISABLE: u32 = 4;
    pub const CTRL_IN: u32 = 5;
    pub const CTRL_OUT: u32 = 6;
    pub const GET_CTRL_IN_COMPLETION_EVENT: u32 = 7;
    pub const GET_CTRL_IN_URB_REPORT: u32 = 8;
    pub const GET_CTRL_OUT_COMPLETION_EVENT: u32 = 9;
    pub const GET_CTRL_OUT_URB_REPORT: u32 = 10;
    pub const CTRL_STALL: u32 = 11;
    pub const APPEND_CONFIGURATION_DATA: u32 = 12;
}

/// IPC command IDs for IDsRootSession ("usb:ds")
pub mod ds_root_commands {
    pub const OPEN_DS_SERVICE: u32 = 0;
}

/// IPC command IDs for IClientEpSession
pub mod client_ep_commands {
    pub const RE_OPEN: u32 = 0;
    pub const CLOSE: u32 = 1;
    pub const GET_COMPLETION_EVENT: u32 = 2;
    pub const POPULATE_RING: u32 = 3;
    pub const POST_BUFFER_ASYNC: u32 = 4;
    pub const GET_XFER_REPORT: u32 = 5;
    pub const POST_BUFFER_MULTI_ASYNC: u32 = 6;
    pub const CREATE_SMMU_SPACE: u32 = 7;
    pub const SHARE_REPORT_RING: u32 = 8;
}

/// IPC command IDs for IClientIfSession
pub mod client_if_commands {
    pub const GET_STATE_CHANGE_EVENT: u32 = 0;
    pub const SET_INTERFACE: u32 = 1;
    pub const GET_INTERFACE: u32 = 2;
    pub const GET_ALTERNATE_INTERFACE: u32 = 3;
    pub const GET_CURRENT_FRAME: u32 = 4;
    pub const CTRL_XFER_ASYNC: u32 = 5;
    pub const GET_CTRL_XFER_COMPLETION_EVENT: u32 = 6;
    pub const GET_CTRL_XFER_REPORT: u32 = 7;
    pub const RESET_DEVICE: u32 = 8;
    pub const OPEN_USB_EP: u32 = 9;
}

/// IPC command IDs for IClientRootSession ("usb:hs")
pub mod hs_root_commands {
    pub const BIND_CLIENT_PROCESS: u32 = 0;
    pub const QUERY_ALL_INTERFACES: u32 = 1;
    pub const QUERY_AVAILABLE_INTERFACES: u32 = 2;
    pub const QUERY_ACQUIRED_INTERFACES: u32 = 3;
    pub const CREATE_INTERFACE_AVAILABLE_EVENT: u32 = 4;
    pub const DESTROY_INTERFACE_AVAILABLE_EVENT: u32 = 5;
    pub const GET_INTERFACE_STATE_CHANGE_EVENT: u32 = 6;
    pub const ACQUIRE_USB_IF: u32 = 7;
    pub const SET_TEST_MODE: u32 = 8;
}

/// IPC command IDs for IPdManager ("usb:pd")
pub mod pd_commands {
    pub const OPEN_SESSION: u32 = 0;
}

/// IPC command IDs for IPdSession
pub mod pd_session_commands {
    pub const BIND_NOTICE_EVENT: u32 = 0;
    pub const UNBIND_NOTICE_EVENT: u32 = 1;
    pub const GET_STATUS: u32 = 2;
    pub const GET_NOTICE: u32 = 3;
    pub const ENABLE_POWER_REQUEST_NOTICE: u32 = 4;
    pub const DISABLE_POWER_REQUEST_NOTICE: u32 = 5;
    pub const REPLY_POWER_REQUEST: u32 = 6;
}

/// IPC command IDs for IPdCradleManager ("usb:pd:c")
pub mod pd_cradle_commands {
    pub const OPEN_CRADLE_SESSION: u32 = 0;
}

/// IPC command IDs for IPdCradleSession
pub mod pd_cradle_session_commands {
    pub const SET_CRADLE_VDO: u32 = 0;
    pub const GET_CRADLE_VDO: u32 = 1;
    pub const RESET_CRADLE_USB_HUB: u32 = 2;
    pub const GET_HOST_PDC_FIRMWARE_TYPE: u32 = 3;
    pub const GET_HOST_PDC_FIRMWARE_REVISION: u32 = 4;
    pub const GET_HOST_PDC_MANUFACTURE_ID: u32 = 5;
    pub const GET_HOST_PDC_DEVICE_ID: u32 = 6;
    pub const ENABLE_CRADLE_RECOVERY: u32 = 7;
    pub const DISABLE_CRADLE_RECOVERY: u32 = 8;
}

/// IPC command IDs for IPmMainService
pub mod pm_commands {
    pub const GET_POWER_EVENT: u32 = 0;
    pub const GET_POWER_STATE: u32 = 1;
    pub const GET_DATA_EVENT: u32 = 2;
    pub const GET_DATA_ROLE: u32 = 3;
    pub const SET_DIAG_DATA: u32 = 4;
    pub const GET_DIAG_DATA: u32 = 5;
}

macro_rules! impl_service_framework {
    ($ty:ty, $name:expr) => {
        impl SessionRequestHandler for $ty {
            fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
                ServiceFramework::handle_sync_request_impl(self, ctx)
            }

            fn service_name(&self) -> &str {
                $name
            }
        }

        impl ServiceFramework for $ty {
            fn get_service_name(&self) -> &str {
                $name
            }

            fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
                &self.handlers
            }

            fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
                &self.handlers_tipc
            }
        }
    };
}

pub struct IDsInterface {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IDsInterface {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (ds_interface_commands::ADD_ENDPOINT, None, "AddEndpoint"),
                (
                    ds_interface_commands::GET_SETUP_EVENT,
                    None,
                    "GetSetupEvent",
                ),
                (
                    ds_interface_commands::GET_SETUP_PACKET,
                    None,
                    "GetSetupPacket",
                ),
                (ds_interface_commands::ENABLE, None, "Enable"),
                (ds_interface_commands::DISABLE, None, "Disable"),
                (ds_interface_commands::CTRL_IN, None, "CtrlIn"),
                (ds_interface_commands::CTRL_OUT, None, "CtrlOut"),
                (
                    ds_interface_commands::GET_CTRL_IN_COMPLETION_EVENT,
                    None,
                    "GetCtrlInCompletionEvent",
                ),
                (
                    ds_interface_commands::GET_CTRL_IN_URB_REPORT,
                    None,
                    "GetCtrlInUrbReport",
                ),
                (
                    ds_interface_commands::GET_CTRL_OUT_COMPLETION_EVENT,
                    None,
                    "GetCtrlOutCompletionEvent",
                ),
                (
                    ds_interface_commands::GET_CTRL_OUT_URB_REPORT,
                    None,
                    "GetCtrlOutUrbReport",
                ),
                (ds_interface_commands::CTRL_STALL, None, "CtrlStall"),
                (
                    ds_interface_commands::APPEND_CONFIGURATION_DATA,
                    None,
                    "AppendConfigurationData",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl_service_framework!(IDsInterface, "IDsInterface");

pub struct IDsRootSession {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IDsRootSession {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(
                ds_root_commands::OPEN_DS_SERVICE,
                None,
                "OpenDsService",
            )]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl_service_framework!(IDsRootSession, "usb:ds");

pub struct IClientEpSession {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IClientEpSession {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (client_ep_commands::RE_OPEN, None, "ReOpen"),
                (client_ep_commands::CLOSE, None, "Close"),
                (
                    client_ep_commands::GET_COMPLETION_EVENT,
                    None,
                    "GetCompletionEvent",
                ),
                (client_ep_commands::POPULATE_RING, None, "PopulateRing"),
                (
                    client_ep_commands::POST_BUFFER_ASYNC,
                    None,
                    "PostBufferAsync",
                ),
                (client_ep_commands::GET_XFER_REPORT, None, "GetXferReport"),
                (
                    client_ep_commands::POST_BUFFER_MULTI_ASYNC,
                    None,
                    "PostBufferMultiAsync",
                ),
                (
                    client_ep_commands::CREATE_SMMU_SPACE,
                    None,
                    "CreateSmmuSpace",
                ),
                (
                    client_ep_commands::SHARE_REPORT_RING,
                    None,
                    "ShareReportRing",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl_service_framework!(IClientEpSession, "IClientEpSession");

pub struct IClientIfSession {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IClientIfSession {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (
                    client_if_commands::GET_STATE_CHANGE_EVENT,
                    None,
                    "GetStateChangeEvent",
                ),
                (client_if_commands::SET_INTERFACE, None, "SetInterface"),
                (client_if_commands::GET_INTERFACE, None, "GetInterface"),
                (
                    client_if_commands::GET_ALTERNATE_INTERFACE,
                    None,
                    "GetAlternateInterface",
                ),
                (
                    client_if_commands::GET_CURRENT_FRAME,
                    None,
                    "GetCurrentFrame",
                ),
                (client_if_commands::CTRL_XFER_ASYNC, None, "CtrlXferAsync"),
                (
                    client_if_commands::GET_CTRL_XFER_COMPLETION_EVENT,
                    None,
                    "GetCtrlXferCompletionEvent",
                ),
                (
                    client_if_commands::GET_CTRL_XFER_REPORT,
                    None,
                    "GetCtrlXferReport",
                ),
                (client_if_commands::RESET_DEVICE, None, "ResetDevice"),
                (client_if_commands::OPEN_USB_EP, None, "OpenUsbEp"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl_service_framework!(IClientIfSession, "IClientIfSession");

pub struct IClientRootSession {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IClientRootSession {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (
                    hs_root_commands::BIND_CLIENT_PROCESS,
                    None,
                    "BindClientProcess",
                ),
                (
                    hs_root_commands::QUERY_ALL_INTERFACES,
                    None,
                    "QueryAllInterfaces",
                ),
                (
                    hs_root_commands::QUERY_AVAILABLE_INTERFACES,
                    None,
                    "QueryAvailableInterfaces",
                ),
                (
                    hs_root_commands::QUERY_ACQUIRED_INTERFACES,
                    None,
                    "QueryAcquiredInterfaces",
                ),
                (
                    hs_root_commands::CREATE_INTERFACE_AVAILABLE_EVENT,
                    None,
                    "CreateInterfaceAvailableEvent",
                ),
                (
                    hs_root_commands::DESTROY_INTERFACE_AVAILABLE_EVENT,
                    None,
                    "DestroyInterfaceAvailableEvent",
                ),
                (
                    hs_root_commands::GET_INTERFACE_STATE_CHANGE_EVENT,
                    None,
                    "GetInterfaceStateChangeEvent",
                ),
                (hs_root_commands::ACQUIRE_USB_IF, None, "AcquireUsbIf"),
                (hs_root_commands::SET_TEST_MODE, None, "SetTestMode"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl_service_framework!(IClientRootSession, "usb:hs");

pub struct IPdSession {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IPdSession {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (
                    pd_session_commands::BIND_NOTICE_EVENT,
                    None,
                    "BindNoticeEvent",
                ),
                (
                    pd_session_commands::UNBIND_NOTICE_EVENT,
                    None,
                    "UnbindNoticeEvent",
                ),
                (pd_session_commands::GET_STATUS, None, "GetStatus"),
                (pd_session_commands::GET_NOTICE, None, "GetNotice"),
                (
                    pd_session_commands::ENABLE_POWER_REQUEST_NOTICE,
                    None,
                    "EnablePowerRequestNotice",
                ),
                (
                    pd_session_commands::DISABLE_POWER_REQUEST_NOTICE,
                    None,
                    "DisablePowerRequestNotice",
                ),
                (
                    pd_session_commands::REPLY_POWER_REQUEST,
                    None,
                    "ReplyPowerRequest",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl_service_framework!(IPdSession, "IPdSession");

/// IPdManager ("usb:pd").
pub struct IPdManager {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IPdManager {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(
                pd_commands::OPEN_SESSION,
                Some(IPdManager::open_session_handler),
                "OpenSession",
            )]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    pub fn open_session(&self) -> IPdSession {
        log::debug!("IPdManager::open_session called");
        IPdSession::new()
    }

    fn open_session_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const IPdManager) };
        let session = service.open_session();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(std::sync::Arc::new(session));
    }
}

impl_service_framework!(IPdManager, "usb:pd");

pub struct IPdCradleSession {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IPdCradleSession {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (
                    pd_cradle_session_commands::SET_CRADLE_VDO,
                    None,
                    "SetCradleVdo",
                ),
                (
                    pd_cradle_session_commands::GET_CRADLE_VDO,
                    None,
                    "GetCradleVdo",
                ),
                (
                    pd_cradle_session_commands::RESET_CRADLE_USB_HUB,
                    None,
                    "ResetCradleUsbHub",
                ),
                (
                    pd_cradle_session_commands::GET_HOST_PDC_FIRMWARE_TYPE,
                    None,
                    "GetHostPdcFirmwareType",
                ),
                (
                    pd_cradle_session_commands::GET_HOST_PDC_FIRMWARE_REVISION,
                    None,
                    "GetHostPdcFirmwareRevision",
                ),
                (
                    pd_cradle_session_commands::GET_HOST_PDC_MANUFACTURE_ID,
                    None,
                    "GetHostPdcManufactureId",
                ),
                (
                    pd_cradle_session_commands::GET_HOST_PDC_DEVICE_ID,
                    None,
                    "GetHostPdcDeviceId",
                ),
                (
                    pd_cradle_session_commands::ENABLE_CRADLE_RECOVERY,
                    None,
                    "EnableCradleRecovery",
                ),
                (
                    pd_cradle_session_commands::DISABLE_CRADLE_RECOVERY,
                    None,
                    "DisableCradleRecovery",
                ),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl_service_framework!(IPdCradleSession, "IPdCradleSession");

/// IPdCradleManager ("usb:pd:c").
pub struct IPdCradleManager {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IPdCradleManager {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(
                pd_cradle_commands::OPEN_CRADLE_SESSION,
                Some(IPdCradleManager::open_cradle_session_handler),
                "OpenCradleSession",
            )]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    pub fn open_cradle_session(&self) -> IPdCradleSession {
        log::debug!("IPdCradleManager::open_cradle_session called");
        IPdCradleSession::new()
    }

    fn open_cradle_session_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const IPdCradleManager) };
        let session = service.open_cradle_session();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(std::sync::Arc::new(session));
    }
}

impl_service_framework!(IPdCradleManager, "usb:pd:c");

/// IPmMainService ("usb:pm"). All stubs.
pub struct IPmMainService {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IPmMainService {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (pm_commands::GET_POWER_EVENT, None, "GetPowerEvent"),
                (pm_commands::GET_POWER_STATE, None, "GetPowerState"),
                (pm_commands::GET_DATA_EVENT, None, "GetDataEvent"),
                (pm_commands::GET_DATA_ROLE, None, "GetDataRole"),
                (pm_commands::SET_DIAG_DATA, None, "SetDiagData"),
                (pm_commands::GET_DIAG_DATA, None, "GetDiagData"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl_service_framework!(IPmMainService, "usb:pm");

/// Registers "usb:ds", "usb:hs", "usb:pd", "usb:pd:c", "usb:pm" services.
///
/// Corresponds to `LoopProcess` in upstream `usb.cpp`.
pub fn loop_process(system: crate::core::SystemRef) {
    use crate::hle::service::server_manager::ServerManager;

    let server_manager = ServerManager::new_shared(system);

    {
        let mut server_manager = server_manager.lock().unwrap();
        server_manager.register_named_service(
            "usb:ds",
            Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(IDsRootSession::new()) }),
            16,
        );
        server_manager.register_named_service(
            "usb:hs",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(IClientRootSession::new())
            }),
            16,
        );
        server_manager.register_named_service(
            "usb:pd",
            Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(IPdManager::new()) }),
            6,
        );
        server_manager.register_named_service(
            "usb:pd:c",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(IPdCradleManager::new())
            }),
            4,
        );
        server_manager.register_named_service(
            "usb:pm",
            Box::new(|| -> SessionRequestHandlerPtr { std::sync::Arc::new(IPmMainService::new()) }),
            5,
        );
        server_manager.register_named_service(
            "usb:pd:m",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(IPdManufactureManager::new())
            }),
            16,
        );
        let firmware_major =
            crate::hle::service::set::system_settings_server::get_firmware_version_impl(
                crate::hle::service::set::settings_types::GetFirmwareVersionType::Version1,
            )
            .major;
        if firmware_major >= 7 {
            server_manager.register_named_service(
                "usb:qdb",
                Box::new(|| -> SessionRequestHandlerPtr {
                    std::sync::Arc::new(IQdbManager::new())
                }),
                16,
            );
        }
        if crate::hle::service::set::system_settings_server::get_firmware_version_impl(
            crate::hle::service::set::settings_types::GetFirmwareVersionType::Version1,
        )
        .major
            >= 8
        {
            server_manager.register_named_service(
                "usb:obsv",
                Box::new(|| -> SessionRequestHandlerPtr {
                    std::sync::Arc::new(IPmObserverService::new())
                }),
                2,
            );
        }
    }

    ServerManager::run_server_shared(server_manager);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_root_services_expose_upstream_command_counts() {
        assert_eq!(IDsRootSession::new().handlers.len(), 1);
        assert_eq!(IClientRootSession::new().handlers.len(), 9);
        assert_eq!(IPdManager::new().handlers.len(), 1);
        assert_eq!(IPdCradleManager::new().handlers.len(), 1);
        assert_eq!(IPmMainService::new().handlers.len(), 6);
        assert_eq!(IPdManufactureManager::new().handlers.len(), 1);
        assert_eq!(IQdbManager::new().handlers.len(), 2);
        assert_eq!(IPmObserverService::new().handlers.len(), 2);
    }

    #[test]
    fn usb_nested_services_expose_upstream_command_counts() {
        assert_eq!(IDsInterface::new().handlers.len(), 13);
        assert_eq!(IClientEpSession::new().handlers.len(), 9);
        assert_eq!(IClientIfSession::new().handlers.len(), 10);
        assert_eq!(IPdSession::new().handlers.len(), 7);
        assert_eq!(IPdCradleSession::new().handlers.len(), 9);
    }
}
