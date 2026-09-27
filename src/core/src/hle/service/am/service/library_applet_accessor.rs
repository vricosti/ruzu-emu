// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/service/library_applet_accessor.h
//! Port of zuyu/src/core/hle/service/am/service/library_applet_accessor.cpp

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::core::SystemRef;
use crate::hle::service::acc::profile_manager::ProfileManager;
use crate::hle::service::am::am_types::AppletId;
use crate::hle::service::am::frontend::applet_profile_select::{UiReturnArg, UiSettings, UiSettingsV1};
use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};
use crate::hle::service::am::applet_data_broker::AppletDataBroker;
use crate::hle::service::am::service::storage::IStorage;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// Upstream ReplaceEmptyUuidWithCurrentUser. The caller verifies UiReturnArg's
/// exact size; explicit bytes avoid alignment assumptions on storage buffers.
fn replace_empty_uuid_with_current_user(data: &mut [u8]) {
    if data[8..24].iter().any(|&byte| byte != 0) {
        return;
    }
    let profile_manager = ProfileManager::new();
    // Construction can repair current_user, so read it only afterwards.
    let current_user = *common::settings::values().current_user.get_value();
    if let Some(uuid) = profile_manager.get_user(current_user as usize) {
        data[..8].copy_from_slice(&0u64.to_le_bytes());
        data[8..24].copy_from_slice(&uuid.to_le_bytes());
    }
}

/// Upstream EnableSingleUserPlay; used only for the two exact UiSettings sizes.
fn enable_single_user_play(data: &mut [u8]) {
    const DISPLAY_OPTIONS_OFFSET: usize = 0x90;
    const IS_SKIP_ENABLED_OFFSET: usize = 1;
    const SHOW_SKIP_BUTTON_OFFSET: usize = 4;
    data[DISPLAY_OPTIONS_OFFSET + IS_SKIP_ENABLED_OFFSET] = 1;
    data[DISPLAY_OPTIONS_OFFSET + SHOW_SKIP_BUTTON_OFFSET] = 1;
}

/// IPC command table for ILibraryAppletAccessor:
/// - 0: GetAppletStateChangedEvent
/// - 1: IsCompleted
/// - 10: Start
/// - 20: RequestExit
/// - 25: Terminate
/// - 30: GetResult
/// - 50: SetOutOfFocusApplicationSuspendingEnabled (unimplemented)
/// - 60: PresetLibraryAppletGpuTimeSliceZero
/// - 100: PushInData
/// - 101: PopOutData
/// - 102: PushExtraStorage (unimplemented)
/// - 103: PushInteractiveInData
/// - 104: PopInteractiveOutData
/// - 105: GetPopOutDataEvent
/// - 106: GetPopInteractiveOutDataEvent
/// - 110: NeedsToExitProcess (unimplemented)
/// - 120: GetLibraryAppletInfo (unimplemented)
/// - 150: RequestForAppletToGetForeground (unimplemented)
/// - 160: GetIndirectLayerConsumerHandle
pub struct ILibraryAppletAccessor {
    system: SystemRef,
    applet: Arc<Mutex<crate::hle::service::am::applet::Applet>>,
    /// Matches upstream `const std::shared_ptr<AppletDataBroker> m_broker`.
    broker: Arc<AppletDataBroker>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ILibraryAppletAccessor {
    /// Upstream ILibraryAppletAccessor::GetApplet.
    pub fn get_applet(&self) -> Arc<Mutex<crate::hle::service::am::applet::Applet>> {
        self.applet.clone()
    }
    pub fn new(
        system: SystemRef,
        broker: Arc<AppletDataBroker>,
        applet: Arc<Mutex<crate::hle::service::am::applet::Applet>>,
    ) -> Self {
        let handlers = build_handler_map(&[
            (
                0,
                Some(Self::get_applet_state_changed_event_handler),
                "GetAppletStateChangedEvent",
            ),
            (1, Some(Self::is_completed_handler), "IsCompleted"),
            (10, Some(Self::start_handler), "Start"),
            (20, Some(Self::request_exit_handler), "RequestExit"),
            (25, Some(Self::terminate_handler), "Terminate"),
            (30, Some(Self::get_result_handler), "GetResult"),
            (50, None, "SetOutOfFocusApplicationSuspendingEnabled"),
            (
                60,
                Some(Self::preset_library_applet_gpu_time_slice_zero_handler),
                "PresetLibraryAppletGpuTimeSliceZero",
            ),
            (100, Some(Self::push_in_data_handler), "PushInData"),
            (101, Some(Self::pop_out_data_handler), "PopOutData"),
            (102, None, "PushExtraStorage"),
            (
                103,
                Some(Self::push_interactive_in_data_handler),
                "PushInteractiveInData",
            ),
            (
                104,
                Some(Self::pop_interactive_out_data_handler),
                "PopInteractiveOutData",
            ),
            (
                105,
                Some(Self::get_pop_out_data_event_handler),
                "GetPopOutDataEvent",
            ),
            (
                106,
                Some(Self::get_pop_interactive_out_data_event_handler),
                "GetPopInteractiveOutDataEvent",
            ),
            (110, None, "NeedsToExitProcess"),
            (120, None, "GetLibraryAppletInfo"),
            (150, None, "RequestForAppletToGetForeground"),
            (
                160,
                Some(Self::get_indirect_layer_consumer_handle_handler),
                "GetIndirectLayerConsumerHandle",
            ),
            (170, Some(Self::unknown170_handler), "Unknown170"),
        ]);
        Self {
            system,
            applet,
            broker,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn unknown170_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let handle = service.applet.lock().unwrap().ensure_unknown_event_object_id(ctx);
        let Some(handle) = handle else {
            ResponseBuilder::new(ctx, 2, 0, 0).push_result(crate::hle::result::RESULT_UNKNOWN);
            return;
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(handle);
    }

    fn get_applet_state_changed_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::GetAppletStateChangedEvent called");
        let handle = service
            .applet
            .lock()
            .unwrap()
            .ensure_state_changed_event_object_id(ctx)
            .unwrap_or(0);

        if std::env::var_os("RUZU_TRACE_APPLET_RETURN").is_some() {
            let applet = service.applet.lock().unwrap();
            log::info!(
                "[APPLET_RETURN] GetAppletStateChangedEvent aruid={} object_id={}",
                applet.aruid.pid,
                handle
            );
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(handle);
    }

    fn is_completed_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::IsCompleted called");
        let is_completed = service.applet.lock().unwrap().is_completed;

        if std::env::var_os("RUZU_TRACE_APPLET_RETURN").is_some() {
            let aruid = service.applet.lock().unwrap().aruid.pid;
            log::info!(
                "[APPLET_RETURN] IsCompleted aruid={} completed={}",
                aruid,
                is_completed
            );
        }

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(is_completed);
    }

    fn get_result_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::GetResult called");
        let result = ResultCode::new(service.applet.lock().unwrap().terminate_result);

        if std::env::var_os("RUZU_TRACE_APPLET_RETURN").is_some() {
            let aruid = service.applet.lock().unwrap().aruid.pid;
            log::info!(
                "[APPLET_RETURN] GetResult aruid={} result=0x{:X}",
                aruid,
                result.get_inner_value()
            );
        }

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(result);
    }

    fn preset_library_applet_gpu_time_slice_zero_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::info!("(STUBBED) ILibraryAppletAccessor::PresetLibraryAppletGpuTimeSliceZero called");
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn start_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::Start called");
        {
            let mut applet = service.applet.lock().unwrap();
            applet.process.run();
        }
        service.frontend_execute();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn request_exit_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::RequestExit called");
        {
            let applet = service.applet.lock().unwrap();
            applet.lifecycle_manager.request_exit();
        }
        service.frontend_request_exit();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn terminate_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::Terminate called");
        {
            let mut applet = service.applet.lock().unwrap();
            applet.process.terminate();
        }
        service.frontend_request_exit();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_indirect_layer_consumer_handle_handler(
        _this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        log::warn!("(STUBBED) ILibraryAppletAccessor::GetIndirectLayerConsumerHandle called");
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(0xdeadbeef);
    }

    fn pop_domain_storage(ctx: &mut HLERequestContext) -> Option<Vec<u8>> {
        let mut rp = RequestParser::new(ctx);
        let object_id = rp.pop_u32();
        if object_id == 0 {
            log::error!("ILibraryAppletAccessor storage argument is null");
            return None;
        }

        let handler = {
            let manager = ctx.get_manager()?;
            let manager = manager.lock().unwrap();
            if !manager.is_domain() {
                log::error!("ILibraryAppletAccessor storage argument requires domain IPC");
                return None;
            }
            manager.domain_handler(object_id as usize - 1)?.clone()
        };

        let storage = handler.as_any().downcast_ref::<IStorage>()?;
        Some(storage.get_data())
    }

    fn push_in_data_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::PushInData called");

        let Some(mut data) = Self::pop_domain_storage(ctx) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };

        if service.applet.lock().unwrap().applet_id == AppletId::ProfileSelect
            && (data.len() == std::mem::size_of::<UiSettings>()
                || data.len() == std::mem::size_of::<UiSettingsV1>())
        {
            enable_single_user_play(&mut data);
        }
        service.broker.get_in_data().push(data);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn pop_out_data_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::PopOutData called");

        match service.broker.get_out_data().pop() {
            Ok(mut data) => {
                // Do not change caller lifecycle state if Pop failed. Snapshot
                // the child before locking its caller to avoid nested locks.
                let (caller, is_frontend, applet_id) = {
                    let applet = service.applet.lock().unwrap();
                    (applet.caller_applet.upgrade(), applet.frontend.is_some(), applet.applet_id)
                };
                if let Some(caller) = caller {
                    let mut caller = caller.lock().unwrap();
                    let lifecycle = &mut caller.lifecycle_manager;
                    let focus_changed = lifecycle.update_requested_focus_state();
                    let is_front_app = is_frontend && lifecycle.is_application();
                    if focus_changed {
                        lifecycle.signal_system_event_if_needed();
                    } else if is_front_app {
                        lifecycle.request_focus_state_changed_notification();
                    }
                }
                if applet_id == AppletId::ProfileSelect
                    && data.len() == std::mem::size_of::<UiReturnArg>()
                {
                    replace_empty_uuid_with_current_user(&mut data);
                }
                if std::env::var_os("RUZU_TRACE_APPLET_RETURN").is_some() {
                    log::info!("[APPLET_RETURN] PopOutData size={}", data.len());
                }
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

    fn push_interactive_in_data_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::PushInteractiveInData called");

        let Some(data) = Self::pop_domain_storage(ctx) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };

        service.broker.get_interactive_in_data().push(data);
        service.frontend_execute_interactive();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn pop_interactive_out_data_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::PopInteractiveOutData called");

        match service.broker.get_interactive_out_data().pop() {
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

    fn get_pop_out_data_event_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::GetPopOutDataEvent called");
        let object_id = service
            .broker
            .get_out_data()
            .get_event_object_id(ctx)
            .unwrap_or(0);

        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    fn get_pop_interactive_out_data_event_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletAccessor) };
        log::debug!("ILibraryAppletAccessor::GetPopInteractiveOutDataEvent called");
        let object_id = service
            .broker
            .get_interactive_out_data()
            .get_event_object_id(ctx)
            .unwrap_or(0);

        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(object_id);
    }

    fn frontend_execute(&self) {
        let mut applet = self.applet.lock().unwrap();
        let complete = if let Some(frontend) = applet.frontend.as_mut() {
            frontend.initialize();
            frontend.execute();
            frontend.is_complete()
        } else {
            false
        };
        drop(applet);
        if complete {
            crate::hle::service::am::frontend::applets::exit(self.system, &Arc::downgrade(&self.applet));
        }
    }

    fn frontend_execute_interactive(&self) {
        let mut applet = self.applet.lock().unwrap();
        let complete = if let Some(frontend) = applet.frontend.as_mut() {
            frontend.execute_interactive();
            frontend.execute();
            frontend.is_complete()
        } else {
            false
        };
        drop(applet);
        if complete {
            crate::hle::service::am::frontend::applets::exit(self.system, &Arc::downgrade(&self.applet));
        }
    }

    fn frontend_request_exit(&self) {
        let mut applet = self.applet.lock().unwrap();
        let complete = if let Some(frontend) = applet.frontend.as_mut() {
            frontend.request_exit();
            frontend.is_complete()
        } else {
            false
        };
        drop(applet);
        if complete {
            crate::hle::service::am::frontend::applets::exit(self.system, &Arc::downgrade(&self.applet));
        }
    }
}

impl SessionRequestHandler for ILibraryAppletAccessor {
    fn as_any(&self) -> &dyn std::any::Any { self }

    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
}

impl ServiceFramework for ILibraryAppletAccessor {
    fn get_service_name(&self) -> &str {
        "am::ILibraryAppletAccessor"
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
    use crate::hle::service::am::applet::Applet;
    use crate::hle::service::os::process::Process;

    #[test]
    fn single_user_play_changes_only_the_two_display_options() {
        use crate::hle::service::am::frontend::applet_profile_select::UiSettingsDisplayOptions;
        assert_eq!(std::mem::offset_of!(super::UiSettings, display_options), 0x90);
        assert_eq!(std::mem::offset_of!(super::UiSettingsV1, display_options), 0x90);
        assert_eq!(std::mem::offset_of!(UiSettingsDisplayOptions, is_skip_enabled), 1);
        assert_eq!(std::mem::offset_of!(UiSettingsDisplayOptions, show_skip_button), 4);
        for size in [0x98, 0xa0] {
            let mut data: Vec<u8> = (0..size).map(|i| i as u8).collect();
            let before = data.clone();
            super::enable_single_user_play(&mut data);
            for index in 0..size {
                assert_eq!(data[index], if index == 0x91 || index == 0x94 { 1 } else { before[index] });
            }
        }
    }

    #[test]
    fn profile_return_fallback_preserves_uuid_and_uses_current_user() {
        const CHILD: &str = "RUZU_APPLET_PROFILE_RETURN_TEST_ROOT";
        if let Some(root) = std::env::var_os(CHILD) {
            use crate::hle::service::acc::profile_manager::{ProfileManager, MAX_USERS, PROFILE_USERNAME_SIZE};
            use common::fs::path_util::{set_ruzu_path, RuzuPath};
            set_ruzu_path(RuzuPath::NANDDir, std::path::Path::new(&root));
            assert_eq!(std::mem::size_of::<super::UiReturnArg>(), 24);
            assert_eq!(std::mem::offset_of!(super::UiReturnArg, result), 0);
            assert_eq!(std::mem::offset_of!(super::UiReturnArg, uuid_selected), 8);

            // Any nonzero UUID byte preserves the full result, even an error,
            // without constructing ProfileManager or creating a default user.
            for byte in 8..24 {
                let mut data = [0u8; 24];
                data[..8].fill(0xab);
                data[byte] = 1;
                let before = data;
                super::replace_empty_uuid_with_current_user(&mut data);
                assert_eq!(data, before);
            }
            let save = std::path::Path::new(&root)
                .join("system/save/8000000000000010/su/avators/profiles.dat");
            assert!(!save.exists());

            common::settings::values_mut().current_user.set_value(6);
            let mut empty = [0u8; 24];
            empty[..8].fill(0xab);
            super::replace_empty_uuid_with_current_user(&mut empty);
            assert_eq!(&empty[..8], &[0; 8]);
            assert!(save.exists());
            assert_eq!(*common::settings::values().current_user.get_value(), 0);
            let mut manager = ProfileManager::new();
            assert_eq!(&empty[8..], &manager.get_user(0).unwrap().to_le_bytes());

            let mut name = [0; PROFILE_USERNAME_SIZE];
            name[..4].copy_from_slice(b"Test");
            let selected = u128::from_le_bytes([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
            assert_eq!(manager.create_new_user(selected, &name), RESULT_SUCCESS);
            manager.write_user_save_file();
            common::settings::values_mut().current_user.set_value(1);
            empty.fill(0);
            super::replace_empty_uuid_with_current_user(&mut empty);
            assert_eq!(&empty[8..], &selected.to_le_bytes());

            // Fill every slot so constructor clamping does not repair the
            // setting. GetUser still receives the original invalid index.
            for index in 2..MAX_USERS {
                assert_eq!(manager.create_new_user(index as u128 + 1, &name), RESULT_SUCCESS);
            }
            manager.write_user_save_file();
            for index in [-1, MAX_USERS as i32] {
                common::settings::values_mut().current_user.set_value(index);
                let reopened = ProfileManager::new();
                let clamped = index.clamp(0, MAX_USERS as i32 - 1) as usize;
                assert_eq!(reopened.get_last_opened_user(), manager.get_user(clamped).unwrap());
                empty.fill(0);
                empty[..8].fill(0xab);
                let before = empty;
                super::replace_empty_uuid_with_current_user(&mut empty);
                assert_eq!(empty, before);
            }
            return;
        }
        let root = std::env::temp_dir().join(format!("ruzu-applet-profile-return-{}-{}",
            std::process::id(), std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&root).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", std::thread::current().name().unwrap(), "--test-threads=1"])
            .env(CHILD, &root).status().unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert!(status.success());
    }

    #[test]
    fn pop_out_data_notifies_only_after_success_and_without_spurious_resume() {
        use crate::hle::service::am::am_types::AppletMessage;
        use crate::hle::service::am::lifecycle_manager::ActivityState;
        use crate::hle::service::am::frontend::applet_general::StubApplet;
        for is_frontend in [false, true] {
            for focus_changed in [false, true] {
                for enabled in [false, true] {
                    let caller = Arc::new(Mutex::new(Applet::new(SystemRef::null(), Process::new(), true)));
                    let applet = Arc::new(Mutex::new(Applet::new(SystemRef::null(), Process::new(), false)));
                    let broker = Arc::new(AppletDataBroker::new());
                    applet.lock().unwrap().caller_applet = Arc::downgrade(&caller);
                    if is_frontend {
                        applet.lock().unwrap().frontend = Some(Box::new(StubApplet::new(
                            SystemRef::null(), Arc::downgrade(&applet), broker.clone(),
                            Default::default(), Default::default(),
                        )));
                    }
                    {
                        let mut caller = caller.lock().unwrap();
                        let lifecycle = &mut caller.lifecycle_manager;
                        lifecycle.update_requested_focus_state();
                        let mut message = AppletMessage::None;
                        while lifecycle.pop_message(&mut message) {}
                        lifecycle.set_resume_notification_enabled(true);
                        lifecycle.set_focus_state_changed_notification_enabled(enabled);
                        if focus_changed {
                            lifecycle.set_activity_state(ActivityState::BackgroundObscured);
                        }
                    }
                    let accessor = ILibraryAppletAccessor::new(SystemRef::null(), broker.clone(), applet);
                    let mut ctx = HLERequestContext::new();
                    ILibraryAppletAccessor::pop_out_data_handler(&accessor, &mut ctx);
                    assert_eq!(ctx.cmd_buf[6], crate::hle::service::am::am_results::RESULT_NO_DATA_IN_CHANNEL.get_inner_value());
                    assert!(!caller.lock().unwrap().lifecycle_manager.get_system_event().is_signaled());
                    broker.get_out_data().push(vec![1, 2, 3]);
                    ILibraryAppletAccessor::pop_out_data_handler(&accessor, &mut ctx);
                    assert_eq!(ctx.cmd_buf[6], RESULT_SUCCESS.get_inner_value());
                    let mut caller = caller.lock().unwrap();
                    let lifecycle = &mut caller.lifecycle_manager;
                    let expected = enabled && (focus_changed || is_frontend);
                    assert_eq!(lifecycle.get_system_event().is_signaled(), expected);
                    let mut message = AppletMessage::None;
                    assert_eq!(lifecycle.pop_message(&mut message), expected);
                    assert_eq!(message, if expected { AppletMessage::FocusStateChanged } else { AppletMessage::None });
                    assert!(!lifecycle.pop_message(&mut message));
                }
            }
        }
    }

    #[test]
    fn unknown170_copies_the_same_unsignaled_applet_event() {
        use crate::hle::service::hle_ipc::KAutoObjectRef;
        use crate::hle::kernel::k_process::{KProcess, ProcessLock};
        use crate::hle::kernel::k_thread::{KThread, KThreadLock};
        let applet = Arc::new(Mutex::new(Applet::new(SystemRef::null(), Process::new(), false)));
        let accessor = ILibraryAppletAccessor::new(SystemRef::null(), Arc::new(AppletDataBroker::new()), Arc::clone(&applet));
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        let mut previous = None;
        for _ in 0..2 {
            let mut ctx = HLERequestContext::new_with_thread(Arc::clone(&thread), 0x2000);
            accessor.handlers[&170].handler_callback.unwrap()(&accessor, &mut ctx);
            assert_eq!(ctx.cmd_buf[6], RESULT_SUCCESS.get_inner_value());
            let [KAutoObjectRef::ObjectId(id)] = ctx.outgoing_copy_objects.as_slice() else { panic!("expected copy object"); };
            if let Some(old) = previous { assert_eq!(*id, old); }
            previous = Some(*id);
            let applet = applet.lock().unwrap();
            let event = applet.unknown_event.as_ref().unwrap().lock().unwrap();
            assert_eq!(event.object_id, *id);
            assert!(!event.is_signaled.load(std::sync::atomic::Ordering::Relaxed));
        }
    }

    #[test]
    fn frontend_start_does_not_fake_a_running_guest_process() {
        let applet = Arc::new(Mutex::new(Applet::new(
            SystemRef::null(),
            Process::new(),
            false,
        )));
        let broker = Arc::new(AppletDataBroker::new());
        let accessor = ILibraryAppletAccessor::new(SystemRef::null(), broker, Arc::clone(&applet));
        let mut ctx = HLERequestContext::new();

        ILibraryAppletAccessor::start_handler(&accessor, &mut ctx);

        assert!(!applet.lock().unwrap().is_process_running);
    }
}
