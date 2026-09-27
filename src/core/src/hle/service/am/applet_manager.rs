// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/applet_manager.h
//! Port of zuyu/src/core/hle/service/am/applet_manager.cpp

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, Weak};

use super::am_types::*;
use super::applet::Applet;
use super::applet_data_broker::AppletDataBroker;
use super::window_system::WindowSystem;
use crate::core::SystemRef;
use crate::hle::kernel::k_process::ProcessLock;
use crate::hle::service::am::process_creation::create_process;
use crate::hle::service::os::process::Process;

// ---------------------------------------------------------------------------
// Upstream anonymous-namespace constant and struct
// ---------------------------------------------------------------------------

/// Upstream: `constexpr u32 LaunchParameterAccountPreselectedUserMagic = 0xC79497CA`
const LAUNCH_PARAMETER_ACCOUNT_PRESELECTED_USER_MAGIC: u32 = 0xC79497CA;

/// Upstream: `struct LaunchParameterAccountPreselectedUser`
/// Size must be 0x88 bytes (verified by static_assert in upstream).
///
/// Note: `current_user` uses `[u8; 16]` rather than `u128` because Rust's
/// `u128` has 16-byte alignment, which would insert hidden padding and make
/// the struct 0x90 bytes. C++ `Common::UUID` is a plain `std::array<u8, 0x10>`
/// with 1-byte alignment, so `[u8; 16]` matches the layout exactly.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct LaunchParameterAccountPreselectedUser {
    magic: u32,
    is_account_selected: u32,
    current_user: [u8; 16], // Common::UUID — 16 bytes, alignment 1
    _padding: [u8; 0x70],
}
const _: () = assert!(std::mem::size_of::<LaunchParameterAccountPreselectedUser>() == 0x88);

// ---------------------------------------------------------------------------
// Push-in helpers (upstream anonymous-namespace free functions)
// ---------------------------------------------------------------------------

/// Upstream: `AppletStorageChannel& InitializeFakeCallerApplet(system, applet)`
///
/// Creates a caller-applet broker on `applet` and returns a reference to its
/// in-data channel so callers can push launch arguments.
fn initialize_fake_caller_applet(
    applet: &mut Applet,
) -> &super::applet_data_broker::AppletStorageChannel {
    applet.caller_applet_broker = Some(Arc::new(AppletDataBroker::new()));
    applet.caller_applet_broker.as_ref().unwrap().get_in_data()
}

/// Upstream: `void PushInShowQlaunch(system, channel)`
fn push_in_show_qlaunch(
    system_tick: u64,
    channel: &super::applet_data_broker::AppletStorageChannel,
) {
    let arguments = CommonArguments {
        arguments_version: CommonArgumentVersion::Version3,
        size: CommonArgumentSize::Version3 as u32,
        library_version: 0,
        theme_color: ThemeColor::BasicBlack,
        play_startup_sound: true,
        _pad: [0u8; 3],
        system_tick,
    };
    let mut buf = vec![0u8; std::mem::size_of::<CommonArguments>()];
    unsafe {
        std::ptr::copy_nonoverlapping(
            &arguments as *const _ as *const u8,
            buf.as_mut_ptr(),
            buf.len(),
        );
    }
    channel.push(buf);
}

/// Upstream: `void PushInShowAlbum(system, channel)`
fn push_in_show_album(system_tick: u64, channel: &super::applet_data_broker::AppletStorageChannel) {
    let arguments = CommonArguments {
        arguments_version: CommonArgumentVersion::Version3,
        size: CommonArgumentSize::Version3 as u32,
        library_version: 1,
        theme_color: ThemeColor::BasicBlack,
        play_startup_sound: true,
        _pad: [0u8; 3],
        system_tick,
    };
    let mut arg_buf = vec![0u8; std::mem::size_of::<CommonArguments>()];
    let settings_buf = vec![2u8];
    unsafe {
        std::ptr::copy_nonoverlapping(
            &arguments as *const _ as *const u8,
            arg_buf.as_mut_ptr(),
            arg_buf.len(),
        );
    }
    channel.push(arg_buf);
    channel.push(settings_buf);
}

/// Upstream: `void PushInShowController(system, channel)`
fn push_in_show_controller(
    system_tick: u64,
    channel: &super::applet_data_broker::AppletStorageChannel,
) {
    use crate::hle::service::am::frontend::applet_controller::{
        ControllerAppletVersion, ControllerSupportArgHeader, ControllerSupportArgNew,
        ControllerSupportArgPrivate,
    };

    let common_args = CommonArguments {
        arguments_version: CommonArgumentVersion::Version3,
        size: CommonArgumentSize::Version3 as u32,
        library_version: ControllerAppletVersion::Version8 as u32,
        theme_color: ThemeColor::BasicBlack,
        play_startup_sound: true,
        _pad: [0u8; 3],
        system_tick,
    };

    let user_args = ControllerSupportArgNew {
        header: ControllerSupportArgHeader {
            player_count_min: 1,
            player_count_max: 4,
            enable_take_over_connection: true,
            enable_left_justify: false,
            enable_permit_joy_dual: true,
            enable_single_mode: false,
            enable_identification_color: false,
        },
        ..Default::default()
    };

    let private_args = ControllerSupportArgPrivate {
        arg_private_size: std::mem::size_of::<ControllerSupportArgPrivate>() as u32,
        arg_size: std::mem::size_of::<ControllerSupportArgNew>() as u32,
        is_home_menu: true,
        flag_1: true,
        mode: 0,      // ControllerSupportMode::ShowControllerSupport
        caller: 0,    // ControllerSupportCaller::Application
        style_set: 0, // NpadStyleSet::None
        joy_hold_type: 0,
    };

    let mut common_buf = vec![0u8; std::mem::size_of::<CommonArguments>()];
    let mut private_buf = vec![0u8; std::mem::size_of::<ControllerSupportArgPrivate>()];
    let mut user_buf = vec![0u8; std::mem::size_of::<ControllerSupportArgNew>()];
    unsafe {
        std::ptr::copy_nonoverlapping(
            &common_args as *const _ as *const u8,
            common_buf.as_mut_ptr(),
            common_buf.len(),
        );
        std::ptr::copy_nonoverlapping(
            &private_args as *const _ as *const u8,
            private_buf.as_mut_ptr(),
            private_buf.len(),
        );
        std::ptr::copy_nonoverlapping(
            &user_args as *const _ as *const u8,
            user_buf.as_mut_ptr(),
            user_buf.len(),
        );
    }
    channel.push(common_buf);
    channel.push(private_buf);
    channel.push(user_buf);
}

/// Upstream: `void PushInShowCabinetData(system, channel)`
fn push_in_show_cabinet_data(
    system_tick: u64,
    cabinet_mode: crate::hle::service::am::frontend::applet_cabinet::CabinetMode,
    channel: &super::applet_data_broker::AppletStorageChannel,
) {
    use crate::hle::service::am::frontend::applet_cabinet::{
        CabinetAppletVersion, StartParamForAmiiboSettings,
    };

    let arguments = CommonArguments {
        arguments_version: CommonArgumentVersion::Version3,
        size: CommonArgumentSize::Version3 as u32,
        library_version: CabinetAppletVersion::Version1 as u32,
        theme_color: ThemeColor::BasicBlack,
        play_startup_sound: true,
        _pad: [0u8; 3],
        system_tick,
    };

    let mut amiibo_settings = StartParamForAmiiboSettings::default();
    amiibo_settings.applet_mode = cabinet_mode as u8;

    let mut arg_buf = vec![0u8; std::mem::size_of::<CommonArguments>()];
    let mut settings_buf = vec![0u8; std::mem::size_of::<StartParamForAmiiboSettings>()];
    unsafe {
        std::ptr::copy_nonoverlapping(
            &arguments as *const _ as *const u8,
            arg_buf.as_mut_ptr(),
            arg_buf.len(),
        );
        std::ptr::copy_nonoverlapping(
            &amiibo_settings as *const _ as *const u8,
            settings_buf.as_mut_ptr(),
            settings_buf.len(),
        );
    }
    channel.push(arg_buf);
    channel.push(settings_buf);
}

/// Upstream: `void PushInShowMiiEditData(system, channel)`
fn push_in_show_mii_edit_data(channel: &super::applet_data_broker::AppletStorageChannel) {
    use crate::hle::service::am::frontend::applet_mii_edit_types::{
        MiiEditAppletInputCommon, MiiEditAppletMode, MiiEditAppletVersion, MiiEditV3,
    };

    let mii_arguments = MiiEditV3 {
        common: MiiEditAppletInputCommon {
            version: MiiEditAppletVersion::Version3 as i32,
            applet_mode: MiiEditAppletMode::ShowMiiEdit as u32,
        },
        input: Default::default(),
    };

    let mut buf = vec![0u8; std::mem::size_of::<MiiEditV3>()];
    unsafe {
        std::ptr::copy_nonoverlapping(
            &mii_arguments as *const _ as *const u8,
            buf.as_mut_ptr(),
            buf.len(),
        );
    }
    channel.push(buf);
}

/// Upstream: `void PushInShowSoftwareKeyboard(system, channel)`
fn push_in_show_software_keyboard(
    system_tick: u64,
    channel: &super::applet_data_broker::AppletStorageChannel,
) {
    use crate::hle::service::am::frontend::applet_software_keyboard_types::{
        SwkbdConfigCommon, SwkbdConfigNew, SwkbdTextDrawType, SwkbdType,
    };

    let arguments = CommonArguments {
        arguments_version: CommonArgumentVersion::Version3,
        size: CommonArgumentSize::Version3 as u32,
        library_version: 0x8000D, // SwkbdAppletVersion::Version524301
        theme_color: ThemeColor::BasicBlack,
        play_startup_sound: true,
        _pad: [0u8; 3],
        system_tick,
    };

    let swkbd_config = SwkbdConfigCommon {
        swkbd_type: SwkbdType::Qwerty as u32,
        max_text_length: 500,
        text_draw_type: SwkbdTextDrawType::Box as u32,
        enable_return_button: true,
        use_blur_background: true,
        ..Default::default()
    };
    let swkbd_config_new = SwkbdConfigNew::default();

    // Upstream: argument_data, then swkbd_data (config_common concatenated with config_new),
    // then work_buffer (empty — initial_string_length = 0).
    let mut arg_buf = vec![0u8; std::mem::size_of::<CommonArguments>()];
    let mut swkbd_buf =
        vec![0u8; std::mem::size_of::<SwkbdConfigCommon>() + std::mem::size_of::<SwkbdConfigNew>()];
    unsafe {
        std::ptr::copy_nonoverlapping(
            &arguments as *const _ as *const u8,
            arg_buf.as_mut_ptr(),
            arg_buf.len(),
        );
        std::ptr::copy_nonoverlapping(
            &swkbd_config as *const _ as *const u8,
            swkbd_buf.as_mut_ptr(),
            std::mem::size_of::<SwkbdConfigCommon>(),
        );
        std::ptr::copy_nonoverlapping(
            &swkbd_config_new as *const _ as *const u8,
            swkbd_buf
                .as_mut_ptr()
                .add(std::mem::size_of::<SwkbdConfigCommon>()),
            std::mem::size_of::<SwkbdConfigNew>(),
        );
    }
    channel.push(arg_buf);
    channel.push(swkbd_buf);
    // work_buffer: initial_string_length = 0, so empty — not pushed.
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LaunchType {
    #[default]
    FrontendInitiated,
    ApplicationInitiated,
}

#[derive(Debug, Clone, Copy)]
pub struct FrontendAppletParameters {
    pub program_id: ProgramId,
    pub applet_id: AppletId,
    pub applet_type: AppletType,
    pub launch_type: LaunchType,
    pub program_index: i32,
    pub previous_program_index: i32,
}

impl Default for FrontendAppletParameters {
    fn default() -> Self {
        Self {
            program_id: 0,
            applet_id: AppletId::default(),
            applet_type: AppletType::default(),
            launch_type: LaunchType::default(),
            program_index: 0,
            previous_program_index: -1,
        }
    }
}

/// Parameters needed to call `KProcess::run()`, stored alongside the pending
/// process so `set_window_system` can call it after applet tracking — matching
/// upstream `applet->process->Run()` at the end of `SetWindowSystem`.
pub struct PendingRunParameters {
    pub priority: i32,
    pub stack_size: usize,
    pub main_thread_id: u64,
    pub main_object_id: u64,
    pub is_64bit: bool,
    pub init_func: Option<Box<dyn FnOnce() + Send>>,
}

// ---------------------------------------------------------------------------
// AppletManager
// ---------------------------------------------------------------------------

/// Port of AppletManager
///
/// Upstream: `class AppletManager` in `applet_manager.h`.
///
/// Manages the lifecycle of applets. Stores a `SystemRef` matching upstream's
/// `m_system: Core::System&`, and an `Arc<Mutex<WindowSystem>>` instead of
/// upstream's raw `WindowSystem*` for safe cross-thread access.
pub struct AppletManager {
    /// Upstream: `Core::System& m_system`
    system: SystemRef,
    /// Lock protecting mutable state — upstream: `std::mutex m_lock`
    lock: Mutex<AppletManagerInner>,
    /// Condition variable — upstream: `std::condition_variable m_cv`
    cv: Condvar,
}

struct AppletManagerInner {
    /// Upstream: `WindowSystem* m_window_system` (non-owning).
    window_system: Option<Weak<Mutex<WindowSystem>>>,
    /// Upstream: `std::unique_ptr<Process> m_pending_process`
    pending_process: Option<Arc<ProcessLock>>,
    /// Upstream: `FrontendAppletParameters m_pending_parameters`
    pending_parameters: Option<FrontendAppletParameters>,
    /// Rust-specific: run parameters needed to call KProcess::run() from set_window_system.
    pending_run_params: Option<PendingRunParameters>,
}

impl AppletManager {
    pub fn new() -> Self {
        Self {
            system: SystemRef::null(),
            lock: Mutex::new(AppletManagerInner {
                window_system: None,
                pending_process: None,
                pending_parameters: None,
                pending_run_params: None,
            }),
            cv: Condvar::new(),
        }
    }

    /// Stores the system reference. Must be called once after `System` is
    /// placed in its final memory location (e.g. at end of `initialize()`).
    ///
    /// Matches upstream: `AppletManager` constructor receives `Core::System& system`.
    pub fn set_system(&mut self, system: SystemRef) {
        self.system = system;
    }

    /// Upstream `AppletManager::GetWindowSystem`.
    /// Upgrade the non-owning reference only for the duration of the caller's
    /// operation; never retain the manager lock while locking WindowSystem.
    pub fn get_window_system(&self) -> Option<Arc<Mutex<WindowSystem>>> {
        self.lock.lock().unwrap().window_system.as_ref().and_then(Weak::upgrade)
    }

    /// Upstream: `void SetWindowSystem(WindowSystem* window_system)`
    pub fn set_window_system(&self, window_system: Option<Arc<Mutex<WindowSystem>>>) {
        let mut inner = self.lock.lock().unwrap();

        inner.window_system = window_system.as_ref().map(Arc::downgrade);
        if window_system.is_none() {
            return;
        }

        // Block until CreateAndInsertByFrontendAppletParameters has been called.
        // Upstream: `m_cv.wait(lk, [&] { return m_pending_process != nullptr; })`
        while inner.pending_process.is_none() {
            let (next_inner, _timeout) = self
                .cv
                .wait_timeout(inner, std::time::Duration::from_millis(100))
                .unwrap();
            inner = next_inner;

            if !self.system.is_null() && self.system.get().is_shutting_down() {
                return;
            }
        }

        // The caller owns WindowSystem for the duration of SetWindowSystem,
        // matching the lifetime of upstream's stack-owned WindowSystem.
        let window_system = window_system.unwrap();
        let process = inner.pending_process.take().unwrap();
        let params = inner.pending_parameters.take().unwrap();
        let run_params = inner.pending_run_params.take();
        drop(inner);

        if *common::settings::values().enable_overlay.get_value()
            && window_system
                .lock()
                .unwrap()
                .get_overlay_display_applet()
                .is_none()
        {
            if let Some(overlay_process) =
                create_process(self.system, AppletProgramId::OverlayDisplay as u64, 0, 0)
            {
                let mut overlay_applet = Applet::new(self.system, overlay_process, false);
                overlay_applet.program_id = AppletProgramId::OverlayDisplay as u64;
                overlay_applet.applet_id = AppletId::OverlayDisplay;
                overlay_applet.applet_type = AppletType::OverlayApplet;
                overlay_applet.library_applet_mode = LibraryAppletMode::PartialForeground;
                overlay_applet.window_visible = true;
                overlay_applet.home_button_short_pressed_blocked = false;
                overlay_applet.home_button_long_pressed_blocked = false;
                let overlay_applet = Arc::new(Mutex::new(overlay_applet));
                window_system
                    .lock()
                    .unwrap()
                    .track_applet(Arc::clone(&overlay_applet), false);
                let started = overlay_applet.lock().unwrap().process.run();
                overlay_applet.lock().unwrap().is_process_running = started;
                log::info!(
                    "Overlay applet launched before application (initially hidden, watching home button)"
                );
            }
        }

        // Build the applet. Upstream: `auto applet = std::make_shared<Applet>(m_system, process, ...)`
        let mut applet = Applet::new(
            self.system,
            Process::with_process(process.clone()),
            params.applet_id == AppletId::Application,
        );
        applet.program_id = params.program_id;
        applet.applet_id = params.applet_id;
        applet.applet_type = params.applet_type;
        applet.previous_program_index = params.previous_program_index;

        // Upstream: push UserChannel data from previous application when
        // launched by another application.
        // `if (params.launch_type == LaunchType::ApplicationInitiated)`
        if params.launch_type == LaunchType::ApplicationInitiated {
            if !self.system.is_null() {
                let user_channel: VecDeque<Vec<u8>> = self.system.take_user_channel();
                applet.user_channel_launch_parameter = user_channel;
            }
        }

        // Upstream: push LaunchParameterAccountPreselectedUser
        {
            let mut lp = LaunchParameterAccountPreselectedUser {
                magic: LAUNCH_PARAMETER_ACCOUNT_PRESELECTED_USER_MAGIC,
                is_account_selected: 1,
                current_user: [0u8; 16],
                _padding: [0u8; 0x70],
            };

            let profile_manager = crate::hle::service::acc::profile_manager::ProfileManager::new();
            let current_user_idx = *common::settings::values().current_user.get_value() as usize;
            if let Some(uuid) = profile_manager.get_user(current_user_idx) {
                lp.current_user = uuid.to_le_bytes();
            }

            let mut buf = vec![0u8; std::mem::size_of::<LaunchParameterAccountPreselectedUser>()];
            unsafe {
                std::ptr::copy_nonoverlapping(
                    &lp as *const _ as *const u8,
                    buf.as_mut_ptr(),
                    buf.len(),
                );
            }
            applet.preselected_user_launch_parameter.push_back(buf);
        }

        // Upstream: push applet-specific input data depending on applet_id.
        let system_tick = if !self.system.is_null() {
            self.system.get().get_core_timing_ticks()
        } else {
            0
        };
        // Upstream lines 302-329 of applet_manager.cpp: switch on applet_id.
        match applet.applet_id {
            AppletId::QLaunch => {
                let channel = initialize_fake_caller_applet(&mut applet);
                push_in_show_qlaunch(system_tick, channel);
            }
            AppletId::Cabinet => {
                let channel = initialize_fake_caller_applet(&mut applet);
                let cabinet_mode = if self.system.is_null() {
                    Default::default()
                } else {
                    self.system
                        .get()
                        .frontend_applet_holder()
                        .get_cabinet_mode()
                };
                push_in_show_cabinet_data(system_tick, cabinet_mode, channel);
            }
            AppletId::MiiEdit => {
                let channel = initialize_fake_caller_applet(&mut applet);
                push_in_show_mii_edit_data(channel);
            }
            AppletId::PhotoViewer => {
                let channel = initialize_fake_caller_applet(&mut applet);
                push_in_show_album(system_tick, channel);
            }
            AppletId::SoftwareKeyboard => {
                let channel = initialize_fake_caller_applet(&mut applet);
                push_in_show_software_keyboard(system_tick, channel);
            }
            AppletId::Controller => {
                let channel = initialize_fake_caller_applet(&mut applet);
                push_in_show_controller(system_tick, channel);
            }
            _ => {}
        }

        // Upstream: `applet->lifecycle_manager.SetFocusState(FocusState::InFocus)`
        applet.is_process_running = true;
        applet
            .lifecycle_manager
            .set_focus_state(FocusState::InFocus);

        let applet = Arc::new(Mutex::new(applet));
        let ws = window_system.lock().unwrap();

        if params.applet_id == AppletId::QLaunch {
            {
                let mut ag = applet.lock().unwrap();
                ag.lifecycle_manager.set_focus_handling_mode(false);
                ag.lifecycle_manager
                    .set_out_of_focus_suspending_enabled(false);
            }
            ws.track_applet(applet, false);
            ws.request_home_menu_to_get_foreground();
        } else {
            ws.track_applet(applet, true);
            ws.request_application_to_get_foreground();
        }
        drop(ws);

        // Upstream: `applet->process->Run()` — start the application main thread.
        // In Rust we need the run parameters computed earlier in core.rs and
        // stored in AppletManagerInner alongside the pending process.
        if let Some(rp) = run_params {
            let run_result = process.lock().unwrap().run(
                rp.priority,
                rp.stack_size,
                rp.main_thread_id,
                rp.main_object_id,
                rp.is_64bit,
                rp.init_func,
            );
            match run_result {
                Ok((main_thread, _handle, _thread_id, _object_id)) => {
                    log::info!(
                        "Application process main thread created (thread_id={})",
                        main_thread.lock().unwrap().get_thread_id()
                    );
                    // Register thread with kernel and global scheduler context.
                    // Upstream: done implicitly via KProcess::Run() in SetWindowSystem.
                    if !self.system.is_null() {
                        self.system.get().register_application_thread(main_thread);
                    }
                }
                Err(e) => {
                    log::error!("Failed to run application process: 0x{:X}", e);
                }
            }
        }
    }

    /// Upstream: `void CreateAndInsertByFrontendAppletParameters(std::unique_ptr<Process>, params)`
    pub fn create_and_insert_by_frontend_applet_parameters(
        &self,
        process: Arc<ProcessLock>,
        params: FrontendAppletParameters,
        run_params: PendingRunParameters,
    ) {
        let mut inner = self.lock.lock().unwrap();
        inner.pending_process = Some(process);
        inner.pending_parameters = Some(params);
        inner.pending_run_params = Some(run_params);
        drop(inner);
        self.cv.notify_all();
    }

    /// Upstream: `void RequestExit()`
    pub fn request_exit(&self) {
        let inner = self.lock.lock().unwrap();
        if let Some(ws) = inner.window_system.as_ref().and_then(Weak::upgrade) {
            ws.lock().unwrap().on_exit_requested();
        }
    }

    /// Upstream: `void OperationModeChanged()`
    pub fn operation_mode_changed(&self) {
        let inner = self.lock.lock().unwrap();
        if let Some(ws) = inner.window_system.as_ref().and_then(Weak::upgrade) {
            ws.lock().unwrap().on_operation_mode_changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::kernel::k_process::KProcess;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn frontend_parameters_default_previous_program_index_matches_upstream() {
        let parameters = FrontendAppletParameters::default();

        assert_eq!(parameters.program_index, 0);
        assert_eq!(parameters.previous_program_index, -1);
        assert_eq!(parameters.launch_type, LaunchType::FrontendInitiated);
    }

    #[test]
    fn set_window_system_uses_frontend_program_id() {
        let manager = AppletManager::new();
        let window_system = Arc::new(Mutex::new(WindowSystem::new(SystemRef::null())));
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let expected_program_id = 0x0100_0000_0000_100d;

        {
            let mut inner = manager.lock.lock().unwrap();
            inner.pending_process = Some(process);
            inner.pending_parameters = Some(FrontendAppletParameters {
                program_id: expected_program_id,
                applet_id: AppletId::Application,
                applet_type: AppletType::Application,
                ..FrontendAppletParameters::default()
            });
            inner.pending_run_params = None;
        }

        manager.set_window_system(Some(Arc::clone(&window_system)));

        let applet = window_system
            .lock()
            .unwrap()
            .get_main_applet()
            .expect("frontend application should be tracked");
        assert_eq!(applet.lock().unwrap().program_id, expected_program_id);
    }

    #[test]
    fn set_window_system_returns_during_shutdown_without_pending_process() {
        let system = crate::core::System::new();
        let mut manager = AppletManager::new();
        manager.set_system(crate::core::SystemRef::from_ref(&system));
        let manager = Arc::new(manager);
        let window_system = Arc::new(Mutex::new(WindowSystem::new(
            crate::core::SystemRef::from_ref(&system),
        )));

        let manager_for_thread = Arc::clone(&manager);
        let window_system_for_thread = Arc::clone(&window_system);
        let waiter = std::thread::spawn(move || {
            manager_for_thread.set_window_system(Some(window_system_for_thread));
        });

        std::thread::sleep(Duration::from_millis(150));
        system.set_shutting_down(true);

        waiter.join().unwrap();
    }

    #[test]
    fn applet_manager_does_not_own_window_system() {
        let manager = AppletManager::new();
        assert!(manager.get_window_system().is_none());
        let window_system = Arc::new(Mutex::new(
            WindowSystem::new(crate::core::SystemRef::null()),
        ));
        let weak = Arc::downgrade(&window_system);

        // The None path is non-blocking; first install a weak reference
        // directly as SetWindowSystem(Some) must wait for a pending process.
        manager.lock.lock().unwrap().window_system = Some(Arc::downgrade(&window_system));
        let observed = manager.get_window_system().unwrap();
        assert!(Arc::ptr_eq(&observed, &window_system));
        assert!(manager.lock.try_lock().is_ok());
        drop(observed);
        drop(window_system);

        assert!(manager.get_window_system().is_none());

        assert!(
            weak.upgrade().is_none(),
            "AppletManager must mirror upstream's non-owning WindowSystem pointer"
        );
    }
}
