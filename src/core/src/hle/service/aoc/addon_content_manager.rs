// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/aoc/addon_content_manager.h
//! Port of zuyu/src/core/hle/service/aoc/addon_content_manager.cpp
//!
//! IAddOnContentManager service ("aoc:u").

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::file_sys::nca_metadata::{ContentRecordType, TitleType};
use crate::file_sys::registered_cache::ContentProvider;
use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};

/// Upstream `AccumulateAOCTitleIDs`: snapshot readable Data content at creation.
fn accumulate_aoc_title_ids(system: &crate::core::System) -> Vec<u64> {
    let Some(provider) = system.get_content_provider() else {
        return Vec::new();
    };
    let provider = provider.lock().unwrap();
    provider
        .list_entries_filter(Some(TitleType::AOC), Some(ContentRecordType::Data), None)
        .into_iter()
        .filter_map(|entry| {
            let nca = provider.get_entry(entry.title_id, ContentRecordType::Data)?;
            (nca.get_status() == crate::file_sys::partition_filesystem::ResultStatus::Success)
                .then_some(entry.title_id)
        })
        .collect()
}
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
        let add_on_content = accumulate_aoc_title_ids(system.get());
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
            add_on_content,
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
    ) -> Result<(u32, Vec<u32>), ResultCode> {
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
            return Err(RESULT_UNKNOWN);
        }
        let result_count = std::cmp::min(out.len() - offset as usize, count as usize) as u32;
        let result_entries: Vec<u32> = out
            .into_iter()
            .skip(offset as usize)
            .take(result_count as usize)
            .collect();
        Ok((result_count, result_entries))
    }

    /// GetAddOnContentBaseId (cmd 5).
    ///
    /// Upstream uses PatchManager to get control metadata and reads DLC base title ID.
    /// If control metadata is unavailable, falls back to `GetAOCBaseTitleID(title_id)`.
    pub fn get_add_on_content_base_id(&self, _process_id: u64) -> u64 {
        log::debug!("IAddOnContentManager::get_add_on_content_base_id called");
        let title_id = self.system.get().runtime_program_id();
        let system = self.system.get();
        let controller = system.get_filesystem_controller();
        let controller = controller.lock().unwrap();
        if let Some(provider) = system.get_content_provider() {
            let provider = provider.lock().unwrap();
            let patch_manager = crate::file_sys::patch_manager::PatchManager::new(
                title_id,
                &controller,
                &*provider,
            );
            if let Some(nacp) = patch_manager.get_control_metadata().0 {
                return nacp.get_dlc_base_title_id();
            }
        }
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
        let (out_count, add_on_content) =
            match service.list_add_on_content(offset, count, ctx.get_pid()) {
                Ok(result) => result,
                Err(result) => {
                    let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                    rb.push_result(result);
                    return;
                }
            };

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
    fn installed_content_is_visible_to_aoc_with_filtering_and_pagination() {
        const CHILD: &str = "RUZU_AOC_REGRESSION_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root =
                std::env::temp_dir().join(format!("ruzu-aoc-{}-{nonce}", std::process::id()));
            std::fs::create_dir(&root).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "hle::service::aoc::addon_content_manager::tests::installed_content_is_visible_to_aoc_with_filtering_and_pagination", "--nocapture"])
                .env(CHILD, &root)
                .env("XDG_DATA_HOME", root.join("data"))
                .env("XDG_CONFIG_HOME", root.join("config"))
                .env("XDG_CACHE_HOME", root.join("cache"))
                .status().unwrap();
            assert!(status.success());
            return;
        }
        // XDG isolation alone does not cover Windows.
        let root = std::path::PathBuf::from(std::env::var_os(CHILD).unwrap());
        common::fs::path_util::set_app_directory(root.to_str().unwrap());
        use crate::crypto::key_manager::{KeyManager, S128KeyType};
        use crate::file_sys::fssystem::nca_header::NcaHeader;
        use crate::file_sys::registered_cache::{
            ContentProviderUnion, ContentProviderUnionSlot, ManualContentProvider,
        };
        use crate::file_sys::vfs::vfs_types::VirtualFile;
        use crate::file_sys::vfs::vfs_vector::VectorVfsFile;
        use std::sync::Mutex;

        // Synthetic plaintext header without sections: exercises NCA validation
        // without any user keys or copyrighted content. Key lives only in child.
        KeyManager::instance()
            .lock()
            .unwrap()
            .set_key_128(S128KeyType::KeyArea, [0x55; 16], 0, 0);
        let mut header: NcaHeader = unsafe { std::mem::zeroed() };
        header.magic = NcaHeader::MAGIC3;
        header.sdk_addon_version = 0x000B_0000;
        header.content_size = NcaHeader::SIZE as u64;
        let data = unsafe {
            std::slice::from_raw_parts(&header as *const NcaHeader as *const u8, NcaHeader::SIZE)
        }
        .to_vec();
        let valid: VirtualFile =
            Arc::new(VectorVfsFile::new(data, "synthetic.nca".to_owned(), None));
        let invalid: VirtualFile = Arc::new(VectorVfsFile::new(
            Vec::new(),
            "invalid.nca".to_owned(),
            None,
        ));
        let mut provider = Box::new(ManualContentProvider::new());
        provider.add_entry(
            TitleType::AOC,
            ContentRecordType::Data,
            0x3001,
            valid.clone(),
        );
        provider.add_entry(
            TitleType::AOC,
            ContentRecordType::Data,
            0x3003,
            valid.clone(),
        );
        provider.add_entry(TitleType::AOC, ContentRecordType::Data, 0x3002, invalid);
        provider.add_entry(
            TitleType::AOC,
            ContentRecordType::Data,
            0x5001,
            valid.clone(),
        );
        provider.add_entry(TitleType::AOC, ContentRecordType::Program, 0x3004, valid);
        let mut union = ContentProviderUnion::new();
        unsafe {
            union.set_slot(
                ContentProviderUnionSlot::UserNAND,
                &mut *provider as *mut dyn ContentProvider,
            );
        }
        let mut system = crate::core::System::new_for_test();
        system.set_runtime_program_id(0x2000);
        system.set_content_provider(Arc::new(Mutex::new(union)));
        let service = IAddOnContentManager::new(crate::core::SystemRef::from_ref(&system));
        assert_eq!(service.add_on_content, vec![0x3001, 0x3003, 0x5001]);
        assert_eq!(service.count_add_on_content(0), 2);
        assert_eq!(service.list_add_on_content(0, 10, 0), Ok((2, vec![1, 3])));
        assert_eq!(service.list_add_on_content(1, 1, 0), Ok((1, vec![3])));
        assert_eq!(service.list_add_on_content(2, 10, 0), Ok((0, vec![])));
        assert_eq!(service.list_add_on_content(3, 10, 0), Err(RESULT_UNKNOWN));
        assert_eq!(service.get_add_on_content_base_id(0), 0x3000);
        common::settings::values_mut()
            .disabled_addons
            .insert(0x2000, vec!["DLC".to_owned()]);
        assert_eq!(service.count_add_on_content(0), 0);
        assert_eq!(service.list_add_on_content(0, 10, 0), Ok((0, vec![])));
        assert_eq!(service.list_add_on_content(1, 10, 0), Err(RESULT_UNKNOWN));
    }

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
