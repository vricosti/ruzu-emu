// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden `src/core/hle/service/am/service/library_applet_creator.h` and
//! `library_applet_creator.cpp`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use crate::core::SystemRef;
use crate::hle::result::{ResultCode, RESULT_SUCCESS, RESULT_UNKNOWN};
use crate::hle::service::am::am_types::{AppletId, AppletProgramId, AppletType, LibraryAppletMode};
use crate::hle::service::am::applet::Applet;
use crate::hle::service::am::applet_data_broker::AppletDataBroker;
use crate::hle::service::am::library_applet_storage::{
    create_handle_storage, create_transfer_memory_storage,
};
use crate::hle::service::am::process_creation::create_process;
use crate::hle::service::am::service::library_applet_accessor::ILibraryAppletAccessor;
use crate::hle::service::am::service::storage::IStorage;
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::os::process::Process;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use common::settings_enums::AppletMode;

/// IPC command table for ILibraryAppletCreator:
/// - 0: CreateLibraryApplet
/// - 1: TerminateAllLibraryApplets
/// - 2: AreAnyLibraryAppletsLeft
/// - 3: CreateLibraryAppletEx
/// - 10: CreateStorage
/// - 11: CreateTransferMemoryStorage
/// - 12: CreateHandleStorage
pub struct ILibraryAppletCreator {
    system: SystemRef,
    applet: Arc<Mutex<crate::hle::service::am::applet::Applet>>,
    window_system: Weak<Mutex<crate::hle::service::am::window_system::WindowSystem>>,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl ILibraryAppletCreator {
    pub fn new(
        system: SystemRef,
        applet: Arc<Mutex<crate::hle::service::am::applet::Applet>>,
        window_system: Weak<Mutex<crate::hle::service::am::window_system::WindowSystem>>,
    ) -> Self {
        let handlers = build_handler_map(&[
            (
                0,
                Some(Self::create_library_applet_handler),
                "CreateLibraryApplet",
            ),
            (1, None, "TerminateAllLibraryApplets"),
            (2, None, "AreAnyLibraryAppletsLeft"),
            (
                3,
                Some(Self::create_library_applet_ex_handler),
                "CreateLibraryAppletEx",
            ),
            (10, Some(Self::create_storage_handler), "CreateStorage"),
            (
                11,
                Some(Self::create_transfer_memory_storage_handler),
                "CreateTransferMemoryStorage",
            ),
            (
                12,
                Some(Self::create_handle_storage_handler),
                "CreateHandleStorage",
            ),
        ]);
        Self {
            system,
            applet,
            window_system,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn should_create_guest_applet(applet_id: AppletId) -> bool {
        let values = common::settings::values();
        let mode = match applet_id {
            AppletId::Cabinet => *values.cabinet_applet_mode.get_value(),
            AppletId::Controller => *values.controller_applet_mode.get_value(),
            AppletId::DataErase => *values.data_erase_applet_mode.get_value(),
            AppletId::Error => *values.error_applet_mode.get_value(),
            AppletId::NetConnect => *values.net_connect_applet_mode.get_value(),
            AppletId::ProfileSelect => *values.player_select_applet_mode.get_value(),
            AppletId::SoftwareKeyboard => *values.swkbd_applet_mode.get_value(),
            AppletId::MiiEdit => *values.mii_edit_applet_mode.get_value(),
            AppletId::Web => *values.web_applet_mode.get_value(),
            AppletId::Shop => *values.shop_applet_mode.get_value(),
            AppletId::PhotoViewer => *values.photo_viewer_applet_mode.get_value(),
            AppletId::OfflineWeb => *values.offline_web_applet_mode.get_value(),
            AppletId::LoginShare => *values.login_share_applet_mode.get_value(),
            AppletId::WebAuth => *values.wifi_web_auth_applet_mode.get_value(),
            AppletId::MyPage => *values.my_page_applet_mode.get_value(),
            _ => return true,
        };
        mode == AppletMode::LLE
    }

    fn applet_id_from_raw(value: u32) -> Option<AppletId> {
        Some(match value {
            0x00 => AppletId::None,
            0x01 => AppletId::Application,
            0x02 => AppletId::OverlayDisplay,
            0x03 => AppletId::QLaunch,
            0x04 => AppletId::Starter,
            0x0A => AppletId::Auth,
            0x0B => AppletId::Cabinet,
            0x0C => AppletId::Controller,
            0x0D => AppletId::DataErase,
            0x0E => AppletId::Error,
            0x0F => AppletId::NetConnect,
            0x10 => AppletId::ProfileSelect,
            0x11 => AppletId::SoftwareKeyboard,
            0x12 => AppletId::MiiEdit,
            0x13 => AppletId::Web,
            0x14 => AppletId::Shop,
            0x15 => AppletId::PhotoViewer,
            0x16 => AppletId::Settings,
            0x17 => AppletId::OfflineWeb,
            0x18 => AppletId::LoginShare,
            0x19 => AppletId::WebAuth,
            0x1A => AppletId::MyPage,
            0x35 => AppletId::Lhub,
            _ => return None,
        })
    }

    fn library_applet_mode_from_raw(value: u32) -> Option<LibraryAppletMode> {
        Some(match value {
            0 => LibraryAppletMode::AllForeground,
            1 => LibraryAppletMode::PartialForeground,
            2 => LibraryAppletMode::NoUi,
            3 => LibraryAppletMode::PartialForegroundIndirectDisplay,
            4 => LibraryAppletMode::AllForegroundInitiallyHidden,
            _ => return None,
        })
    }

    fn applet_id_to_program_id(applet_id: AppletId) -> u64 {
        match applet_id {
            AppletId::OverlayDisplay => AppletProgramId::OverlayDisplay as u64,
            AppletId::QLaunch => AppletProgramId::QLaunch as u64,
            AppletId::Starter => AppletProgramId::Starter as u64,
            AppletId::Auth => AppletProgramId::Auth as u64,
            AppletId::Cabinet => AppletProgramId::Cabinet as u64,
            AppletId::Controller => AppletProgramId::Controller as u64,
            AppletId::DataErase => AppletProgramId::DataErase as u64,
            AppletId::Error => AppletProgramId::Error as u64,
            AppletId::NetConnect => AppletProgramId::NetConnect as u64,
            AppletId::ProfileSelect => AppletProgramId::ProfileSelect as u64,
            AppletId::SoftwareKeyboard => AppletProgramId::SoftwareKeyboard as u64,
            AppletId::MiiEdit => AppletProgramId::MiiEdit as u64,
            AppletId::Web => AppletProgramId::Web as u64,
            AppletId::Shop => AppletProgramId::Shop as u64,
            AppletId::PhotoViewer => AppletProgramId::PhotoViewer as u64,
            AppletId::Settings => AppletProgramId::Settings as u64,
            AppletId::OfflineWeb => AppletProgramId::OfflineWeb as u64,
            AppletId::LoginShare => AppletProgramId::LoginShare as u64,
            AppletId::WebAuth => AppletProgramId::WebAuth as u64,
            AppletId::MyPage => AppletProgramId::MyPage as u64,
            _ => 0,
        }
    }

    fn create_frontend_applet(
        &self,
        applet_id: AppletId,
        mode: LibraryAppletMode,
    ) -> Arc<ILibraryAppletAccessor> {
        let program_id = Self::applet_id_to_program_id(applet_id);
        let process = Process::new();
        let applet = Arc::new(Mutex::new(Applet::new(self.system, process, false)));
        let broker = Arc::new(AppletDataBroker::new());

        {
            let mut applet_guard = applet.lock().unwrap();
            applet_guard.program_id = program_id;
            applet_guard.applet_id = applet_id;
            applet_guard.applet_type = AppletType::LibraryApplet;
            applet_guard.library_applet_mode = mode;
            applet_guard.caller_applet = Arc::downgrade(&self.applet);
            applet_guard.caller_applet_broker = Some(Arc::clone(&broker));
            applet_guard.frontend = self.system.get().frontend_applet_holder().get_applet(
                self.system,
                Arc::downgrade(&applet),
                Arc::clone(&broker),
                applet_id,
                mode,
            );
        }

        {
            let mut caller = self.applet.lock().unwrap();
            caller.child_applets.push(Arc::clone(&applet));
        }

        self.window_system
            .upgrade()
            .expect("WindowSystem must outlive active AM services")
            .lock()
            .unwrap()
            .track_applet(Arc::clone(&applet), false);

        Arc::new(ILibraryAppletAccessor::new(self.system, broker, applet))
    }

    fn create_guest_applet(
        &self,
        applet_id: AppletId,
        mode: LibraryAppletMode,
    ) -> Option<Arc<ILibraryAppletAccessor>> {
        let program_id = Self::applet_id_to_program_id(applet_id);
        if program_id == 0 {
            return None;
        }

        let process = create_process(self.system, program_id, 14, 22)?;
        let applet = Arc::new(Mutex::new(Applet::new(self.system, process, false)));
        let broker = Arc::new(AppletDataBroker::new());

        {
            let mut applet_guard = applet.lock().unwrap();
            applet_guard.program_id = program_id;
            applet_guard.applet_id = applet_id;
            applet_guard.applet_type = AppletType::LibraryApplet;
            applet_guard.library_applet_mode = mode;
            applet_guard.window_visible = mode != LibraryAppletMode::AllForegroundInitiallyHidden;
            applet_guard.caller_applet = Arc::downgrade(&self.applet);
            applet_guard.caller_applet_broker = Some(Arc::clone(&broker));
        }

        self.applet
            .lock()
            .unwrap()
            .child_applets
            .push(Arc::clone(&applet));
        self.window_system
            .upgrade()
            .expect("WindowSystem must outlive active AM services")
            .lock()
            .unwrap()
            .track_applet(Arc::clone(&applet), false);

        Some(Arc::new(ILibraryAppletAccessor::new(
            self.system,
            broker,
            applet,
        )))
    }

    fn create_library_applet_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let creator =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletCreator) };
        let mut rp = RequestParser::new(ctx);
        let applet_id_raw = rp.pop_u32();
        let mode_raw = rp.pop_u32();

        let Some(applet_id) = Self::applet_id_from_raw(applet_id_raw) else {
            log::error!(
                "ILibraryAppletCreator::CreateLibraryApplet invalid applet_id=0x{:X}",
                applet_id_raw
            );
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };
        let Some(mode) = Self::library_applet_mode_from_raw(mode_raw) else {
            log::error!(
                "ILibraryAppletCreator::CreateLibraryApplet invalid applet_mode=0x{:X}",
                mode_raw
            );
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };

        log::debug!(
            "ILibraryAppletCreator::CreateLibraryApplet called with applet_id={:?} applet_mode={:?}",
            applet_id,
            mode
        );

        let library_applet: Option<Arc<ILibraryAppletAccessor>> =
            if Self::should_create_guest_applet(applet_id) {
                creator.create_guest_applet(applet_id, mode)
            } else {
                None
            };
        let library_applet =
            library_applet.unwrap_or_else(|| creator.create_frontend_applet(applet_id, mode));

        if let Some(thread) = ctx.get_thread() {
            if let Some(process) = thread
                .lock()
                .unwrap()
                .parent
                .as_ref()
                .and_then(|parent| parent.upgrade())
            {
                let mut process = process.lock().unwrap();
                creator
                    .applet
                    .lock()
                    .unwrap()
                    .signal_library_applet_launchable_event(&mut process);
            }
        }

        Self::push_interface_response(ctx, library_applet);
    }

    fn create_library_applet_ex_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let creator =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletCreator) };
        let mut rp = RequestParser::new(ctx);
        let applet_id_raw = rp.pop_u32();
        let mode_raw = rp.pop_u32();
        let thread_id = rp.pop_u64();

        let Some(applet_id) = Self::applet_id_from_raw(applet_id_raw) else {
            log::error!(
                "ILibraryAppletCreator::CreateLibraryAppletEx invalid applet_id=0x{:X}",
                applet_id_raw
            );
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };
        let Some(mode) = Self::library_applet_mode_from_raw(mode_raw) else {
            log::error!(
                "ILibraryAppletCreator::CreateLibraryAppletEx invalid applet_mode=0x{:X}",
                mode_raw
            );
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };

        log::debug!(
            "ILibraryAppletCreator::CreateLibraryAppletEx called with applet_id={:?} applet_mode={:?} thread_id={}",
            applet_id,
            mode,
            thread_id
        );

        let library_applet: Option<Arc<ILibraryAppletAccessor>> =
            if Self::should_create_guest_applet(applet_id) {
                creator.create_guest_applet(applet_id, mode)
            } else {
                None
            };
        let library_applet =
            library_applet.unwrap_or_else(|| creator.create_frontend_applet(applet_id, mode));

        if let Some(thread) = ctx.get_thread() {
            if let Some(process) = thread
                .lock()
                .unwrap()
                .parent
                .as_ref()
                .and_then(|parent| parent.upgrade())
            {
                let mut process = process.lock().unwrap();
                creator
                    .applet
                    .lock()
                    .unwrap()
                    .signal_library_applet_launchable_event(&mut process);
            }
        }

        Self::push_interface_response(ctx, library_applet);
    }

    fn push_interface_response(
        ctx: &mut HLERequestContext,
        object: Arc<dyn SessionRequestHandler>,
    ) {
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 1);
        rb.push_result(RESULT_SUCCESS);
        rb.push_ipc_interface(object);
    }

    fn create_storage_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let creator =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletCreator) };
        let mut rp = RequestParser::new(ctx);
        let size = rp.pop_i64();
        log::debug!("ILibraryAppletCreator::CreateStorage called, size={}", size);

        if size <= 0 {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        }

        let storage = Arc::new(IStorage::new_with_system(
            creator.system,
            vec![0u8; size as usize],
        ));
        Self::push_interface_response(ctx, storage);
    }

    fn create_transfer_memory_storage_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let creator =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletCreator) };
        let mut rp = RequestParser::new(ctx);
        let is_writable = rp.pop_bool();
        // Upstream's HIPC deserializer aligns each argument to its natural
        // alignment, so the s64 begins at offset 8 after the bool.
        rp.align_for::<i64>();
        let size = rp.pop_i64();
        let transfer_memory_handle = ctx.get_copy_handle(0);

        log::debug!(
            "ILibraryAppletCreator::CreateTransferMemoryStorage called, is_writable={} size={}",
            is_writable,
            size
        );

        if size <= 0 {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        }

        let Some((memory, transfer_memory, object_id)) =
            Self::resolve_transfer_memory(ctx, transfer_memory_handle)
        else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };

        let backing =
            create_transfer_memory_storage(memory, transfer_memory, object_id, is_writable, size);
        let storage = Arc::new(IStorage::new_with_backing(creator.system, backing));
        Self::push_interface_response(ctx, storage);
    }

    fn create_handle_storage_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let creator =
            unsafe { &*(this as *const dyn ServiceFramework as *const ILibraryAppletCreator) };
        let mut rp = RequestParser::new(ctx);
        let size = rp.pop_i64();
        let transfer_memory_handle = ctx.get_copy_handle(0);

        log::debug!(
            "ILibraryAppletCreator::CreateHandleStorage called, size={}",
            size
        );

        if size <= 0 {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        }

        let Some((memory, transfer_memory, object_id)) =
            Self::resolve_transfer_memory(ctx, transfer_memory_handle)
        else {
            let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
            rb.push_result(RESULT_UNKNOWN);
            return;
        };

        let backing = create_handle_storage(memory, transfer_memory, object_id, size);
        let storage = Arc::new(IStorage::new_with_backing(creator.system, backing));
        Self::push_interface_response(ctx, storage);
    }

    fn resolve_transfer_memory(
        ctx: &HLERequestContext,
        handle: u32,
    ) -> Option<(
        Arc<Mutex<crate::memory::memory::Memory>>,
        Arc<Mutex<crate::hle::kernel::k_transfer_memory::KTransferMemory>>,
        u64,
    )> {
        let thread = ctx.get_thread()?;
        let process = {
            let thread = thread.lock().unwrap();
            thread.parent.as_ref()?.upgrade()?
        };
        let process = process.lock().unwrap();
        let object_id = process.handle_table.get_object(handle)?;
        let transfer_memory = process.get_transfer_memory_by_object_id(object_id)?;
        let memory = process.get_memory()?;
        Some((memory, transfer_memory, object_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applet_id_to_program_id_matches_upstream_switch() {
        assert_eq!(
            ILibraryAppletCreator::applet_id_to_program_id(AppletId::MiiEdit),
            AppletProgramId::MiiEdit as u64
        );
        assert_eq!(
            ILibraryAppletCreator::applet_id_to_program_id(AppletId::ProfileSelect),
            AppletProgramId::ProfileSelect as u64
        );
        assert_eq!(
            ILibraryAppletCreator::applet_id_to_program_id(AppletId::Application),
            0
        );
    }

    #[test]
    fn applet_modes_select_guest_creation() {
        // Isolate the process-wide settings from other tests.
        const CHILD: &str = "RUZU_TEST_APPLET_MODE_BANKS";
        if std::env::var_os(CHILD).is_none() {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "hle::service::am::service::library_applet_creator::tests::applet_modes_select_guest_creation"])
                .env(CHILD, "1").status().unwrap().success());
            return;
        }
        type Field = fn(&mut common::settings::Values)
            -> &mut dyn common::settings_setting::BasicSetting;
        // Same Name/name pairs as upstream ShouldCreateGuestApplet.
        let fields: &[(AppletId, Field)] = &[
            (AppletId::Cabinet, |v| &mut v.cabinet_applet_mode),
            (AppletId::Controller, |v| &mut v.controller_applet_mode),
            (AppletId::DataErase, |v| &mut v.data_erase_applet_mode),
            (AppletId::Error, |v| &mut v.error_applet_mode),
            (AppletId::NetConnect, |v| &mut v.net_connect_applet_mode),
            (AppletId::ProfileSelect, |v| &mut v.player_select_applet_mode),
            (AppletId::SoftwareKeyboard, |v| &mut v.swkbd_applet_mode),
            (AppletId::MiiEdit, |v| &mut v.mii_edit_applet_mode),
            (AppletId::Web, |v| &mut v.web_applet_mode),
            (AppletId::Shop, |v| &mut v.shop_applet_mode),
            (AppletId::PhotoViewer, |v| &mut v.photo_viewer_applet_mode),
            (AppletId::OfflineWeb, |v| &mut v.offline_web_applet_mode),
            (AppletId::LoginShare, |v| &mut v.login_share_applet_mode),
            (AppletId::WebAuth, |v| &mut v.wifi_web_auth_applet_mode),
            (AppletId::MyPage, |v| &mut v.my_page_applet_mode),
        ];
        for global in [true, false] {
            for (selected, _) in fields {
                {
                    let mut values = common::settings::values_mut();
                    for (id, field) in fields {
                        let setting = field(&mut values);
                        setting.set_global(global);
                        setting.load_string(if id == selected { "LLE" } else { "HLE" });
                    }
                }
                for (id, _) in fields {
                    assert_eq!(
                        ILibraryAppletCreator::should_create_guest_applet(*id),
                        id == selected,
                        "applet={id:?}, selected={selected:?}, global={global}",
                    );
                }
            }
        }
        assert!(ILibraryAppletCreator::should_create_guest_applet(
            AppletId::QLaunch
        ));
    }
}

impl SessionRequestHandler for ILibraryAppletCreator {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }
}

impl ServiceFramework for ILibraryAppletCreator {
    fn get_service_name(&self) -> &str {
        "am::ILibraryAppletCreator"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}
