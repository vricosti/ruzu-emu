// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/hle/service/bcat/news/service_creator.h
//! Port of zuyu/src/core/hle/service/bcat/news/service_creator.cpp

use std::collections::BTreeMap;
use std::sync::Arc;

use super::newly_arrived_event_holder::INewlyArrivedEventHolder;
use super::news_data_service::INewsDataService;
use super::news_database_service::INewsDatabaseService;
use super::news_service::INewsService;
use super::overwrite_event_holder::IOverwriteEventHolder;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::ResponseBuilder;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command IDs for News::IServiceCreator
pub mod commands {
    pub const CREATE_NEWS_SERVICE: u32 = 0;
    pub const CREATE_NEWLY_ARRIVED_EVENT_HOLDER: u32 = 1;
    pub const CREATE_NEWS_DATA_SERVICE: u32 = 2;
    pub const CREATE_NEWS_DATABASE_SERVICE: u32 = 3;
    pub const CREATE_OVERWRITE_EVENT_HOLDER: u32 = 4;
}

/// News::IServiceCreator corresponds to upstream `News::IServiceCreator`.
pub struct IServiceCreator {
    pub permissions: u32,
    pub service_name: String,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IServiceCreator {
    pub fn new(permissions: u32, name: &str) -> Self {
        let handlers = build_handler_map(&[
            (
                commands::CREATE_NEWS_SERVICE,
                Some(Self::create_news_service_handler),
                "CreateNewsService",
            ),
            (
                commands::CREATE_NEWLY_ARRIVED_EVENT_HOLDER,
                Some(Self::create_newly_arrived_event_holder_handler),
                "CreateNewlyArrivedEventHolder",
            ),
            (
                commands::CREATE_NEWS_DATA_SERVICE,
                Some(Self::create_news_data_service_handler),
                "CreateNewsDataService",
            ),
            (
                commands::CREATE_NEWS_DATABASE_SERVICE,
                Some(Self::create_news_database_service_handler),
                "CreateNewsDatabaseService",
            ),
            (
                commands::CREATE_OVERWRITE_EVENT_HOLDER,
                Some(Self::create_overwrite_event_holder_handler),
                "CreateOverwriteEventHolder",
            ),
        ]);

        Self {
            permissions,
            service_name: name.to_string(),
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn create_news_service_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let (result, interface) = service.create_news_service();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(result);
        rb.push_ipc_interface(interface);
    }

    fn create_newly_arrived_event_holder_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let (result, interface) = service.create_newly_arrived_event_holder();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(result);
        rb.push_ipc_interface(interface);
    }

    fn create_news_data_service_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let (result, interface) = service.create_news_data_service();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(result);
        rb.push_ipc_interface(interface);
    }

    fn create_news_database_service_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let (result, interface) = service.create_news_database_service();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(result);
        rb.push_ipc_interface(interface);
    }

    fn create_overwrite_event_holder_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let (result, interface) = service.create_overwrite_event_holder();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(result);
        rb.push_ipc_interface(interface);
    }

    pub fn create_news_service(&self) -> (ResultCode, Arc<INewsService>) {
        log::info!("News::IServiceCreator::create_news_service called");
        let service = Arc::new(INewsService::new());
        (RESULT_SUCCESS, service)
    }

    pub fn create_newly_arrived_event_holder(&self) -> (ResultCode, Arc<INewlyArrivedEventHolder>) {
        log::info!("News::IServiceCreator::create_newly_arrived_event_holder called");
        let service = Arc::new(INewlyArrivedEventHolder::new());
        (RESULT_SUCCESS, service)
    }

    pub fn create_news_data_service(&self) -> (ResultCode, Arc<INewsDataService>) {
        log::info!("News::IServiceCreator::create_news_data_service called");
        let service = Arc::new(INewsDataService::new());
        (RESULT_SUCCESS, service)
    }

    pub fn create_news_database_service(&self) -> (ResultCode, Arc<INewsDatabaseService>) {
        log::info!("News::IServiceCreator::create_news_database_service called");
        let service = Arc::new(INewsDatabaseService::new());
        (RESULT_SUCCESS, service)
    }

    pub fn create_overwrite_event_holder(&self) -> (ResultCode, Arc<IOverwriteEventHolder>) {
        log::info!("News::IServiceCreator::create_overwrite_event_holder called");
        let service = Arc::new(IOverwriteEventHolder::new());
        (RESULT_SUCCESS, service)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::service::hle_ipc::SessionRequestManager;
    #[test]
    fn event_holders_return_stable_unsignaled_kernel_objects() {
        use crate::hle::kernel::k_process::{KProcess, ProcessLock};
        use crate::hle::kernel::k_readable_event::KReadableEvent;
        use crate::hle::kernel::k_thread::{KThread, KThreadLock};
        use crate::hle::service::hle_ipc::KAutoObjectRef;
        use std::sync::Mutex;
        let arrived = Arc::new(INewlyArrivedEventHolder::new());
        let overwrite = Arc::new(IOverwriteEventHolder::new());
        let holders: [(
            Arc<dyn SessionRequestHandler>,
            Arc<crate::hle::service::os::event::Event>,
        ); 2] = [
            (arrived.clone(), arrived.get().1),
            (overwrite.clone(), overwrite.get().1),
        ];
        for (holder, event) in holders {
            let process = Arc::new(ProcessLock::from_value(KProcess::new()));
            let readable = Arc::new(Mutex::new(KReadableEvent::new()));
            readable.lock().unwrap().initialize(1, 2);
            event.attach_kernel_event(readable, process.clone());
            let thread = Arc::new(KThreadLock::new(KThread::new()));
            thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
            for _ in 0..2 {
                let mut ctx = HLERequestContext::new_with_thread(thread.clone(), 0);
                ctx.populate_from_incoming_command_buffer(&[
                    4, 8, 0, 0, 0x49434653, 0, 0, 0, 0, 0, 0, 0,
                ]);
                holder.handle_sync_request(&mut ctx);
                assert!(matches!(
                    ctx.outgoing_copy_objects.as_slice(),
                    [KAutoObjectRef::ObjectId(2)]
                ));
                assert!(!event.is_signaled());
            }
        }
    }

    #[test]
    fn every_factory_command_returns_its_own_domain_interface() {
        let creator = Arc::new(IServiceCreator::new(0, "synthetic:news"));
        for (command, expected) in [
            (0, "INewsService"),
            (1, "INewlyArrivedEventHolder"),
            (2, "INewsDataService"),
            (3, "INewsDatabaseService"),
            (4, "IOverwriteEventHolder"),
        ] {
            let manager = Arc::new(std::sync::Mutex::new(SessionRequestManager::new()));
            manager.lock().unwrap().set_session_handler(creator.clone());
            manager.lock().unwrap().convert_to_domain();
            let mut ctx = HLERequestContext::new();
            ctx.populate_from_incoming_command_buffer(&[
                4, 8, 0, 0, 0x49434653, 0, command, 0, 0, 0, 0, 0,
            ]);
            ctx.set_session_request_manager(manager.clone());
            assert_eq!(creator.handle_sync_request(&mut ctx), RESULT_SUCCESS);
            let m = manager.lock().unwrap();
            assert_eq!(m.domain_handler_count(), 2);
            assert_eq!(m.domain_handler(1).unwrap().service_name(), expected);
            assert_eq!(ctx.cmd_buf[ctx.domain_offset as usize - 1], 2);
        }
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
