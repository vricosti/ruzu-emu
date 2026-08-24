// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/hle/service/bcat/service_creator.h
//! Port of zuyu/src/core/hle/service/bcat/service_creator.cpp
//!
//! IServiceCreator: factory for BCAT sub-services.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::backend::{BcatBackend, NullBcatBackend};
use super::bcat_service::IBcatService;
use super::delivery_cache_storage_service::IDeliveryCacheStorageService;
use crate::core::SystemRef;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::cmif_serialization::{CmifRequest, CmifResponse};
use crate::hle::service::filesystem::filesystem::FileSystemController;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command IDs for IServiceCreator
pub mod commands {
    pub const CREATE_BCAT_SERVICE: u32 = 0;
    pub const CREATE_DELIVERY_CACHE_STORAGE_SERVICE: u32 = 1;
    pub const CREATE_DELIVERY_CACHE_STORAGE_SERVICE_WITH_APPLICATION_ID: u32 = 2;
    pub const CREATE_DELIVERY_CACHE_PROGRESS_SERVICE: u32 = 3;
    pub const CREATE_DELIVERY_CACHE_PROGRESS_SERVICE_WITH_APPLICATION_ID: u32 = 4;
}

/// IServiceCreator corresponds to `IServiceCreator` in upstream `service_creator.h`.
pub struct IServiceCreator {
    pub service_name: String,
    system: SystemRef,
    backend: Arc<Mutex<dyn BcatBackend + Send>>,
    fsc: Arc<Mutex<FileSystemController>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IServiceCreator {
    pub fn new(system: SystemRef, name: &str) -> Self {
        let handlers = build_handler_map(&[
            (
                commands::CREATE_BCAT_SERVICE,
                Some(Self::create_bcat_service_handler),
                "CreateBcatService",
            ),
            (
                commands::CREATE_DELIVERY_CACHE_STORAGE_SERVICE,
                Some(Self::create_delivery_cache_storage_service_handler),
                "CreateDeliveryCacheStorageService",
            ),
            (
                commands::CREATE_DELIVERY_CACHE_STORAGE_SERVICE_WITH_APPLICATION_ID,
                Some(Self::create_delivery_cache_storage_service_with_application_id_handler),
                "CreateDeliveryCacheStorageServiceWithApplicationId",
            ),
            (
                commands::CREATE_DELIVERY_CACHE_PROGRESS_SERVICE,
                None,
                "CreateDeliveryCacheProgressService",
            ),
            (
                commands::CREATE_DELIVERY_CACHE_PROGRESS_SERVICE_WITH_APPLICATION_ID,
                None,
                "CreateDeliveryCacheProgressServiceWithApplicationId",
            ),
        ]);

        // Upstream: backend = CreateBackendFromSettings(system_, [this](u64 tid) { return fsc.GetBCATDirectory(tid); });
        // CreateBackendFromSettings always creates NullBcatBackend.
        let backend: Arc<Mutex<dyn BcatBackend + Send>> =
            Arc::new(Mutex::new(NullBcatBackend::new()));
        let fsc = system.get().get_filesystem_controller();

        Self {
            service_name: name.to_string(),
            system,
            backend,
            fsc,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    pub fn create_bcat_service(&self, process_id: u64) -> (ResultCode, Arc<IBcatService>) {
        log::info!(
            "IServiceCreator::create_bcat_service called, process_id={}",
            process_id
        );
        let service = Arc::new(IBcatService::new(self.backend.clone()));
        (RESULT_SUCCESS, service)
    }

    fn create_bcat_service_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let (result, bcat_service) = service.create_bcat_service(ctx.get_pid());

        let mut response = CmifResponse::new(ctx, 2, 0, 1);
        response.push_result(result);
        response.push_interface(bcat_service);
    }

    pub fn create_delivery_cache_storage_service(
        &self,
        process_id: u64,
    ) -> (ResultCode, Arc<IDeliveryCacheStorageService>) {
        log::info!(
            "IServiceCreator::create_delivery_cache_storage_service called, process_id={}",
            process_id
        );
        let title_id = self.system.get().runtime_program_id();
        let root = self
            .fsc
            .lock()
            .unwrap()
            .get_bcat_directory(title_id)
            .expect("BCAT directory must be available after filesystem initialization");
        let service = Arc::new(IDeliveryCacheStorageService::new(root));
        (RESULT_SUCCESS, service)
    }

    fn create_delivery_cache_storage_service_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let (result, storage_service) =
            service.create_delivery_cache_storage_service(ctx.get_pid());

        let mut response = CmifResponse::new(ctx, 2, 0, 1);
        response.push_result(result);
        response.push_interface(storage_service);
    }

    pub fn create_delivery_cache_storage_service_with_application_id(
        &self,
        application_id: u64,
    ) -> (ResultCode, Arc<IDeliveryCacheStorageService>) {
        log::debug!(
            "IServiceCreator::create_delivery_cache_storage_service_with_application_id called, application_id={:016X}",
            application_id
        );
        let root = self
            .fsc
            .lock()
            .unwrap()
            .get_bcat_directory(application_id)
            .expect("BCAT directory must be available after filesystem initialization");
        let service = Arc::new(IDeliveryCacheStorageService::new(root));
        (RESULT_SUCCESS, service)
    }

    fn create_delivery_cache_storage_service_with_application_id_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let application_id = CmifRequest::new(ctx).u64();
        let (result, storage_service) =
            service.create_delivery_cache_storage_service_with_application_id(application_id);

        let mut response = CmifResponse::new(ctx, 2, 0, 1);
        response.push_result(result);
        response.push_interface(storage_service);
    }
}

impl SessionRequestHandler for IServiceCreator {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        &self.service_name
    }
}

impl ServiceFramework for IServiceCreator {
    fn get_service_name(&self) -> &str {
        &self.service_name
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
    fn create_bcat_service_command_returns_an_ipc_interface() {
        let system = Box::new(crate::core::System::new());
        let service = IServiceCreator::new(SystemRef::from_ref(&system), "bcat:u");
        let handler = service.handlers[&commands::CREATE_BCAT_SERVICE]
            .handler_callback
            .expect("CreateBcatService must be wired");
        let mut ctx = HLERequestContext::new();

        handler(&service, &mut ctx);

        assert_eq!(ctx.command_buffer()[6], RESULT_SUCCESS.get_inner_value());
        assert_eq!(ctx.outgoing_move_objects.len(), 1);
    }

    #[test]
    fn delivery_cache_storage_commands_match_edens_dispatch_table() {
        let system = Box::new(crate::core::System::new());
        let service = IServiceCreator::new(SystemRef::from_ref(&system), "bcat:u");

        assert!(
            service.handlers[&commands::CREATE_DELIVERY_CACHE_STORAGE_SERVICE]
                .handler_callback
                .is_some()
        );
        assert!(service.handlers
            [&commands::CREATE_DELIVERY_CACHE_STORAGE_SERVICE_WITH_APPLICATION_ID]
            .handler_callback
            .is_some());
    }
}
