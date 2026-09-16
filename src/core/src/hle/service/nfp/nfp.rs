// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/nfp/nfp.h
//! Port of zuyu/src/core/hle/service/nfp/nfp.cpp
//!
//! NFP service — "nfp:user", "nfp:sys", "nfp:dbg" service registration.
//!
//! Upstream nfp.cpp defines IUser, ISystem, IDebug classes (each deriving from Interface),
//! plus IUserManager, ISystemManager, IDebugManager service entry points.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{
    HLERequestContext, SessionRequestHandler, SessionRequestHandlerPtr,
};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for Interface (IUser / ISystem / IDebug).
///
/// Corresponds to the function table in upstream nfp.cpp constructors.
pub mod interface_commands {
    pub const INITIALIZE: u32 = 0;
    pub const FINALIZE: u32 = 1;
    pub const LIST_DEVICES: u32 = 2;
    pub const START_DETECTION: u32 = 3;
    pub const STOP_DETECTION: u32 = 4;
    pub const MOUNT: u32 = 5;
    pub const UNMOUNT: u32 = 6;
    pub const OPEN_APPLICATION_AREA: u32 = 7;
    pub const GET_APPLICATION_AREA: u32 = 8;
    pub const SET_APPLICATION_AREA: u32 = 9;
    pub const FLUSH: u32 = 10;
    pub const RESTORE: u32 = 11;
    pub const CREATE_APPLICATION_AREA: u32 = 12;
    pub const GET_TAG_INFO: u32 = 13;
    pub const GET_REGISTER_INFO: u32 = 14;
    pub const GET_COMMON_INFO: u32 = 15;
    pub const GET_MODEL_INFO: u32 = 16;
    pub const ATTACH_ACTIVATE_EVENT: u32 = 17;
    pub const ATTACH_DEACTIVATE_EVENT: u32 = 18;
    pub const GET_STATE: u32 = 19;
    pub const GET_DEVICE_STATE: u32 = 20;
    pub const GET_NFC_NPAD_ID: u32 = 21;
    pub const GET_APPLICATION_AREA_SIZE: u32 = 22;
    pub const ATTACH_AVAILABILITY_CHANGE_EVENT: u32 = 23;
    pub const RECREATE_APPLICATION_AREA: u32 = 24;
    pub const START_DETECTION_WITH_FILTER: u32 = 25;
    // System/Debug commands
    pub const FORMAT: u32 = 100;
    pub const GET_ADMIN_INFO: u32 = 101;
    pub const GET_REGISTER_INFO_PRIVATE: u32 = 102;
    pub const SET_REGISTER_INFO_PRIVATE: u32 = 103;
    pub const DELETE_REGISTER_INFO: u32 = 104;
    pub const DELETE_APPLICATION_AREA: u32 = 105;
    pub const EXISTS_APPLICATION_AREA: u32 = 106;
    // Debug commands
    pub const GET_ALL: u32 = 200;
    pub const SET_ALL: u32 = 201;
    pub const FLUSH_DEBUG: u32 = 202;
    pub const BREAK_TAG: u32 = 203;
    pub const READ_BACKUP_DATA: u32 = 204;
    pub const WRITE_BACKUP_DATA: u32 = 205;
    pub const WRITE_NTF: u32 = 206;
    // InitializeSystem/Debug/Finalize commands
    pub const INITIALIZE_SYSTEM: u32 = 400;
    pub const FINALIZE_SYSTEM: u32 = 401;
    pub const INITIALIZE_DEBUG: u32 = 500;
    pub const FINALIZE_DEBUG: u32 = 501;
}

// Rust shares the inherited Interface handlers. Keep registration in nfp.rs,
// the owner of the upstream IUser/ISystem/IDebug constructor tables.
impl super::nfp_interface::Interface {
    pub(super) fn make_handlers() -> BTreeMap<u32, FunctionInfo> {
        build_handler_map(&[
            (
                interface_commands::INITIALIZE,
                Some(Self::initialize_handler),
                "Initialize",
            ),
            (interface_commands::FINALIZE, Some(Self::finalize_handler), "Finalize"),
            (
                interface_commands::LIST_DEVICES,
                Some(Self::list_devices_handler),
                "ListDevices",
            ),
            (
                interface_commands::START_DETECTION,
                Some(Self::start_detection_handler),
                "StartDetection",
            ),
            (
                interface_commands::STOP_DETECTION,
                Some(Self::stop_detection_handler),
                "StopDetection",
            ),
            (interface_commands::MOUNT, Some(Self::mount_handler), "Mount"),
            (interface_commands::UNMOUNT, Some(Self::unmount_handler), "Unmount"),
            (
                interface_commands::OPEN_APPLICATION_AREA,
                Some(Self::open_application_area_handler),
                "OpenApplicationArea",
            ),
            (
                interface_commands::GET_APPLICATION_AREA,
                Some(Self::get_application_area_handler),
                "GetApplicationArea",
            ),
            (
                interface_commands::SET_APPLICATION_AREA,
                Some(Self::set_application_area_handler),
                "SetApplicationArea",
            ),
            (interface_commands::FLUSH, Some(Self::flush_handler), "Flush"),
            (interface_commands::RESTORE, Some(Self::restore_handler), "Restore"),
            (
                interface_commands::CREATE_APPLICATION_AREA,
                Some(Self::create_application_area_handler),
                "CreateApplicationArea",
            ),
            (
                interface_commands::GET_TAG_INFO,
                Some(Self::get_tag_info_handler),
                "GetTagInfo",
            ),
            (
                interface_commands::GET_REGISTER_INFO,
                Some(Self::get_register_info_handler),
                "GetRegisterInfo",
            ),
            (
                interface_commands::GET_COMMON_INFO,
                Some(Self::get_common_info_handler),
                "GetCommonInfo",
            ),
            (
                interface_commands::GET_MODEL_INFO,
                Some(Self::get_model_info_handler),
                "GetModelInfo",
            ),
            (
                interface_commands::ATTACH_ACTIVATE_EVENT,
                Some(Self::attach_activate_event_handler),
                "AttachActivateEvent",
            ),
            (
                interface_commands::ATTACH_DEACTIVATE_EVENT,
                Some(Self::attach_deactivate_event_handler),
                "AttachDeactivateEvent",
            ),
            (
                interface_commands::GET_STATE,
                Some(Self::get_state_handler),
                "GetState",
            ),
            (
                interface_commands::GET_DEVICE_STATE,
                Some(Self::get_device_state_handler),
                "GetDeviceState",
            ),
            (
                interface_commands::GET_NFC_NPAD_ID,
                Some(Self::get_npad_id_handler),
                "GetNpadId",
            ),
            (
                interface_commands::GET_APPLICATION_AREA_SIZE,
                Some(Self::get_application_area_size_handler),
                "GetApplicationAreaSize",
            ),
            (
                interface_commands::ATTACH_AVAILABILITY_CHANGE_EVENT,
                Some(Self::attach_availability_change_event_handler),
                "AttachAvailabilityChangeEvent",
            ),
            (
                interface_commands::RECREATE_APPLICATION_AREA,
                Some(Self::recreate_application_area_handler),
                "RecreateApplicationArea",
            ),
            (interface_commands::START_DETECTION_WITH_FILTER, Some(Self::start_detection_handler), "StartDetectionWithFilter"),
        ])
    }
}

/// IPC command table for IUserManager.
pub mod user_manager_commands {
    pub const CREATE_USER_INTERFACE: u32 = 0;
}

/// IPC command table for ISystemManager.
pub mod system_manager_commands {
    pub const CREATE_SYSTEM_INTERFACE: u32 = 0;
}

/// IPC command table for IDebugManager.
pub mod debug_manager_commands {
    pub const CREATE_DEBUG_INTERFACE: u32 = 0;
}

/// IPC command table for IUser (from nfp.cpp).
///
/// Corresponds to the function table in `IUser` in upstream nfp.cpp.
pub mod iuser_commands {
    pub const INITIALIZE: u32 = 0;
    pub const FINALIZE: u32 = 1;
    pub const LIST_DEVICES: u32 = 2;
    pub const START_DETECTION: u32 = 3;
    pub const STOP_DETECTION: u32 = 4;
    pub const MOUNT: u32 = 5;
    pub const UNMOUNT: u32 = 6;
    pub const OPEN_APPLICATION_AREA: u32 = 7;
    pub const GET_APPLICATION_AREA: u32 = 8;
    pub const SET_APPLICATION_AREA: u32 = 9;
    pub const FLUSH: u32 = 10;
    pub const RESTORE: u32 = 11;
    pub const CREATE_APPLICATION_AREA: u32 = 12;
    pub const GET_TAG_INFO: u32 = 13;
    pub const GET_REGISTER_INFO: u32 = 14;
    pub const GET_COMMON_INFO: u32 = 15;
    pub const GET_MODEL_INFO: u32 = 16;
    pub const ATTACH_ACTIVATE_EVENT: u32 = 17;
    pub const ATTACH_DEACTIVATE_EVENT: u32 = 18;
    pub const GET_STATE: u32 = 19;
    pub const GET_DEVICE_STATE: u32 = 20;
    pub const GET_NPAD_ID: u32 = 21;
    pub const GET_APPLICATION_AREA_SIZE: u32 = 22;
    pub const ATTACH_AVAILABILITY_CHANGE_EVENT: u32 = 23;
    pub const RECREATE_APPLICATION_AREA: u32 = 24;
    pub const START_DETECTION_WITH_FILTER: u32 = 25;
}

/// IPC command table for ISystem (from nfp.cpp).
///
/// Corresponds to the function table in `ISystem` in upstream nfp.cpp.
pub mod isystem_commands {
    pub const INITIALIZE_SYSTEM: u32 = 0;
    pub const FINALIZE_SYSTEM: u32 = 1;
    pub const LIST_DEVICES: u32 = 2;
    pub const START_DETECTION: u32 = 3;
    pub const STOP_DETECTION: u32 = 4;
    pub const MOUNT: u32 = 5;
    pub const UNMOUNT: u32 = 6;
    pub const FLUSH: u32 = 10;
    pub const RESTORE: u32 = 11;
    pub const CREATE_APPLICATION_AREA: u32 = 12;
    pub const GET_TAG_INFO: u32 = 13;
    pub const GET_REGISTER_INFO: u32 = 14;
    pub const GET_COMMON_INFO: u32 = 15;
    pub const GET_MODEL_INFO: u32 = 16;
    pub const ATTACH_ACTIVATE_EVENT: u32 = 17;
    pub const ATTACH_DEACTIVATE_EVENT: u32 = 18;
    pub const GET_STATE: u32 = 19;
    pub const GET_DEVICE_STATE: u32 = 20;
    pub const GET_NPAD_ID: u32 = 21;
    pub const ATTACH_AVAILABILITY_CHANGE_EVENT: u32 = 23;
    pub const START_DETECTION_WITH_FILTER: u32 = 25;
    pub const FORMAT: u32 = 100;
    pub const GET_ADMIN_INFO: u32 = 101;
    pub const GET_REGISTER_INFO_PRIVATE: u32 = 102;
    pub const SET_REGISTER_INFO_PRIVATE: u32 = 103;
    pub const DELETE_REGISTER_INFO: u32 = 104;
    pub const DELETE_APPLICATION_AREA: u32 = 105;
    pub const EXISTS_APPLICATION_AREA: u32 = 106;
}

/// IPC command table for IDebug (from nfp.cpp).
///
/// Corresponds to the function table in `IDebug` in upstream nfp.cpp.
pub mod idebug_commands {
    pub const INITIALIZE_DEBUG: u32 = 0;
    pub const FINALIZE_DEBUG: u32 = 1;
    pub const LIST_DEVICES: u32 = 2;
    pub const START_DETECTION: u32 = 3;
    pub const STOP_DETECTION: u32 = 4;
    pub const MOUNT: u32 = 5;
    pub const UNMOUNT: u32 = 6;
    pub const OPEN_APPLICATION_AREA: u32 = 7;
    pub const GET_APPLICATION_AREA: u32 = 8;
    pub const SET_APPLICATION_AREA: u32 = 9;
    pub const FLUSH: u32 = 10;
    pub const RESTORE: u32 = 11;
    pub const CREATE_APPLICATION_AREA: u32 = 12;
    pub const GET_TAG_INFO: u32 = 13;
    pub const GET_REGISTER_INFO: u32 = 14;
    pub const GET_COMMON_INFO: u32 = 15;
    pub const GET_MODEL_INFO: u32 = 16;
    pub const ATTACH_ACTIVATE_EVENT: u32 = 17;
    pub const ATTACH_DEACTIVATE_EVENT: u32 = 18;
    pub const GET_STATE: u32 = 19;
    pub const GET_DEVICE_STATE: u32 = 20;
    pub const GET_NPAD_ID: u32 = 21;
    pub const GET_APPLICATION_AREA_SIZE: u32 = 22;
    pub const ATTACH_AVAILABILITY_CHANGE_EVENT: u32 = 23;
    pub const RECREATE_APPLICATION_AREA: u32 = 24;
    pub const START_DETECTION_WITH_FILTER: u32 = 25;
    pub const FORMAT: u32 = 100;
    pub const GET_ADMIN_INFO: u32 = 101;
    pub const GET_REGISTER_INFO_PRIVATE: u32 = 102;
    pub const SET_REGISTER_INFO_PRIVATE: u32 = 103;
    pub const DELETE_REGISTER_INFO: u32 = 104;
    pub const DELETE_APPLICATION_AREA: u32 = 105;
    pub const EXISTS_APPLICATION_AREA: u32 = 106;
    pub const GET_ALL: u32 = 200;
    pub const SET_ALL: u32 = 201;
    pub const FLUSH_DEBUG: u32 = 202;
    pub const BREAK_TAG: u32 = 203;
    pub const READ_BACKUP_DATA: u32 = 204;
    pub const WRITE_BACKUP_DATA: u32 = 205;
    pub const WRITE_NTF: u32 = 206;
}

/// IUserManager — entry point for "nfp:user".
///
/// Corresponds to `IUserManager` in upstream nfp.cpp.
pub struct IUserManager {
    system: crate::core::SystemRef,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IUserManager {
    pub fn new(system: crate::core::SystemRef) -> Self {
        Self {
            system,
            handlers: build_handler_map(&[(
                user_manager_commands::CREATE_USER_INTERFACE,
                Some(Self::create_user_interface_handler),
                "CreateUserInterface",
            )]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// CreateUserInterface (cmd 0).
    pub fn create_user_interface(&self) -> super::nfp_interface::Interface {
        log::debug!("IUserManager::create_user_interface called");
        super::nfp_interface::Interface::new(self.system, "NFP:IUser")
    }

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    fn create_user_interface_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let object: Arc<dyn SessionRequestHandler> = Arc::new(service.create_user_interface());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(object);
    }
}

impl SessionRequestHandler for IUserManager {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "nfp:user"
    }
}

impl ServiceFramework for IUserManager {
    fn get_service_name(&self) -> &str {
        "nfp:user"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// ISystemManager — entry point for "nfp:sys".
///
/// Corresponds to `ISystemManager` in upstream nfp.cpp.
pub struct ISystemManager {
    system: crate::core::SystemRef,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ISystemManager {
    pub fn new(system: crate::core::SystemRef) -> Self {
        Self {
            system,
            handlers: build_handler_map(&[(
                system_manager_commands::CREATE_SYSTEM_INTERFACE,
                Some(Self::create_system_interface_handler),
                "CreateSystemInterface",
            )]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// CreateSystemInterface (cmd 0).
    ///
    /// Upstream creates an ISystem (which derives from Interface with name "NFP:ISystem").
    pub fn create_system_interface(&self) -> super::nfp_interface::Interface {
        log::debug!("ISystemManager::create_system_interface called");
        super::nfp_interface::Interface::new(self.system, "NFP:ISystem")
    }

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    fn create_system_interface_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let object: Arc<dyn SessionRequestHandler> = Arc::new(service.create_system_interface());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(object);
    }
}

impl SessionRequestHandler for ISystemManager {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "nfp:sys"
    }
}

impl ServiceFramework for ISystemManager {
    fn get_service_name(&self) -> &str {
        "nfp:sys"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// IDebugManager — entry point for "nfp:dbg".
///
/// Corresponds to `IDebugManager` in upstream nfp.cpp.
pub struct IDebugManager {
    system: crate::core::SystemRef,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IDebugManager {
    pub fn new(system: crate::core::SystemRef) -> Self {
        Self {
            system,
            handlers: build_handler_map(&[(
                debug_manager_commands::CREATE_DEBUG_INTERFACE,
                Some(Self::create_debug_interface_handler),
                "CreateDebugInterface",
            )]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// CreateDebugInterface (cmd 0).
    ///
    /// Upstream creates an IDebug (which derives from Interface with name "NFP:IDebug").
    pub fn create_debug_interface(&self) -> super::nfp_interface::Interface {
        log::debug!("IDebugManager::create_debug_interface called");
        super::nfp_interface::Interface::new(self.system, "NFP:IDebug")
    }

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    fn create_debug_interface_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let object: Arc<dyn SessionRequestHandler> = Arc::new(service.create_debug_interface());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(object);
    }
}

impl SessionRequestHandler for IDebugManager {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "nfp:dbg"
    }
}

impl ServiceFramework for IDebugManager {
    fn get_service_name(&self) -> &str {
        "nfp:dbg"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// LoopProcess — registers "nfp:user", "nfp:sys", "nfp:dbg" services.
///
/// Corresponds to `Service::NFP::LoopProcess` in upstream nfp.cpp.
pub fn loop_process(system: crate::core::SystemRef) {
    use crate::hle::service::server_manager::ServerManager;

    log::debug!("NFP::LoopProcess called");

    let server_manager = ServerManager::new_shared(system);
    {
        let mut server_manager = server_manager.lock().unwrap();
        server_manager.register_named_service(
            "nfp:user",
            Box::new(move || -> SessionRequestHandlerPtr { Arc::new(IUserManager::new(system)) }),
            64,
        );
        server_manager.register_named_service(
            "nfp:sys",
            Box::new(move || -> SessionRequestHandlerPtr { Arc::new(ISystemManager::new(system)) }),
            64,
        );
        server_manager.register_named_service(
            "nfp:dbg",
            Box::new(move || -> SessionRequestHandlerPtr { Arc::new(IDebugManager::new(system)) }),
            64,
        );
    }
    ServerManager::run_server_shared(server_manager);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filtered_detection_uses_the_existing_detection_handler_on_all_interfaces() {
        let system = crate::core::SystemRef::null();
        for interface in [
            IUserManager::new(system).create_user_interface(),
            ISystemManager::new(system).create_system_interface(),
            IDebugManager::new(system).create_debug_interface(),
        ] {
            let original = interface.handlers()[&3].handler_callback.unwrap();
            let filtered = &interface.handlers()[&25];
            assert_eq!(filtered.name, "StartDetectionWithFilter");
            assert!(std::ptr::fn_addr_eq(original, filtered.handler_callback.unwrap()));
        }
    }

    #[test]
    fn manager_handler_tables_match_upstream() {
        let system = crate::core::SystemRef::null();
        assert_eq!(IUserManager::new(system).handlers().len(), 1);
        assert_eq!(ISystemManager::new(system).handlers().len(), 1);
        assert_eq!(IDebugManager::new(system).handlers().len(), 1);
    }
}
