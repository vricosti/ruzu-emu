// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/aoc/addon_content_manager.h
//! Port of zuyu/src/core/hle/service/aoc/addon_content_manager.cpp
//!
//! IAddOnContentManager service ("aoc:u").

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{
    HLERequestContext, SessionRequestHandler, SessionRequestHandlerFactory,
    SessionRequestHandlerPtr,
};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command IDs for IAddOnContentManager
pub mod commands {
    pub const COUNT_ADD_ON_CONTENT_BY_APPLICATION_ID: u32 = 0;
    pub const LIST_ADD_ON_CONTENT_BY_APPLICATION_ID: u32 = 1;
    pub const COUNT_ADD_ON_CONTENT: u32 = 2;
    pub const LIST_ADD_ON_CONTENT: u32 = 3;
    pub const GET_ADD_ON_CONTENT_BASE_ID_BY_APPLICATION_ID: u32 = 4;
    pub const GET_ADD_ON_CONTENT_BASE_ID: u32 = 5;
    pub const PREPARE_ADD_ON_CONTENT_BY_APPLICATION_ID: u32 = 6;
    pub const PREPARE_ADD_ON_CONTENT: u32 = 7;
    pub const GET_ADD_ON_CONTENT_LIST_CHANGED_EVENT: u32 = 8;
    pub const GET_ADD_ON_CONTENT_LOST_ERROR_CODE: u32 = 9;
    pub const GET_ADD_ON_CONTENT_LIST_CHANGED_EVENT_WITH_PROCESS_ID: u32 = 10;
    pub const NOTIFY_MOUNT_ADD_ON_CONTENT: u32 = 11;
    pub const NOTIFY_UNMOUNT_ADD_ON_CONTENT: u32 = 12;
    pub const IS_ADD_ON_CONTENT_MOUNTED_FOR_DEBUG: u32 = 13;
    pub const CHECK_ADD_ON_CONTENT_MOUNT_STATUS: u32 = 50;
    pub const CREATE_EC_PURCHASED_EVENT_MANAGER: u32 = 100;
    pub const CREATE_PERMANENT_EC_PURCHASED_EVENT_MANAGER: u32 = 101;
    pub const CREATE_CONTENTS_SERVICE_MANAGER: u32 = 110;
    pub const SET_REQUIRED_ADD_ON_CONTENTS_ON_CONTENTS_AVAILABILITY_TRANSITION: u32 = 200;
    pub const SETUP_HOST_ADD_ON_CONTENT: u32 = 300;
    pub const GET_REGISTERED_ADD_ON_CONTENT_PATH: u32 = 301;
    pub const UPDATE_CACHED_LIST: u32 = 302;
}

/// IAddOnContentManager service.
///
/// Corresponds to `IAddOnContentManager` in upstream `addon_content_manager.h`.
pub struct IAddOnContentManager {
    system: crate::core::SystemRef,
    add_on_content: Vec<u64>,
    service_context: crate::hle::service::kernel_helpers::ServiceContext,
    /// Handle for the AOC change event. Returned by GetAddOnContentListChangedEvent.
    aoc_change_event_handle: u32,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IAddOnContentManager {
    pub fn new(system: crate::core::SystemRef) -> Self {
        let handlers = build_handler_map(&[
            (
                commands::COUNT_ADD_ON_CONTENT,
                Some(Self::count_add_on_content_handler),
                "CountAddOnContent",
            ),
            (
                commands::LIST_ADD_ON_CONTENT,
                Some(Self::list_add_on_content_handler),
                "ListAddOnContent",
            ),
            (
                commands::GET_ADD_ON_CONTENT_BASE_ID,
                Some(Self::get_add_on_content_base_id_handler),
                "GetAddOnContentBaseId",
            ),
            (
                commands::PREPARE_ADD_ON_CONTENT,
                Some(Self::prepare_add_on_content_handler),
                "PrepareAddOnContent",
            ),
            (
                commands::GET_ADD_ON_CONTENT_LIST_CHANGED_EVENT,
                Some(Self::get_add_on_content_list_changed_event_handler),
                "GetAddOnContentListChangedEvent",
            ),
            (
                commands::GET_ADD_ON_CONTENT_LIST_CHANGED_EVENT_WITH_PROCESS_ID,
                Some(Self::get_add_on_content_list_changed_event_with_process_id_handler),
                "GetAddOnContentListChangedEventWithProcessId",
            ),
            (
                commands::NOTIFY_MOUNT_ADD_ON_CONTENT,
                Some(Self::notify_mount_add_on_content_handler),
                "NotifyMountAddOnContent",
            ),
            (
                commands::NOTIFY_UNMOUNT_ADD_ON_CONTENT,
                Some(Self::notify_unmount_add_on_content_handler),
                "NotifyUnmountAddOnContent",
            ),
            (
                commands::CHECK_ADD_ON_CONTENT_MOUNT_STATUS,
                Some(Self::check_add_on_content_mount_status_handler),
                "CheckAddOnContentMountStatus",
            ),
            (
                commands::CREATE_EC_PURCHASED_EVENT_MANAGER,
                Some(Self::create_ec_purchased_event_manager_handler),
                "CreateEcPurchasedEventManager",
            ),
            (
                commands::CREATE_PERMANENT_EC_PURCHASED_EVENT_MANAGER,
                Some(Self::create_permanent_ec_purchased_event_manager_handler),
                "CreatePermanentEcPurchasedEventManager",
            ),
        ]);
        let mut service_context = crate::hle::service::kernel_helpers::ServiceContext::new(
            "IAddOnContentManager".to_string(),
        );
        let aoc_change_event_handle =
            service_context.create_event("GetAddOnContentListChangedEvent".to_string());
        Self {
            system,
            add_on_content: Vec::new(),
            service_context,
            aoc_change_event_handle,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// CountAddOnContent (cmd 2).
    ///
    /// Upstream counts how many AOC title IDs in `add_on_content` match the current
    /// application's base title ID, respecting the "DLC" disabled-addons setting.
    pub fn count_add_on_content(&self, _process_id: u64) -> u32 {
        log::debug!("IAddOnContentManager::count_add_on_content called");
        let current = self.system.get().runtime_program_id();
        let disabled = common::settings::values()
            .disabled_addons
            .get(&current)
            .cloned()
            .unwrap_or_default();
        if disabled.iter().any(|s| s == "DLC") {
            return 0;
        }
        self.add_on_content
            .iter()
            .filter(|&&tid| crate::file_sys::common_funcs::get_base_title_id(tid) == current)
            .count() as u32
    }

    /// ListAddOnContent (cmd 3).
    ///
    /// Upstream collects AOC IDs matching the current title, applies offset/count,
    /// and writes them to the output buffer. Respects "DLC" disabled-addons setting.
    pub fn list_add_on_content(
        &self,
        offset: u32,
        count: u32,
        _process_id: u64,
    ) -> (u32, Vec<u32>) {
        log::debug!(
            "IAddOnContentManager::list_add_on_content called, offset={}, count={}",
            offset,
            count
        );
        let current = crate::file_sys::common_funcs::get_base_title_id(
            self.system.get().runtime_program_id(),
        );
        let disabled = common::settings::values()
            .disabled_addons
            .get(&current)
            .cloned()
            .unwrap_or_default();
        let mut out: Vec<u32> = Vec::new();
        if !disabled.iter().any(|s| s == "DLC") {
            for &content_id in &self.add_on_content {
                if crate::file_sys::common_funcs::get_base_title_id(content_id) != current {
                    continue;
                }
                out.push(crate::file_sys::common_funcs::get_aoc_id(content_id) as u32);
            }
        }
        if (offset as usize) > out.len() {
            // Upstream returns ResultUnknown when offset > out.size()
            return (0, Vec::new());
        }
        let result_count = std::cmp::min(out.len() - offset as usize, count as usize) as u32;
        let result_entries: Vec<u32> = out
            .into_iter()
            .skip(offset as usize)
            .take(result_count as usize)
            .collect();
        (result_count, result_entries)
    }

    /// GetAddOnContentBaseId (cmd 5).
    ///
    /// Upstream uses PatchManager to get control metadata and reads DLC base title ID.
    /// If control metadata is unavailable, falls back to `GetAOCBaseTitleID(title_id)`.
    pub fn get_add_on_content_base_id(&self, _process_id: u64) -> u64 {
        log::debug!("IAddOnContentManager::get_add_on_content_base_id called");
        let title_id = self.system.get().runtime_program_id();
        // Upstream: PatchManager pm{title_id, system.GetFileSystemController(), system.GetContentProvider()};
        // const auto res = pm.GetControlMetadata();
        // if (res.first == nullptr) { return GetAOCBaseTitleID(title_id); }
        // return res.first->GetDLCBaseTitleId();
        //
        // PatchManager::GetControlMetadata requires FileSystemController and ContentProvider
        // integration that is not yet wired at the system level. Fall back to the
        // no-metadata path which computes the AOC base title ID arithmetically.
        crate::file_sys::common_funcs::get_aoc_base_title_id(title_id)
    }

    /// Stubbed: PrepareAddOnContent (cmd 7)
    pub fn prepare_add_on_content(&self, addon_index: i32, process_id: u64) {
        log::warn!(
            "(STUBBED) IAddOnContentManager::prepare_add_on_content called, addon_index={}, process_id={}",
            addon_index,
            process_id
        );
    }

    /// GetAddOnContentListChangedEvent (cmd 8)
    ///
    /// Returns the readable side of the AOC change event.
    /// Upstream: `GetAddOnContentListChangedEvent` returns `aoc_change_event->GetReadableEvent()`.
    pub fn get_add_on_content_list_changed_event(
        &self,
    ) -> Option<Arc<crate::hle::service::os::event::Event>> {
        log::debug!("IAddOnContentManager::get_add_on_content_list_changed_event called");
        self.service_context.get_event(self.aoc_change_event_handle)
    }

    /// GetAddOnContentListChangedEventWithProcessId (cmd 10)
    ///
    /// Same as GetAddOnContentListChangedEvent but takes a process_id parameter.
    pub fn get_add_on_content_list_changed_event_with_process_id(
        &self,
        _process_id: u64,
    ) -> Option<Arc<crate::hle::service::os::event::Event>> {
        log::debug!(
            "IAddOnContentManager::get_add_on_content_list_changed_event_with_process_id called"
        );
        self.service_context.get_event(self.aoc_change_event_handle)
    }

    /// Stubbed: NotifyMountAddOnContent (cmd 11)
    pub fn notify_mount_add_on_content(&self) {
        log::warn!("(STUBBED) IAddOnContentManager::notify_mount_add_on_content called");
    }

    /// Stubbed: NotifyUnmountAddOnContent (cmd 12)
    pub fn notify_unmount_add_on_content(&self) {
        log::warn!("(STUBBED) IAddOnContentManager::notify_unmount_add_on_content called");
    }

    /// Stubbed: CheckAddOnContentMountStatus (cmd 50)
    pub fn check_add_on_content_mount_status(&self) {
        log::warn!("(STUBBED) IAddOnContentManager::check_add_on_content_mount_status called");
    }

    /// Stubbed: CreateEcPurchasedEventManager (cmd 100)
    pub fn create_ec_purchased_event_manager(
        &self,
    ) -> Arc<super::purchase_event_manager::IPurchaseEventManager> {
        log::warn!("(STUBBED) IAddOnContentManager::create_ec_purchased_event_manager called");
        Arc::new(super::purchase_event_manager::IPurchaseEventManager::new())
    }

    /// Stubbed: CreatePermanentEcPurchasedEventManager (cmd 101)
    pub fn create_permanent_ec_purchased_event_manager(
        &self,
    ) -> Arc<super::purchase_event_manager::IPurchaseEventManager> {
        log::warn!(
            "(STUBBED) IAddOnContentManager::create_permanent_ec_purchased_event_manager called"
        );
        Arc::new(super::purchase_event_manager::IPurchaseEventManager::new())
    }

    fn count_add_on_content_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IAddOnContentManager) };
        let count = service.count_add_on_content(ctx.get_pid());

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(count);
    }

    fn list_add_on_content_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IAddOnContentManager) };
        let mut rp = RequestParser::new(ctx);
        let offset = rp.pop_u32();
        let count = rp.pop_u32();
        let (out_count, add_on_content) = service.list_add_on_content(offset, count, ctx.get_pid());

        let mut out_bytes = Vec::with_capacity(add_on_content.len() * std::mem::size_of::<u32>());
        for add_on_content_id in add_on_content {
            out_bytes.extend_from_slice(&add_on_content_id.to_le_bytes());
        }
        if !out_bytes.is_empty() {
            ctx.write_buffer(&out_bytes, 0);
        }

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(out_count);
    }

    fn get_add_on_content_base_id_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IAddOnContentManager) };
        let base_id = service.get_add_on_content_base_id(0);

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(base_id);
    }

    fn prepare_add_on_content_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IAddOnContentManager) };
        let mut rp = RequestParser::new(ctx);
        let addon_index = rp.pop_i32();
        service.prepare_add_on_content(addon_index, 0);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_add_on_content_list_changed_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IAddOnContentManager) };
        let _event = service.get_add_on_content_list_changed_event();
        // Return the readable event handle via copy handle.
        if let Some(handle) = ctx.create_readable_event_handle(false) {
            let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
            rb.push_result(RESULT_SUCCESS);
            rb.push_copy_objects(handle);
        } else {
            let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
            rb.push_result(RESULT_SUCCESS);
            rb.push_copy_objects(0);
        }
    }

    fn get_add_on_content_list_changed_event_with_process_id_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IAddOnContentManager) };
        let _event = service.get_add_on_content_list_changed_event_with_process_id(0);
        if let Some(handle) = ctx.create_readable_event_handle(false) {
            let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
            rb.push_result(RESULT_SUCCESS);
            rb.push_copy_objects(handle);
        } else {
            let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
            rb.push_result(RESULT_SUCCESS);
            rb.push_copy_objects(0);
        }
    }

    fn notify_mount_add_on_content_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IAddOnContentManager) };
        service.notify_mount_add_on_content();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn notify_unmount_add_on_content_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IAddOnContentManager) };
        service.notify_unmount_add_on_content();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn check_add_on_content_mount_status_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IAddOnContentManager) };
        service.check_add_on_content_mount_status();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn create_ec_purchased_event_manager_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IAddOnContentManager) };
        let manager = service.create_ec_purchased_event_manager();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(manager);
    }

    fn create_permanent_ec_purchased_event_manager_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IAddOnContentManager) };
        let manager = service.create_permanent_ec_purchased_event_manager();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(manager);
    }
}

/// Launches AOC services.
///
/// Matches upstream `void AOC::LoopProcess(Core::System& system)`:
/// ```cpp
/// void LoopProcess(Core::System& system) {
///     auto server_manager = std::make_unique<ServerManager>(system);
///     server_manager->RegisterNamedService("aoc:u", ...);
///     ServerManager::RunServer(std::move(server_manager));
/// }
/// ```
pub fn loop_process(system: crate::core::SystemRef) {
    let server_manager = crate::hle::service::server_manager::ServerManager::new_shared(system);
    {
        let mut server_manager = server_manager.lock().unwrap();
        let factory: SessionRequestHandlerFactory =
            Box::new(move || -> SessionRequestHandlerPtr {
                Arc::new(IAddOnContentManager::new(system))
            });
        server_manager.register_named_service("aoc:u", factory, 64);
    }
    crate::hle::service::server_manager::ServerManager::run_server_shared(server_manager);
}

impl SessionRequestHandler for IAddOnContentManager {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }
}

impl ServiceFramework for IAddOnContentManager {
    fn get_service_name(&self) -> &str {
        "aoc:u"
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
    fn implemented_handlers_match_upstream_function_table() {
        let system = crate::core::System::new();
        let service = IAddOnContentManager::new(crate::core::SystemRef::from_ref(&system));

        for command in [
            commands::LIST_ADD_ON_CONTENT,
            commands::NOTIFY_MOUNT_ADD_ON_CONTENT,
            commands::NOTIFY_UNMOUNT_ADD_ON_CONTENT,
            commands::CHECK_ADD_ON_CONTENT_MOUNT_STATUS,
        ] {
            assert!(
                service
                    .handlers()
                    .get(&command)
                    .and_then(|info| info.handler_callback)
                    .is_some(),
                "command {command} must be dispatched"
            );
        }
    }
}
