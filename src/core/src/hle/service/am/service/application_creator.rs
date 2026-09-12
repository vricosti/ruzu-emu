// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden's `core/hle/service/am/service/application_creator.{h,cpp}`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use crate::core::SystemRef;
use crate::file_sys::nca_metadata::ContentRecordType;
use crate::file_sys::registered_cache::ContentProvider;
use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};
use crate::hle::service::am::am_types::{AppletId, AppletType, LibraryAppletMode};
use crate::hle::service::am::applet::Applet;
use crate::hle::service::am::process_creation::{create_application_process, create_process};
use crate::hle::service::am::window_system::WindowSystem;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use crate::loader::loader::ResultStatus;

use super::application_accessor::IApplicationAccessor;

/// IPC command table for IApplicationCreator:
/// - 0: CreateApplication
/// - 1: PopLaunchRequestedApplication (unimplemented)
/// - 10: CreateSystemApplication
/// - 100: PopFloatingApplicationForDevelopment (unimplemented)
pub struct IApplicationCreator {
    system: SystemRef,
    window_system: Weak<Mutex<WindowSystem>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IApplicationCreator {
    pub fn new(system: SystemRef, window_system: Weak<Mutex<WindowSystem>>) -> Self {
        let handlers = build_handler_map(&[
            (
                0,
                Some(Self::create_application_handler),
                "CreateApplication",
            ),
            (1, None, "PopLaunchRequestedApplication"),
            (
                10,
                Some(Self::create_system_application_handler),
                "CreateSystemApplication",
            ),
            (100, None, "PopFloatingApplicationForDevelopment"),
        ]);
        Self {
            system,
            window_system,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// Port of upstream anonymous `CreateGuestApplication`.
    fn create_guest_application(&self, program_id: u64) -> Option<Arc<IApplicationAccessor>> {
        let system = self.system.get();
        let storage = system.get_content_provider()?;
        let nca_raw = storage
            .lock()
            .unwrap()
            .get_entry_raw(program_id, ContentRecordType::Program)?;

        let mut control = Vec::new();
        let mut loader = None;
        let mut load_result = ResultStatus::ErrorNotInitialized;
        let process = create_application_process(
            &mut control,
            &mut loader,
            &mut load_result,
            self.system,
            nca_raw,
            program_id,
            0,
        )?;

        let mut applet = Applet::new(self.system, process, true);
        applet.program_id = program_id;
        applet.applet_id = AppletId::Application;
        applet.applet_type = AppletType::Application;
        applet.library_applet_mode = LibraryAppletMode::AllForeground;
        let applet = Arc::new(Mutex::new(applet));

        self.window_system
            .upgrade()?
            .lock()
            .unwrap()
            .track_applet(Arc::clone(&applet), true);

        Some(Arc::new(IApplicationAccessor::new(
            applet,
            self.window_system.clone(),
        )))
    }

    pub fn create_application(&self, application_id: u64) -> Option<Arc<IApplicationAccessor>> {
        log::info!(
            "IApplicationCreator::CreateApplication called, application_id={application_id:016X}"
        );
        crate::launch_timestamp_cache::save_launch_timestamp(application_id);
        self.create_guest_application(application_id)
    }

    pub fn create_system_application(
        &self,
        application_id: u64,
    ) -> Option<Arc<IApplicationAccessor>> {
        let system = self.system.get();
        system
            .get_content_provider()?
            .lock()
            .unwrap()
            .get_entry_raw(application_id, ContentRecordType::Program)?;

        let process = create_process(
            self.system,
            application_id,
            1,
            crate::hle::api_version::HOS_VERSION_MAJOR,
        )?;
        let mut applet = Applet::new(self.system, process, true);
        applet.program_id = application_id;
        applet.applet_id = AppletId::Starter;
        applet.applet_type = AppletType::LibraryApplet;
        applet.library_applet_mode = LibraryAppletMode::AllForeground;
        let applet = Arc::new(Mutex::new(applet));

        self.window_system
            .upgrade()?
            .lock()
            .unwrap()
            .track_applet(Arc::clone(&applet), true);
        let accessor = Arc::new(IApplicationAccessor::new(
            applet,
            self.window_system.clone(),
        ));
        crate::launch_timestamp_cache::save_launch_timestamp(application_id);
        Some(accessor)
    }

    fn create_application_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationCreator) };
        let mut rp = RequestParser::new(ctx);
        let application_id = rp.pop_u64();
        let Some(accessor) = service.create_application(application_id) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(accessor);
    }

    fn create_system_application_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service =
            unsafe { &*(this as *const dyn ServiceFramework as *const IApplicationCreator) };
        let mut rp = RequestParser::new(ctx);
        let application_id = rp.pop_u64();
        let Some(accessor) = service.create_system_application(application_id) else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(accessor);
    }
}

impl SessionRequestHandler for IApplicationCreator {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }

    fn service_name(&self) -> &str {
        "am::IApplicationCreator"
    }
}

impl ServiceFramework for IApplicationCreator {
    fn get_service_name(&self) -> &str {
        "am::IApplicationCreator"
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
    fn creator_registers_both_upstream_implemented_commands() {
        let window_system = Arc::new(Mutex::new(WindowSystem::new(SystemRef::null())));
        let creator = IApplicationCreator::new(SystemRef::null(), Arc::downgrade(&window_system));

        assert!(creator.handlers.get(&0).unwrap().handler_callback.is_some());
        assert!(creator
            .handlers
            .get(&10)
            .unwrap()
            .handler_callback
            .is_some());
    }
}
