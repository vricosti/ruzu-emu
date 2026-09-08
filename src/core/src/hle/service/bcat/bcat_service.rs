// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/hle/service/bcat/bcat_service.h
//! Port of zuyu/src/core/hle/service/bcat/bcat_service.cpp
//!
//! IBcatService: main BCAT service interface.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::backend::{BcatBackend, ProgressServiceBackend};
use super::bcat_types::*;
use super::delivery_cache_progress_service::IDeliveryCacheProgressService;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command IDs for IBcatService
pub mod commands {
    pub const REQUEST_SYNC_DELIVERY_CACHE: u32 = 10100;
    pub const REQUEST_SYNC_DELIVERY_CACHE_WITH_DIRECTORY_NAME: u32 = 10101;
    pub const CANCEL_SYNC_DELIVERY_CACHE_REQUEST: u32 = 10200;
    pub const REQUEST_SYNC_DELIVERY_CACHE_WITH_APPLICATION_ID: u32 = 20100;
    pub const REQUEST_SYNC_DELIVERY_CACHE_WITH_APPLICATION_ID_AND_DIRECTORY_NAME: u32 = 20101;
    pub const GET_DELIVERY_CACHE_STORAGE_UPDATE_NOTIFIER: u32 = 20300;
    pub const REQUEST_SUSPEND_DELIVERY_TASK: u32 = 20301;
    pub const REGISTER_SYSTEM_APPLICATION_DELIVERY_TASK: u32 = 20400;
    pub const UNREGISTER_SYSTEM_APPLICATION_DELIVERY_TASK: u32 = 20401;
    pub const SET_SYSTEM_APPLICATION_DELIVERY_TASK_TIMER: u32 = 20410;
    pub const SET_PASSPHRASE: u32 = 30100;
    pub const UNKNOWN_30101: u32 = 30101;
    pub const UNKNOWN_30102: u32 = 30102;
    pub const REGISTER_BACKGROUND_DELIVERY_TASK: u32 = 30200;
    pub const UNREGISTER_BACKGROUND_DELIVERY_TASK: u32 = 30201;
    pub const BLOCK_DELIVERY_TASK: u32 = 30202;
    pub const UNBLOCK_DELIVERY_TASK: u32 = 30203;
    pub const SET_DELIVERY_TASK_TIMER: u32 = 30210;
    pub const REGISTER_SYSTEM_APPLICATION_DELIVERY_TASKS: u32 = 30300;
    pub const ENUMERATE_BACKGROUND_DELIVERY_TASK: u32 = 90100;
    pub const UNKNOWN_90101: u32 = 90101;
    pub const GET_DELIVERY_LIST: u32 = 90200;
    pub const CLEAR_DELIVERY_CACHE_STORAGE: u32 = 90201;
    pub const CLEAR_DELIVERY_TASK_SUBSCRIPTION_STATUS: u32 = 90202;
    pub const GET_PUSH_NOTIFICATION_LOG: u32 = 90300;
    pub const UNKNOWN_90301: u32 = 90301;
}

/// IBcatService corresponds to `IBcatService` in upstream `bcat_service.h`.
pub struct IBcatService {
    backend: Arc<Mutex<dyn BcatBackend + Send>>,
    pub progress: [ProgressServiceBackend; 2], // Normal, Directory
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IBcatService {
    pub fn new(backend: Arc<Mutex<dyn BcatBackend + Send>>) -> Self {
        let handlers = build_handler_map(&[
            (
                commands::REQUEST_SYNC_DELIVERY_CACHE,
                None,
                "RequestSyncDeliveryCache",
            ),
            (
                commands::REQUEST_SYNC_DELIVERY_CACHE_WITH_DIRECTORY_NAME,
                None,
                "RequestSyncDeliveryCacheWithDirectoryName",
            ),
            (
                commands::CANCEL_SYNC_DELIVERY_CACHE_REQUEST,
                None,
                "CancelSyncDeliveryCacheRequest",
            ),
            (
                commands::REQUEST_SYNC_DELIVERY_CACHE_WITH_APPLICATION_ID,
                None,
                "RequestSyncDeliveryCacheWithApplicationId",
            ),
            (
                commands::REQUEST_SYNC_DELIVERY_CACHE_WITH_APPLICATION_ID_AND_DIRECTORY_NAME,
                None,
                "RequestSyncDeliveryCacheWithApplicationIdAndDirectoryName",
            ),
            (
                commands::GET_DELIVERY_CACHE_STORAGE_UPDATE_NOTIFIER,
                None,
                "GetDeliveryCacheStorageUpdateNotifier",
            ),
            (
                commands::REQUEST_SUSPEND_DELIVERY_TASK,
                None,
                "RequestSuspendDeliveryTask",
            ),
            (
                commands::REGISTER_SYSTEM_APPLICATION_DELIVERY_TASK,
                None,
                "RegisterSystemApplicationDeliveryTask",
            ),
            (
                commands::UNREGISTER_SYSTEM_APPLICATION_DELIVERY_TASK,
                None,
                "UnregisterSystemApplicationDeliveryTask",
            ),
            (
                commands::SET_SYSTEM_APPLICATION_DELIVERY_TASK_TIMER,
                None,
                "SetSystemApplicationDeliveryTaskTimer",
            ),
            (commands::SET_PASSPHRASE, None, "SetPassphrase"),
            (commands::UNKNOWN_30101, None, "Unknown30101"),
            (commands::UNKNOWN_30102, None, "Unknown30102"),
            (
                commands::REGISTER_BACKGROUND_DELIVERY_TASK,
                None,
                "RegisterBackgroundDeliveryTask",
            ),
            (
                commands::UNREGISTER_BACKGROUND_DELIVERY_TASK,
                None,
                "UnregisterBackgroundDeliveryTask",
            ),
            (commands::BLOCK_DELIVERY_TASK, None, "BlockDeliveryTask"),
            (commands::UNBLOCK_DELIVERY_TASK, None, "UnblockDeliveryTask"),
            (
                commands::SET_DELIVERY_TASK_TIMER,
                None,
                "SetDeliveryTaskTimer",
            ),
            (
                commands::REGISTER_SYSTEM_APPLICATION_DELIVERY_TASKS,
                Some(Self::register_system_application_delivery_tasks_handler),
                "RegisterSystemApplicationDeliveryTasks",
            ),
            (
                commands::ENUMERATE_BACKGROUND_DELIVERY_TASK,
                None,
                "EnumerateBackgroundDeliveryTask",
            ),
            (commands::UNKNOWN_90101, None, "Unknown90101"),
            (commands::GET_DELIVERY_LIST, None, "GetDeliveryList"),
            (
                commands::CLEAR_DELIVERY_CACHE_STORAGE,
                None,
                "ClearDeliveryCacheStorage",
            ),
            (
                commands::CLEAR_DELIVERY_TASK_SUBSCRIPTION_STATUS,
                None,
                "ClearDeliveryTaskSubscriptionStatus",
            ),
            (
                commands::GET_PUSH_NOTIFICATION_LOG,
                None,
                "GetPushNotificationLog",
            ),
            (commands::UNKNOWN_90301, None, "Unknown90301"),
        ]);

        Self {
            backend,
            progress: [
                ProgressServiceBackend::new("Normal"),
                ProgressServiceBackend::new("Directory"),
            ],
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    pub fn request_sync_delivery_cache(
        &mut self,
    ) -> (ResultCode, Arc<IDeliveryCacheProgressService>) {
        log::debug!("IBcatService::request_sync_delivery_cache called");

        let progress_backend = &mut self.progress[SyncType::Normal as usize];
        // Upstream calls backend.Synchronize with title info and progress backend.
        // We call the backend synchronize here.
        {
            let mut backend = self.backend.lock().unwrap();
            backend.synchronize(
                TitleIdVersion {
                    title_id: 0, // Would come from system.GetApplicationProcessProgramID()
                    build_id: 0, // Would come from system.GetApplicationProcessBuildID()
                },
                progress_backend,
            );
        }

        let event = progress_backend.get_event();
        let impl_data = progress_backend.get_impl().clone();
        let service = Arc::new(IDeliveryCacheProgressService::new(event, impl_data));
        (RESULT_SUCCESS, service)
    }

    pub fn request_sync_delivery_cache_with_directory_name(
        &mut self,
        name_raw: &DirectoryName,
    ) -> (ResultCode, Arc<IDeliveryCacheProgressService>) {
        let name =
            common::string_util::string_from_fixed_zero_terminated_buffer(name_raw, name_raw.len());

        log::debug!(
            "IBcatService::request_sync_delivery_cache_with_directory_name called, name={}",
            name
        );

        let progress_backend = &mut self.progress[SyncType::Directory as usize];
        {
            let mut backend = self.backend.lock().unwrap();
            backend.synchronize_directory(
                TitleIdVersion {
                    title_id: 0,
                    build_id: 0,
                },
                name,
                progress_backend,
            );
        }

        let event = progress_backend.get_event();
        let impl_data = progress_backend.get_impl().clone();
        let service = Arc::new(IDeliveryCacheProgressService::new(event, impl_data));
        (RESULT_SUCCESS, service)
    }

    pub fn set_passphrase(&self, application_id: u64, passphrase_buffer: &[u8]) -> ResultCode {
        log::debug!(
            "IBcatService::set_passphrase called, application_id={:016X}",
            application_id
        );
        if application_id == 0 {
            return super::bcat_result::RESULT_INVALID_ARGUMENT;
        }
        if passphrase_buffer.len() > 0x40 {
            return super::bcat_result::RESULT_INVALID_ARGUMENT;
        }

        let mut passphrase = Passphrase::default();
        let copy_len = std::cmp::min(passphrase.len(), passphrase_buffer.len());
        passphrase[..copy_len].copy_from_slice(&passphrase_buffer[..copy_len]);

        self.backend
            .lock()
            .unwrap()
            .set_passphrase(application_id, &passphrase);
        RESULT_SUCCESS
    }

    fn register_system_application_delivery_tasks_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let result = service.register_system_application_delivery_tasks();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    pub fn register_system_application_delivery_tasks(&self) -> ResultCode {
        log::warn!("(STUBBED) IBcatService::register_system_application_delivery_tasks called");
        RESULT_SUCCESS
    }

    pub fn clear_delivery_cache_storage(&self, application_id: u64) -> ResultCode {
        log::debug!(
            "IBcatService::clear_delivery_cache_storage called, title_id={:016X}",
            application_id
        );
        if application_id == 0 {
            return super::bcat_result::RESULT_INVALID_ARGUMENT;
        }
        if !self.backend.lock().unwrap().clear(application_id) {
            // Upstream: FileSys::ResultPermissionDenied — module FS(2), description 6400
            return ResultCode::from_module_description(crate::hle::result::ErrorModule::FS, 6400);
        }
        RESULT_SUCCESS
    }

    pub fn get_progress_backend(&self, sync_type: SyncType) -> &ProgressServiceBackend {
        &self.progress[sync_type as usize]
    }

    pub fn get_progress_backend_mut(&mut self, sync_type: SyncType) -> &mut ProgressServiceBackend {
        &mut self.progress[sync_type as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_command_returns_success_without_output_objects() {
        let service = IBcatService::new(Arc::new(Mutex::new(
            super::super::backend::NullBcatBackend::new(),
        )));
        let mut ctx = HLERequestContext::new();
        let command = commands::REGISTER_SYSTEM_APPLICATION_DELIVERY_TASKS;
        service.handlers[&command].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(ctx.command_buffer()[6], RESULT_SUCCESS.get_inner_value());
        assert!(ctx.outgoing_copy_objects.is_empty());
        assert!(service.handlers[&commands::REGISTER_BACKGROUND_DELIVERY_TASK]
            .handler_callback.is_none());
    }
}

impl SessionRequestHandler for IBcatService {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "IBcatService"
    }
}

impl ServiceFramework for IBcatService {
    fn get_service_name(&self) -> &str {
        "IBcatService"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
