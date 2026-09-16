// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/service/application_accessor.h
//! Port of zuyu/src/core/hle/service/am/service/application_accessor.cpp

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

/// IPC command table for IApplicationAccessor:
/// - 0: GetAppletStateChangedEvent
/// - 1: IsCompleted (unimplemented)
/// - 10: Start
/// - 20: RequestExit
/// - 25: Terminate
/// - 30: GetResult
/// - 101: RequestForApplicationToGetForeground
/// - 110: TerminateAllLibraryApplets (unimplemented)
/// - 111: AreAnyLibraryAppletsLeft (unimplemented)
/// - 112: GetCurrentLibraryApplet
/// - 120: GetApplicationId (unimplemented)
/// - 121: PushLaunchParameter
/// - 122: GetApplicationControlProperty
/// - 123: GetApplicationLaunchProperty (unimplemented)
/// - 124: GetApplicationLaunchRequestInfo (unimplemented)
/// - 130: SetUsers
/// - 131: CheckRightsEnvironmentAvailable
/// - 132: GetNsRightsEnvironmentHandle
/// - 140: GetDesirableUids (unimplemented)
/// - 150: ReportApplicationExitTimeout
/// - 160: SetApplicationAttribute (unimplemented)
/// - 170: HasSaveDataAccessPermission (unimplemented)
/// - 180: PushToFriendInvitationStorageChannel (unimplemented)
/// - 190: PushToNotificationStorageChannel (unimplemented)
/// - 200: RequestApplicationSoftReset (unimplemented)
/// - 201: RestartApplicationTimer (unimplemented)
pub struct IApplicationAccessor {
    system: crate::core::SystemRef,
    applet: Arc<Mutex<crate::hle::service::am::applet::Applet>>,
    window_system: Weak<Mutex<crate::hle::service::am::window_system::WindowSystem>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IApplicationAccessor {
    pub fn new(
        system: crate::core::SystemRef,
        applet: Arc<Mutex<crate::hle::service::am::applet::Applet>>,
        window_system: Weak<Mutex<crate::hle::service::am::window_system::WindowSystem>>,
    ) -> Self {
        let handlers = build_handler_map(&[
            (0, Some(Self::get_applet_state_changed_event_handler), "GetAppletStateChangedEvent"),
            (1, None, "IsCompleted"),
            (10, Some(Self::start_handler), "Start"),
            (20, Some(Self::request_exit_handler), "RequestExit"),
            (25, Some(Self::terminate_handler), "Terminate"),
            (30, Some(Self::get_result_handler), "GetResult"),
            (
                101,
                Some(Self::request_for_application_to_get_foreground_handler),
                "RequestForApplicationToGetForeground",
            ),
            (110, None, "TerminateAllLibraryApplets"),
            (111, None, "AreAnyLibraryAppletsLeft"),
            (112, Some(Self::get_current_library_applet_handler), "GetCurrentLibraryApplet"),
            (120, None, "GetApplicationId"),
            (121, Some(Self::push_launch_parameter_handler), "PushLaunchParameter"),
            (122, Some(Self::get_application_control_property_handler), "GetApplicationControlProperty"),
            (123, None, "GetApplicationLaunchProperty"),
            (124, None, "GetApplicationLaunchRequestInfo"),
            (130, Some(Self::set_users_handler), "SetUsers"),
            (
                131,
                Some(Self::check_rights_environment_available_handler),
                "CheckRightsEnvironmentAvailable",
            ),
            (
                132,
                Some(Self::get_ns_rights_environment_handle_handler),
                "GetNsRightsEnvironmentHandle",
            ),
            (140, None, "GetDesirableUids"),
            (
                150,
                Some(Self::report_application_exit_timeout_handler),
                "ReportApplicationExitTimeout",
            ),
            (160, None, "SetApplicationAttribute"),
            (170, None, "HasSaveDataAccessPermission"),
            (180, None, "PushToFriendInvitationStorageChannel"),
            (190, None, "PushToNotificationStorageChannel"),
            (200, None, "RequestApplicationSoftReset"),
            (201, None, "RestartApplicationTimer"),
        ]);
        Self {
            system,
            applet,
            window_system,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// Port of IApplicationAccessor::Start
    /// Upstream: m_applet->process->Run()
    pub fn start(&self) {
        log::info!("IApplicationAccessor::Start called");
        let mut applet = self.applet.lock().unwrap();
        applet.process.run();
    }

    /// Port of IApplicationAccessor::RequestExit
    /// Upstream: checks exit_locked; if locked, requests exit via lifecycle_manager
    /// and updates suspension state; otherwise terminates the process directly.
    pub fn request_exit(&self) {
        log::info!("IApplicationAccessor::RequestExit called");
        let mut applet = self.applet.lock().unwrap();
        if applet.exit_locked {
            applet.lifecycle_manager.request_exit();
            applet.update_suspension_state_locked(true);
        } else {
            applet.process.terminate();
        }
    }

    /// Port of IApplicationAccessor::Terminate
    /// Upstream: m_applet->process->Terminate()
    pub fn terminate(&self) {
        log::info!("IApplicationAccessor::Terminate called");
        let mut applet = self.applet.lock().unwrap();
        applet.process.terminate();
    }

    /// Port of IApplicationAccessor::RequestForApplicationToGetForeground.
    pub fn request_for_application_to_get_foreground(&self) {
        log::info!("IApplicationAccessor::RequestForApplicationToGetForeground called");
        self.window_system
            .upgrade()
            .expect("WindowSystem must outlive active application accessors")
            .lock()
            .unwrap()
            .request_application_to_get_foreground();
    }

    /// Port of IApplicationAccessor::CheckRightsEnvironmentAvailable
    pub fn check_rights_environment_available(&self) -> bool {
        log::warn!("(STUBBED) CheckRightsEnvironmentAvailable called");
        true
    }

    /// Port of IApplicationAccessor::GetNsRightsEnvironmentHandle
    pub fn get_ns_rights_environment_handle(&self) -> u64 {
        log::warn!("(STUBBED) GetNsRightsEnvironmentHandle called");
        0xdeadbeef
    }

    /// Port of IApplicationAccessor::ReportApplicationExitTimeout
    pub fn report_application_exit_timeout(&self) {
        log::error!("ReportApplicationExitTimeout called");
    }

    fn get_applet_state_changed_event_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let id = service.applet.lock().unwrap().ensure_state_changed_event_object_id(ctx);
        let Some(id) = id else {
            ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_UNKNOWN);
            return;
        };
        let mut rb = ResponseBuilder::new(ctx, 2, 1, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_copy_object_id(id);
    }

    fn get_result_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let result = service.applet.lock().unwrap().terminate_result;
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(ResultCode::new(result));
    }

    fn get_current_library_applet_handler(_: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let is_domain = ctx.get_manager().is_some_and(|manager| manager.lock().unwrap().is_domain());
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        if !is_domain { rb.push_move_objects(0); }
        drop(rb);
        if is_domain { ctx.add_null_domain_object(); }
    }

    fn set_users_handler(_: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        // Upstream intentionally does not mutate the account selection here.
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(RESULT_SUCCESS);
    }

    fn push_launch_parameter(&self, kind: u32, data: Vec<u8>) -> ResultCode {
        if kind != crate::hle::service::am::am_types::LaunchParameterKind::AccountPreselectedUser as u32 {
            return RESULT_UNKNOWN;
        }
        self.applet.lock().unwrap().preselected_user_launch_parameter.push_back(data);
        RESULT_SUCCESS
    }

    fn push_launch_parameter_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        use super::storage::IStorage;
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let mut rp = RequestParser::new(ctx);
        let kind = rp.pop_u32();
        let id = rp.pop_u32();
        // The upstream CMIF SharedPointer input requires a domain object.
        assert!(ctx.get_domain_message_header().is_some_and(|header| header.input_object_count() > 0));
        let handler = {
            let manager = ctx.get_manager().expect("input interface manager");
            let manager = manager.lock().unwrap();
            assert!(manager.is_domain());
            manager.domain_handler(id.checked_sub(1).expect("input interface ID") as usize)
                .expect("input storage object").clone()
        };
        let storage = handler.as_any().downcast_ref::<IStorage>().expect("IStorage input interface");
        let result = service.push_launch_parameter(kind, storage.get_data());
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn get_application_control_property_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const Self) };
        let program_id = service.applet.lock().unwrap().program_id;
        let (result, data) = service.system.get().arp_manager().lock().unwrap().get_control_property(program_id);
        if let Some(data) = data.filter(|_| result.is_success()) {
            let count = data.len().min(ctx.get_write_buffer_size(0));
            ctx.write_buffer(&data[..count], 0);
        }
        ResponseBuilder::new(ctx, 2, 0, 0).push_result(result);
    }

    fn start_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationAccessor) };
        service.start();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn request_exit_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationAccessor) };
        service.request_exit();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn terminate_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationAccessor) };
        service.terminate();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn request_for_application_to_get_foreground_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationAccessor) };
        service.request_for_application_to_get_foreground();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn check_rights_environment_available_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationAccessor) };
        let available = service.check_rights_environment_available();

        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_bool(available);
    }

    fn get_ns_rights_environment_handle_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationAccessor) };
        let handle = service.get_ns_rights_environment_handle();

        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(handle);
    }

    fn report_application_exit_timeout_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationAccessor) };
        service.report_application_exit_timeout();

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }
}

impl SessionRequestHandler for IApplicationAccessor {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }

    fn service_name(&self) -> &str {
        "am::IApplicationAccessor"
    }
}

impl ServiceFramework for IApplicationAccessor {
    fn get_service_name(&self) -> &str {
        "am::IApplicationAccessor"
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
    use crate::hle::service::am::am_types::{AppletId, AppletResourceUserId};
    use crate::hle::service::am::applet::Applet;
    use crate::hle::service::am::window_system::WindowSystem;
    use crate::hle::service::os::process::Process;

    #[test]
    fn application_accessor_commands_and_launch_parameter_queue_match_upstream() {
        let applet = Arc::new(Mutex::new(Applet::new(crate::core::SystemRef::null(), Process::new(), true)));
        let service = IApplicationAccessor::new(crate::core::SystemRef::null(), Arc::clone(&applet), Weak::new());
        for command in [0, 10, 20, 25, 30, 101, 112, 121, 122, 130, 131, 132, 150] {
            assert!(service.handlers[&command].handler_callback.is_some(), "command {command}");
        }
        assert_eq!(service.push_launch_parameter(1, vec![9]), RESULT_UNKNOWN);
        assert_eq!(service.push_launch_parameter(u32::MAX, vec![9]), RESULT_UNKNOWN);
        assert_eq!(service.push_launch_parameter(2, vec![1, 2]), RESULT_SUCCESS);
        assert_eq!(service.push_launch_parameter(2, vec![3]), RESULT_SUCCESS);
        let mut applet = applet.lock().unwrap();
        assert_eq!(applet.preselected_user_launch_parameter.pop_front(), Some(vec![1, 2]));
        assert_eq!(applet.preselected_user_launch_parameter.pop_front(), Some(vec![3]));
        assert!(applet.preselected_user_launch_parameter.is_empty());
        applet.terminate_result = 0x1234;
        drop(applet);
        let mut ctx = HLERequestContext::new();
        service.handlers[&30].handler_callback.unwrap()(&service, &mut ctx);
        assert_eq!(ctx.command_buffer()[6], 0x1234);
    }

    #[test]
    fn application_state_event_is_stable_and_not_aliased_to_unknown_event() {
        use crate::hle::kernel::k_process::{KProcess, ProcessLock};
        use crate::hle::kernel::k_thread::{KThread, KThreadLock};
        use crate::hle::service::hle_ipc::KAutoObjectRef;
        let applet = Arc::new(Mutex::new(Applet::new(crate::core::SystemRef::null(), Process::new(), true)));
        let service = IApplicationAccessor::new(crate::core::SystemRef::null(), Arc::clone(&applet), Weak::new());
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let thread = Arc::new(KThreadLock::new(KThread::new()));
        thread.lock().unwrap().parent = Some(Arc::downgrade(&process));
        let mut previous = None;
        for _ in 0..2 {
            let mut ctx = HLERequestContext::new_with_thread(thread.clone(), 0);
            service.handlers[&0].handler_callback.unwrap()(&service, &mut ctx);
            let [KAutoObjectRef::ObjectId(id)] = ctx.outgoing_copy_objects.as_slice() else { panic!("copy event missing"); };
            if let Some(old) = previous { assert_eq!(*id, old); }
            previous = Some(*id);
            let unknown = applet.lock().unwrap().ensure_unknown_event_object_id(&ctx).unwrap();
            assert_ne!(*id, unknown);
        }
    }

    #[test]
    fn foreground_request_is_forwarded_to_the_window_system() {
        let window_system = Arc::new(Mutex::new(
            WindowSystem::new(crate::core::SystemRef::null()),
        ));
        let mut applet = Applet::new(crate::core::SystemRef::null(), Process::new(), true);
        applet.applet_id = AppletId::Application;
        applet.aruid = AppletResourceUserId { pid: 1 };
        applet.is_process_running = true;
        let applet = Arc::new(Mutex::new(applet));

        {
            let window_system = window_system.lock().unwrap();
            window_system.track_applet(Arc::clone(&applet), true);
            window_system.request_home_menu_to_get_foreground();
            window_system.update();
        }
        assert!(!applet.lock().unwrap().is_interactible);

        let accessor =
            IApplicationAccessor::new(crate::core::SystemRef::null(), Arc::clone(&applet), Arc::downgrade(&window_system));
        accessor.request_for_application_to_get_foreground();
        window_system.lock().unwrap().update();

        assert!(applet.lock().unwrap().is_interactible);
    }
}
