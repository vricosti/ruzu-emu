// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/service/library_applet_self_accessor.h
//! Port of zuyu/src/core/hle/service/am/service/library_applet_self_accessor.cpp

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::core::SystemRef;
use crate::file_sys::patch_manager::PatchManager;
use crate::file_sys::registered_cache::get_update_title_id;
use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};
use crate::hle::service::am::am_types::{AppletId, AppletIdentityInfo, LibraryAppletMode};
use crate::hle::service::am::applet_data_broker::AppletDataBroker;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::ns::read_only_application_control_data_interface::IReadOnlyApplicationControlDataInterface;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

use super::storage::IStorage;

/// Library applet info.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct LibraryAppletInfo {
    pub applet_id: AppletId,
    pub library_applet_mode: LibraryAppletMode,
}
const _: () = assert!(core::mem::size_of::<LibraryAppletInfo>() == 0x8);

/// Error code.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ErrorCode {
    pub category: u32,
    pub number: u32,
}
const _: () = assert!(core::mem::size_of::<ErrorCode>() == 0x8);

/// Error context.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ErrorContext {
    pub error_type: u8,
    pub _padding: [u8; 0x7],
    pub data: [u8; 0x1f4],
    pub result: u32,
}
const _: () = assert!(core::mem::size_of::<ErrorContext>() == 0x200);

impl Default for ErrorContext {
    fn default() -> Self {
        Self {
            error_type: 0,
            _padding: [0u8; 0x7],
            data: [0u8; 0x1f4],
            result: 0,
        }
    }
}

/// ILibraryAppletSelfAccessor service.
pub struct ILibraryAppletSelfAccessor {
    system: SystemRef,
    applet: Arc<Mutex<crate::hle::service::am::applet::Applet>>,
    /// Matches upstream `const std::shared_ptr<AppletDataBroker> m_broker`.
    /// Obtained from `m_applet->caller_applet_broker`.
    broker: Arc<AppletDataBroker>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ILibraryAppletSelfAccessor {
    pub fn new(
        system: SystemRef,
        applet: Arc<Mutex<crate::hle::service::am::applet::Applet>>,
    ) -> Self {
        // Upstream: m_broker{m_applet->caller_applet_broker}
        let broker = applet
            .lock()
            .unwrap()
            .caller_applet_broker
            .clone()
            .expect("library applet caller broker must be initialized");
        let handlers = build_handler_map(&[
            (0, Some(Self::pop_in_data_handler), "PopInData"),
            (1, Some(Self::push_out_data_handler), "PushOutData"),
            (
                2,
                Some(Self::pop_interactive_in_data_handler),
                "PopInteractiveInData",
            ),
            (
                3,
                Some(Self::push_interactive_out_data_handler),
                "PushInteractiveOutData",
            ),
            (
                5,
                Some(Self::get_pop_in_data_event_handler),
                "GetPopInDataEvent",
            ),
            (
                6,
                Some(Self::get_pop_interactive_in_data_event_handler),
                "GetPopInteractiveInDataEvent",
            ),
            (
                10,
                Some(Self::exit_process_and_return_handler),
                "ExitProcessAndReturn",
            ),
            (
                11,
                Some(Self::get_library_applet_info_handler),
                "GetLibraryAppletInfo",
            ),
            (
                12,
                Some(Self::get_main_applet_identity_info_handler),
                "GetMainAppletIdentityInfo",
            ),
            (
                13,
                Some(Self::can_use_application_core_handler),
                "CanUseApplicationCore",
            ),
            (
                14,
                Some(Self::get_caller_applet_identity_info_handler),
                "GetCallerAppletIdentityInfo",
            ),
            (
                15,
                Some(Self::get_main_applet_application_control_property_handler),
                "GetMainAppletApplicationControlProperty",
            ),
            (
                16,
                Some(Self::get_main_applet_storage_id_handler),
                "GetMainAppletStorageId",
            ),
            (
                17,
                Some(Self::get_caller_applet_identity_info_stack_handler),
                "GetCallerAppletIdentityInfoStack",
            ),
            (18, None, "GetNextReturnDestinationAppletIdentityInfo"),
            (
                19,
                Some(Self::get_desirable_keyboard_layout_handler),
                "GetDesirableKeyboardLayout",
            ),
            (20, None, "PopExtraStorage"),
            (25, None, "GetPopExtraStorageEvent"),
            (30, None, "UnpopInData"),
            (31, None, "UnpopExtraStorage"),
            (40, None, "GetIndirectLayerProducerHandle"),
            (
                50,
                Some(Self::report_visible_error_handler),
                "ReportVisibleError",
            ),
            (
                51,
                Some(Self::report_visible_error_with_error_context_handler),
                "ReportVisibleErrorWithErrorContext",
            ),
            (
                60,
                Some(Self::get_main_applet_application_desired_language_handler),
                "GetMainAppletApplicationDesiredLanguage",
            ),
            (
                70,
                Some(Self::get_current_application_id_handler),
                "GetCurrentApplicationId",
            ),
            (80, None, "RequestExitToSelf"),
            (90, None, "CreateApplicationAndPushAndRequestToLaunch"),
            (100, None, "CreateGameMovieTrimmer"),
            (101, None, "ReserveResourceForMovieOperation"),
            (102, None, "UnreserveResourceForMovieOperation"),
            (
                110,
                Some(Self::get_main_applet_available_users_handler),
                "GetMainAppletAvailableUsers",
            ),
            (120, None, "GetLaunchStorageInfoForDebug"),
            (130, None, "GetGpuErrorDetectedSystemEvent"),
            (140, None, "SetApplicationMemoryReservation"),
            (
                150,
                Some(Self::should_set_gpu_time_slice_manually_handler),
                "ShouldSetGpuTimeSliceManually",
            ),
            (160, Some(Self::get_library_applet_info_ex_handler), "GetLibraryAppletInfoEx"),
        ]);
        Self {
            system,
            applet,
            broker,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn pop_domain_storage(ctx: &mut HLERequestContext) -> Option<Vec<u8>> {
        let mut rp = RequestParser::new(ctx);
        let object_id = rp.pop_u32();
        if object_id == 0 {
            log::error!("ILibraryAppletSelfAccessor storage argument is null");
            return None;
        }

        let handler = {
            let manager = ctx.get_manager()?;
            let manager = manager.lock().unwrap();
            if !manager.is_domain() {
                log::error!("ILibraryAppletSelfAccessor storage argument requires domain IPC");
                return None;
            }
            manager.domain_handler(object_id as usize - 1)?.clone()
        };

        let storage = handler.as_any().downcast_ref::<IStorage>()?;
        Some(storage.get_data())
    }

    fn pop_in_data_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletSelfAccessor) };
        log::info!("ILibraryAppletSelfAccessor::PopInData called");
        match service.broker.get_in_data().pop() {
            Ok(data) => {
                let storage = Arc::new(IStorage::new_with_system(service.system, data));
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
                rb.push_result(RESULT_SUCCESS);
                rb.push_ipc_interface(storage);
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }

    fn push_out_data_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletSelfAccessor) };
        log::info!("ILibraryAppletSelfAccessor::PushOutData called");
        let Some(data) = Self::pop_domain_storage(ctx) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };

        service.broker.get_out_data().push(data);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn pop_interactive_in_data_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletSelfAccessor) };
        log::info!("ILibraryAppletSelfAccessor::PopInteractiveInData called");
        match service.broker.get_interactive_in_data().pop() {
            Ok(data) => {
                let storage = Arc::new(IStorage::new_with_system(service.system, data));
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
                rb.push_result(RESULT_SUCCESS);
                rb.push_ipc_interface(storage);
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }

    fn push_interactive_out_data_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletSelfAccessor) };
        log::info!("ILibraryAppletSelfAccessor::PushInteractiveOutData called");
        let Some(data) = Self::pop_domain_storage(ctx) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };

        service.broker.get_interactive_out_data().push(data);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_pop_in_data_event_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletSelfAccessor) };
        log::info!("ILibraryAppletSelfAccessor::GetPopInDataEvent called");
        let object_id = service
            .broker
            .get_in_data()
            .get_event_object_id(ctx)
            .unwrap_or(0);

        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    fn get_pop_interactive_in_data_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletSelfAccessor) };
        log::info!("ILibraryAppletSelfAccessor::GetPopInteractiveInDataEvent called");
        let object_id = service
            .broker
            .get_interactive_in_data()
            .get_event_object_id(ctx)
            .unwrap_or(0);

        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    /// Port of `ILibraryAppletSelfAccessor::GetLibraryAppletInfo`.
    fn get_library_applet_info(&self) -> LibraryAppletInfo {
        let applet = self.applet.lock().unwrap();
        LibraryAppletInfo {
            applet_id: applet.applet_id,
            library_applet_mode: applet.library_applet_mode,
        }
    }

    fn get_library_applet_info_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletSelfAccessor) };
        log::info!("ILibraryAppletSelfAccessor::GetLibraryAppletInfo called");
        let info = service.get_library_applet_info();

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw(&info);
    }

    fn get_caller_identity(
        applet: &Arc<Mutex<crate::hle::service::am::applet::Applet>>,
    ) -> AppletIdentityInfo {
        let caller = applet.lock().unwrap().caller_applet.upgrade();
        if let Some(caller) = caller {
            let caller = caller.lock().unwrap();
            AppletIdentityInfo {
                applet_id: caller.applet_id as u32,
                _padding: [0; 4],
                application_id: caller.program_id,
            }
        } else {
            AppletIdentityInfo {
                applet_id: AppletId::QLaunch as u32,
                _padding: [0; 4],
                application_id: 0x0100_0000_0000_1000,
            }
        }
    }

    fn exit_process_and_return_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletSelfAccessor) };
        log::info!("ILibraryAppletSelfAccessor::ExitProcessAndReturn called");
        service.applet.lock().unwrap().process.terminate();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_main_applet_identity_info_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) ILibraryAppletSelfAccessor::GetMainAppletIdentityInfo called");
        let identity = AppletIdentityInfo {
            applet_id: AppletId::QLaunch as u32,
            _padding: [0; 4],
            application_id: 0x0100_0000_0000_1000,
        };
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw(&identity);
    }

    fn can_use_application_core_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        log::warn!("(STUBBED) ILibraryAppletSelfAccessor::CanUseApplicationCore called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(false);
    }

    fn get_caller_applet_identity_info_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletSelfAccessor) };
        log::info!("ILibraryAppletSelfAccessor::GetCallerAppletIdentityInfo called");
        let identity = Self::get_caller_identity(&service.applet);
        let mut rb = ResponseBuilder::new(ctx, 6, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw(&identity);
    }

    fn get_main_applet_application_control_property_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletSelfAccessor) };
        log::warn!(
            "(STUBBED) ILibraryAppletSelfAccessor::GetMainAppletApplicationControlProperty called"
        );
        let application = Self::get_caller_identity(&service.applet);
        let (result, nacp) = service
            .system
            .get()
            .arp_manager()
            .lock()
            .unwrap()
            .get_control_property(application.application_id);
        if let Some(nacp) = nacp {
            ctx.write_buffer(&nacp, 0);
        }
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn get_main_applet_storage_id_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::info!("(STUBBED) ILibraryAppletSelfAccessor::GetMainAppletStorageId called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u8(crate::file_sys::romfs_factory::StorageId::NandUser as u8);
    }

    fn get_caller_applet_identity_info_stack_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletSelfAccessor) };
        log::info!("ILibraryAppletSelfAccessor::GetCallerAppletIdentityInfoStack called");
        let capacity = ctx.get_write_buffer_size(0) / core::mem::size_of::<AppletIdentityInfo>();
        let mut identities = Vec::with_capacity(capacity);
        let mut current = Some(Arc::clone(&service.applet));
        while let Some(applet) = current {
            if identities.len() >= capacity {
                break;
            }
            identities.push(Self::get_caller_identity(&applet));
            current = applet.lock().unwrap().caller_applet.upgrade();
        }
        if !identities.is_empty() {
            let bytes = unsafe {
                core::slice::from_raw_parts(
                    identities.as_ptr() as *const u8,
                    identities.len() * core::mem::size_of::<AppletIdentityInfo>(),
                )
            };
            ctx.write_buffer(bytes, 0);
        }
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(identities.len() as i32);
    }

    fn get_desirable_keyboard_layout_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) ILibraryAppletSelfAccessor::GetDesirableKeyboardLayout called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u32(0);
    }

    fn report_visible_error_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let mut rp = RequestParser::new(ctx);
        let error_code = rp.pop_raw::<ErrorCode>();
        log::warn!(
            "(STUBBED) ILibraryAppletSelfAccessor::ReportVisibleError called, error {}-{}",
            error_code.category,
            error_code.number
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn report_visible_error_with_error_context_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let mut rp = RequestParser::new(ctx);
        let error_code = rp.pop_raw::<ErrorCode>();
        let _error_context = ctx.read_buffer(0);
        log::warn!(
            "(STUBBED) ILibraryAppletSelfAccessor::ReportVisibleErrorWithErrorContext called, error {}-{}",
            error_code.category,
            error_code.number
        );
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_main_applet_application_desired_language_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletSelfAccessor) };
        let identity = Self::get_caller_identity(&service.applet);
        let fs_controller = service.system.get().get_filesystem_controller();
        let fs_controller = fs_controller.lock().unwrap();
        let mut supported_languages = 0;
        if let Some(provider) = service.system.get().get_content_provider() {
            let provider = provider.lock().unwrap();
            let metadata = PatchManager::new(identity.application_id, &fs_controller, &*provider)
                .get_control_metadata();
            if let Some(nacp) = metadata.0 {
                supported_languages = nacp.get_supported_languages();
            } else {
                let metadata = PatchManager::new(
                    get_update_title_id(identity.application_id),
                    &fs_controller,
                    &*provider,
                )
                .get_control_metadata();
                if let Some(nacp) = metadata.0 {
                    supported_languages = nacp.get_supported_languages();
                }
            }
        }

        let read_only = IReadOnlyApplicationControlDataInterface::new(service.system);
        let result = read_only
            .get_application_desired_language(supported_languages)
            .and_then(|language| read_only.convert_application_language_to_language_code(language));
        match result {
            Ok(language_code) => {
                let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
                rb.push_result(RESULT_SUCCESS);
                rb.push_u64(language_code);
            }
            Err(result) => {
                let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
                rb.push_result(result);
            }
        }
    }

    fn get_current_application_id_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletSelfAccessor) };
        log::warn!("(STUBBED) ILibraryAppletSelfAccessor::GetCurrentApplicationId called");
        let identity = Self::get_caller_identity(&service.applet);
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(identity.application_id);
    }

    fn get_main_applet_available_users_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let manager = crate::hle::service::acc::profile_manager::ProfileManager::new();
        let user_count = manager.get_user_count();
        let users = manager.get_all_users();
        if user_count > 0 {
            let bytes = unsafe {
                core::slice::from_raw_parts(users.as_ptr() as *const u8, user_count * 16)
            };
            ctx.write_buffer(bytes, 0);
        }

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(user_count > 0);
        rb.push_i32(if user_count > 0 {
            user_count as i32
        } else {
            -1
        });
    }

    fn should_set_gpu_time_slice_manually_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::info!("(STUBBED) ILibraryAppletSelfAccessor::ShouldSetGpuTimeSliceManually called");
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(false);
    }

    fn get_library_applet_info_ex_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletSelfAccessor) };
        log::info!("ILibraryAppletSelfAccessor::GetLibraryAppletInfoEx called");
        let info = service.get_library_applet_info();
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_raw(&info);
    }
}

impl SessionRequestHandler for ILibraryAppletSelfAccessor {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }

    fn service_name(&self) -> &str {
        "am::ILibraryAppletSelfAccessor"
    }
}

impl ServiceFramework for ILibraryAppletSelfAccessor {
    fn get_service_name(&self) -> &str {
        "am::ILibraryAppletSelfAccessor"
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
    fn extended_info_returns_actual_applet_identity_and_mode() {
        let mut applet = crate::hle::service::am::applet::Applet::new(
            SystemRef::null(), crate::hle::service::os::process::Process::new(), false,
        );
        applet.applet_id = AppletId::PhotoViewer;
        applet.library_applet_mode = LibraryAppletMode::PartialForeground;
        applet.caller_applet_broker = Some(Arc::new(AppletDataBroker::new()));
        let service = ILibraryAppletSelfAccessor::new(SystemRef::null(), Arc::new(Mutex::new(applet)));
        let mut ctx = HLERequestContext::new();
        service.handlers()[&160].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(ctx.cmd_buf[6], RESULT_SUCCESS.get_inner_value());
        assert_eq!(ctx.cmd_buf[8], AppletId::PhotoViewer as u32);
        assert_eq!(ctx.cmd_buf[9], LibraryAppletMode::PartialForeground as u32);
    }

    #[test]
    fn library_applet_info_preserves_upstream_field_order() {
        let info = LibraryAppletInfo {
            applet_id: AppletId::PhotoViewer,
            library_applet_mode: LibraryAppletMode::AllForeground,
        };
        let bytes = unsafe {
            core::slice::from_raw_parts(
                &info as *const LibraryAppletInfo as *const u8,
                core::mem::size_of::<LibraryAppletInfo>(),
            )
        };

        assert_eq!(bytes, &[0x15, 0, 0, 0, 0, 0, 0, 0]);
    }
}
