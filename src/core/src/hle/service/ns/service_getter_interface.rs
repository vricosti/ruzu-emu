// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ns/service_getter_interface.h
//! Port of zuyu/src/core/hle/service/ns/service_getter_interface.cpp
//!
//! IServiceGetterInterface dispatches to sub-interfaces.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::application_manager_interface::IApplicationManagerInterface;
use super::content_management_interface::IContentManagementInterface;
use super::document_interface::IDocumentInterface;
use super::ecommerce_interface::IECommerceInterface;
use super::download_task_interface::IDownloadTaskInterface;
use super::dynamic_rights_interface::IDynamicRightsInterface;
use super::read_only_application_record_interface::IReadOnlyApplicationRecordInterface;
use super::read_only_application_control_data_interface::IReadOnlyApplicationControlDataInterface;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for IServiceGetterInterface.
///
/// Corresponds to the function table in upstream service_getter_interface.cpp.
pub mod commands {
    pub const GET_DYNAMIC_RIGHTS_INTERFACE: u32 = 7988;
    pub const GET_READ_ONLY_APPLICATION_CONTROL_DATA_INTERFACE: u32 = 7989;
    pub const GET_READ_ONLY_APPLICATION_RECORD_INTERFACE: u32 = 7991;
    pub const GET_ECOMMERCE_INTERFACE: u32 = 7992;
    pub const GET_APPLICATION_VERSION_INTERFACE: u32 = 7993;
    pub const GET_FACTORY_RESET_INTERFACE: u32 = 7994;
    pub const GET_ACCOUNT_PROXY_INTERFACE: u32 = 7995;
    pub const GET_APPLICATION_MANAGER_INTERFACE: u32 = 7996;
    pub const GET_DOWNLOAD_TASK_INTERFACE: u32 = 7997;
    pub const GET_CONTENT_MANAGEMENT_INTERFACE: u32 = 7998;
    pub const GET_DOCUMENT_INTERFACE: u32 = 7999;
}

/// IServiceGetterInterface — dispatches to sub-interfaces for NS.
///
/// Corresponds to `IServiceGetterInterface` in upstream.
/// Each Get*Interface method creates and returns a new sub-interface object.
pub struct IServiceGetterInterface {
    system: crate::core::SystemRef,
    service_name: &'static str,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IServiceGetterInterface {
    pub fn new(system: crate::core::SystemRef, service_name: &'static str) -> Self {
        let handlers = build_handler_map(&[
            (
                commands::GET_DYNAMIC_RIGHTS_INTERFACE,
                Some(Self::get_dynamic_rights_interface_handler),
                "GetDynamicRightsInterface",
            ),
            (
                commands::GET_READ_ONLY_APPLICATION_CONTROL_DATA_INTERFACE,
                Some(Self::get_read_only_application_control_data_interface_handler),
                "GetReadOnlyApplicationControlDataInterface",
            ),
            (
                commands::GET_READ_ONLY_APPLICATION_RECORD_INTERFACE,
                Some(Self::get_read_only_application_record_interface_handler),
                "GetReadOnlyApplicationRecordInterface",
            ),
            (
                commands::GET_ECOMMERCE_INTERFACE,
                Some(Self::get_ecommerce_interface_handler),
                "GetECommerceInterface",
            ),
            (
                commands::GET_APPLICATION_VERSION_INTERFACE,
                None,
                "GetApplicationVersionInterface",
            ),
            (
                commands::GET_FACTORY_RESET_INTERFACE,
                None,
                "GetFactoryResetInterface",
            ),
            (
                commands::GET_ACCOUNT_PROXY_INTERFACE,
                None,
                "GetAccountProxyInterface",
            ),
            (
                commands::GET_APPLICATION_MANAGER_INTERFACE,
                Some(Self::get_application_manager_interface_handler),
                "GetApplicationManagerInterface",
            ),
            (
                commands::GET_DOWNLOAD_TASK_INTERFACE,
                Some(Self::get_download_task_interface_handler),
                "GetDownloadTaskInterface",
            ),
            (
                commands::GET_CONTENT_MANAGEMENT_INTERFACE,
                Some(Self::get_content_management_interface_handler),
                "GetContentManagementInterface",
            ),
            (
                commands::GET_DOCUMENT_INTERFACE,
                Some(Self::get_document_interface_handler),
                "GetDocumentInterface",
            ),
        ]);
        Self {
            system,
            service_name,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// GetDynamicRightsInterface (cmd 7988).
    pub fn get_dynamic_rights_interface(&self) -> IDynamicRightsInterface {
        log::debug!("IServiceGetterInterface::get_dynamic_rights_interface called");
        IDynamicRightsInterface::new()
    }

    fn get_dynamic_rights_interface_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let interface = Arc::new(service.get_dynamic_rights_interface());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(interface);
    }

    /// GetReadOnlyApplicationControlDataInterface (cmd 7989).
    pub fn get_read_only_application_control_data_interface(&self) -> IReadOnlyApplicationControlDataInterface {
        log::debug!(
            "IServiceGetterInterface::get_read_only_application_control_data_interface called"
        );
        IReadOnlyApplicationControlDataInterface::new(self.system)
    }

    /// GetReadOnlyApplicationRecordInterface (cmd 7991).
    pub fn get_read_only_application_record_interface(&self) -> IReadOnlyApplicationRecordInterface {
        log::debug!("IServiceGetterInterface::get_read_only_application_record_interface called");
        IReadOnlyApplicationRecordInterface::new(self.system)
    }

    fn get_read_only_application_control_data_interface_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let interface = Arc::new(service.get_read_only_application_control_data_interface());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(interface);
    }

    fn get_read_only_application_record_interface_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let interface = Arc::new(service.get_read_only_application_record_interface());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(interface);
    }

    /// GetECommerceInterface (cmd 7992).
    pub fn get_ecommerce_interface(&self) -> IECommerceInterface {
        log::debug!("IServiceGetterInterface::get_ecommerce_interface called");
        IECommerceInterface::new()
    }

    fn get_ecommerce_interface_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IServiceGetterInterface) };
        let interface = Arc::new(service.get_ecommerce_interface());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(interface);
    }

    /// GetApplicationVersionInterface (cmd 7993).
    pub fn get_application_version_interface(&self) {
        log::debug!("IServiceGetterInterface::get_application_version_interface called");
    }

    /// GetFactoryResetInterface (cmd 7994).
    pub fn get_factory_reset_interface(&self) {
        log::debug!("IServiceGetterInterface::get_factory_reset_interface called");
    }

    /// GetAccountProxyInterface (cmd 7995).
    pub fn get_account_proxy_interface(&self) {
        log::debug!("IServiceGetterInterface::get_account_proxy_interface called");
    }

    /// GetApplicationManagerInterface (cmd 7996).
    pub fn get_application_manager_interface(&self) -> IApplicationManagerInterface {
        log::debug!("IServiceGetterInterface::get_application_manager_interface called");
        IApplicationManagerInterface::new(self.system)
    }

    fn get_application_manager_interface_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IServiceGetterInterface) };
        let interface = Arc::new(service.get_application_manager_interface());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(interface);
    }

    /// GetDownloadTaskInterface (cmd 7997).
    pub fn get_download_task_interface(&self) -> IDownloadTaskInterface {
        log::debug!("IServiceGetterInterface::get_download_task_interface called");
        IDownloadTaskInterface::new()
    }

    fn get_download_task_interface_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let interface = Arc::new(service.get_download_task_interface());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(interface);
    }

    fn get_content_management_interface_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IServiceGetterInterface) };
        let interface = Arc::new(service.get_content_management_interface());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(interface);
    }

    /// GetContentManagementInterface (cmd 7998).
    pub fn get_content_management_interface(&self) -> IContentManagementInterface {
        log::debug!("IServiceGetterInterface::get_content_management_interface called");
        IContentManagementInterface::new(self.system)
    }

    /// GetDocumentInterface (cmd 7999).
    pub fn get_document_interface(&self) -> IDocumentInterface {
        log::debug!("IServiceGetterInterface::get_document_interface called");
        IDocumentInterface::new(self.system)
    }

    fn get_document_interface_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = this.as_any().downcast_ref::<Self>().unwrap();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(Arc::new(service.get_document_interface()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_getter_returns_session_with_upstream_replies() {
        use crate::core::{System, SystemRef};
        use crate::hle::kernel::k_process::{KProcess, ProcessLock};
        use crate::hle::kernel::k_thread::{KThread, KThreadLock};
        use crate::hle::service::hle_ipc::{KAutoObjectRef, SessionRequestManager};
        use std::sync::Mutex;

        let mut system = Box::new(System::new());
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let application_id = 0x0100_1234_5678_ABCD;
        process.lock().unwrap().program_id = application_id;
        system.set_current_process_arc(process.clone());
        system.set_runtime_program_id(0xDEAD); // not the application process ID
        let service = IServiceGetterInterface::new(SystemRef::from_ref(&system), "ns:am2");
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        let mut ctx = HLERequestContext::new_with_thread(thread, 0);
        ctx.set_session_request_manager(Arc::new(Mutex::new(SessionRequestManager::new())));
        service.handlers()[&7999].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(ctx.outgoing_move_objects.len(), 1);
        let KAutoObjectRef::ObjectId(id) = ctx.outgoing_move_objects[0] else { panic!("missing child session") };
        let server = process.lock().unwrap().get_server_session_by_object_id(id).unwrap();
        let manager = server.lock().unwrap().get_manager().unwrap().clone();
        let child = manager.lock().unwrap().session_handler().unwrap().clone();
        let document = child.as_any().downcast_ref::<IDocumentInterface>().unwrap();
        assert_eq!(document.handlers().len(), 3);
        assert!(document.handlers()[&21].handler_callback.is_some());
        for command in [23, 92] {
            let mut ctx = HLERequestContext::new();
            // Exercise full-width caller/path input and nonzero ContentPath padding.
            ctx.command_buffer_mut().fill(u32::MAX);
            document.handlers()[&command].handler_callback.unwrap()(document, &mut ctx);
            let offset = ctx.get_data_payload_offset() as usize;
            assert_eq!(&ctx.command_buffer()[offset..offset + 2], &[0, 0]);
            assert_eq!(ctx.write_size as usize, offset + if command == 23 { 2 } else { 4 });
            if command == 92 {
                let words = ctx.command_buffer();
                assert_eq!(u64::from(words[offset + 2]) | (u64::from(words[offset + 3]) << 32), application_id);
            }
        }
        assert_eq!(std::mem::size_of::<super::super::ns_types::ContentPath>(), 16);
        assert_eq!(std::mem::offset_of!(super::super::ns_types::ContentPath, program_id), 8);
    }

    #[test]
    fn read_only_getters_return_wired_interfaces() {
        let service = IServiceGetterInterface::new(crate::core::SystemRef::null(), "ns:am2");
        for command in [7989, 7991] {
            assert!(service.handlers[&command].handler_callback.is_some());
        }
        let record = service.get_read_only_application_record_interface();
        assert!(record.handlers()[&3].handler_callback.is_some());
        for (command, expected) in [(0, 1), (2, 0)] {
            let mut ctx = HLERequestContext::new();
            record.handlers()[&command].handler_callback.unwrap()(&record, &mut ctx);
            assert_eq!(ctx.command_buffer()[6], 0);
            assert_eq!(ctx.command_buffer()[8], expected);
        }
        assert!(record.handlers()[&1].handler_callback.is_none());
        let control = service.get_read_only_application_control_data_interface();
        for command in [0, 1, 2, 5, 10, 13, 19, 23] {
            assert!(control.handlers()[&command].handler_callback.is_some());
        }
    }

    #[test]
    fn dynamic_rights_getter_returns_upstream_interface() {
        let service = IServiceGetterInterface::new(crate::core::SystemRef::null(), "ns:am2");
        assert!(service.handlers[&7988].handler_callback.is_some());
        assert_eq!(service.get_dynamic_rights_interface().handlers().len(), 29);
    }

    #[test]
    fn download_task_getter_returns_upstream_commands() {
        let service = IServiceGetterInterface::new(crate::core::SystemRef::null(), "ns:am2");
        assert!(service.handlers[&7997].handler_callback.is_some());
        let child = service.get_download_task_interface();
        assert_eq!(child.handlers().len(), 9);
        for command in 701..=709 {
            let handler = child.handlers()[&command].handler_callback;
            assert_eq!(handler.is_some(), matches!(command, 707 | 708));
            if let Some(handler) = handler {
                let mut ctx = HLERequestContext::new();
                handler(&child, &mut ctx);
                assert_eq!(ctx.command_buffer()[6], 0);
            }
        }
    }

    #[test]
    fn content_management_getter_has_upstream_handler() {
        let service = IServiceGetterInterface::new(crate::core::SystemRef::null(), "ns:am2");
        assert!(service
            .handlers()
            .get(&commands::GET_CONTENT_MANAGEMENT_INTERFACE)
            .and_then(|info| info.handler_callback)
            .is_some());
    }

    #[test]
    fn ecommerce_getter_has_upstream_handler_and_returns_exact_child_table() {
        let service = IServiceGetterInterface::new(crate::core::SystemRef::null(), "ns:am2");
        assert!(service
            .handlers()
            .get(&commands::GET_ECOMMERCE_INTERFACE)
            .and_then(|info| info.handler_callback)
            .is_some());
        assert_eq!(service.get_ecommerce_interface().handlers().len(), 7);
    }

    #[test]
    fn application_manager_getter_returns_the_upstream_child_interface() {
        let service = IServiceGetterInterface::new(crate::core::SystemRef::null(), "ns:am2");
        assert!(
            service.handlers[&commands::GET_APPLICATION_MANAGER_INTERFACE]
                .handler_callback
                .is_some()
        );
        assert_eq!(
            service.get_application_manager_interface().handlers().len(),
            crate::hle::service::ns::application_manager_interface::IAPPLICATION_MANAGER_INTERFACE_COMMANDS.len()
        );
    }
}

impl SessionRequestHandler for IServiceGetterInterface {
    fn as_any(&self) -> &dyn std::any::Any { self }
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        self.service_name
    }
}

impl ServiceFramework for IServiceGetterInterface {
    fn get_service_name(&self) -> &str {
        self.service_name
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
