// SPDX-FileCopyrightText: Copyright 2020 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/frontend/applet_controller.h
//! Port of zuyu/src/core/hle/service/am/frontend/applet_controller.cpp

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use hid_core::hid_types::NpadStyleSet;

use crate::core::SystemRef;
use crate::frontend::applets::controller::{
    ControllerApplet, ControllerParameters, ReconfigureCallback,
};
use crate::hle::result::{ErrorModule, ResultCode, RESULT_SUCCESS};
use crate::hle::service::am::am_types::{CommonArguments, LibraryAppletMode};
use crate::hle::service::am::applet::Applet;
use crate::hle::service::am::applet_data_broker::AppletDataBroker;

use super::applets::FrontendApplet;

#[allow(dead_code)]
const RESULT_CONTROLLER_SUPPORT_CANCELED: ResultCode =
    ResultCode::from_module_description(ErrorModule::HID, 3101);
#[allow(dead_code)]
const RESULT_CONTROLLER_SUPPORT_NOT_SUPPORTED_NPAD_STYLE: ResultCode =
    ResultCode::from_module_description(ErrorModule::HID, 3102);

pub type IdentificationColor = [u8; 4];
pub type ExplainText = [u8; 0x81];

/// Port of ControllerAppletVersion
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerAppletVersion {
    Version3 = 0x3,
    Version4 = 0x4,
    Version5 = 0x5,
    Version7 = 0x7,
    Version8 = 0x8,
}

/// Port of ControllerSupportMode
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerSupportMode {
    ShowControllerSupport = 0,
    ShowControllerStrapGuide = 1,
    ShowControllerFirmwareUpdate = 2,
    ShowControllerKeyRemappingForSystem = 3,
    MaxControllerSupportMode = 4,
}

/// Port of ControllerSupportCaller
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerSupportCaller {
    Application = 0,
    System = 1,
    MaxControllerSupportCaller = 2,
}

/// Port of ControllerSupportResult
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerSupportResult {
    Success = 0,
    Cancel = 2,
}

/// Port of ControllerSupportArgPrivate
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ControllerSupportArgPrivate {
    pub arg_private_size: u32,
    pub arg_size: u32,
    pub is_home_menu: bool,
    pub flag_1: bool,
    pub mode: u8,   // ControllerSupportMode
    pub caller: u8, // ControllerSupportCaller
    pub style_set: u32,
    pub joy_hold_type: u32,
}
const _: () = assert!(std::mem::size_of::<ControllerSupportArgPrivate>() == 0x14);

/// Port of ControllerSupportArgHeader
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ControllerSupportArgHeader {
    pub player_count_min: i8,
    pub player_count_max: i8,
    pub enable_take_over_connection: bool,
    pub enable_left_justify: bool,
    pub enable_permit_joy_dual: bool,
    pub enable_single_mode: bool,
    pub enable_identification_color: bool,
}
const _: () = assert!(std::mem::size_of::<ControllerSupportArgHeader>() == 0x7);

/// Port of ControllerSupportArgOld (LibraryAppletVersion 0x3, 0x4, 0x5) — 0x21C bytes.
///
/// `identification_colors`: 4 entries × 4 bytes each.
/// `explain_text`: 4 entries × 0x81 bytes each.
/// No padding between fields (all fields have alignment ≤ 1).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ControllerSupportArgOld {
    pub header: ControllerSupportArgHeader,
    pub identification_colors: [IdentificationColor; 4],
    pub enable_explain_text: bool,
    pub explain_text: [ExplainText; 4],
}
const _: () = assert!(std::mem::size_of::<ControllerSupportArgOld>() == 0x21C);

impl Default for ControllerSupportArgOld {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

/// Port of ControllerSupportArgNew (LibraryAppletVersion 0x7, 0x8) — 0x430 bytes.
///
/// `identification_colors`: 8 entries × 4 bytes each.
/// `explain_text`: 8 entries × 0x81 bytes each.
/// No padding between fields (all fields have alignment ≤ 1).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ControllerSupportArgNew {
    pub header: ControllerSupportArgHeader,
    pub identification_colors: [IdentificationColor; 8],
    pub enable_explain_text: bool,
    pub explain_text: [ExplainText; 8],
}
const _: () = assert!(std::mem::size_of::<ControllerSupportArgNew>() == 0x430);

impl Default for ControllerSupportArgNew {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ControllerUpdateFirmwareArg {
    pub enable_force_update: bool,
    _padding: [u8; 3],
}
const _: () = assert!(std::mem::size_of::<ControllerUpdateFirmwareArg>() == 0x4);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ControllerKeyRemappingArg {
    pub unknown: u64,
    pub unknown_2: u32,
    _padding: [u32; 1],
}
const _: () = assert!(std::mem::size_of::<ControllerKeyRemappingArg>() == 0x10);

/// Port of ControllerSupportResultInfo
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ControllerSupportResultInfo {
    pub player_count: i8,
    _padding: [u8; 3],
    pub selected_id: u32,
    pub result: u32, // ControllerSupportResult
}
const _: () = assert!(std::mem::size_of::<ControllerSupportResultInfo>() == 0xC);

pub struct Controller {
    system: SystemRef,
    applet: Weak<Mutex<Applet>>,
    broker: Arc<AppletDataBroker>,
    applet_mode: LibraryAppletMode,
    frontend: Arc<dyn ControllerApplet>,
    initialized: bool,
    controller_applet_version: u32,
    controller_private_arg: ControllerSupportArgPrivate,
    controller_user_arg_old: ControllerSupportArgOld,
    controller_user_arg_new: ControllerSupportArgNew,
    controller_update_arg: ControllerUpdateFirmwareArg,
    controller_key_remapping_arg: ControllerKeyRemappingArg,
    completion: Arc<ControllerCompletion>,
    status: ResultCode,
    is_single_mode: bool,
}

#[derive(Default)]
struct ControllerCompletion {
    complete: AtomicBool,
    executing: AtomicBool,
    out_data: Mutex<Vec<u8>>,
}

impl Controller {
    pub fn new(
        system: SystemRef,
        applet: Weak<Mutex<Applet>>,
        broker: Arc<AppletDataBroker>,
        applet_mode: LibraryAppletMode,
        frontend: Arc<dyn ControllerApplet>,
    ) -> Self {
        Self {
            system,
            applet,
            broker,
            applet_mode,
            frontend,
            initialized: false,
            controller_applet_version: 0,
            controller_private_arg: ControllerSupportArgPrivate::default(),
            controller_user_arg_old: ControllerSupportArgOld::default(),
            controller_user_arg_new: ControllerSupportArgNew::default(),
            controller_update_arg: ControllerUpdateFirmwareArg::default(),
            controller_key_remapping_arg: ControllerKeyRemappingArg::default(),
            completion: Arc::new(ControllerCompletion::default()),
            status: RESULT_SUCCESS,
            is_single_mode: false,
        }
    }

    fn copy_from_prefix<T: Copy>(data: &[u8]) -> Option<T> {
        if data.len() < std::mem::size_of::<T>() {
            return None;
        }

        let mut value = std::mem::MaybeUninit::<T>::uninit();
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                value.as_mut_ptr().cast::<u8>(),
                std::mem::size_of::<T>(),
            );
            Some(value.assume_init())
        }
    }

    fn struct_to_vec<T>(value: &T) -> Vec<u8> {
        unsafe {
            std::slice::from_raw_parts((value as *const T).cast::<u8>(), std::mem::size_of::<T>())
                .to_vec()
        }
    }

    fn support_mode(&self) -> ControllerSupportMode {
        match self.controller_private_arg.mode {
            0 => ControllerSupportMode::ShowControllerSupport,
            1 => ControllerSupportMode::ShowControllerStrapGuide,
            2 => ControllerSupportMode::ShowControllerFirmwareUpdate,
            3 => ControllerSupportMode::ShowControllerKeyRemappingForSystem,
            mode => panic!("Unimplemented ControllerSupportMode={mode}"),
        }
    }

    fn convert_to_frontend_parameters(
        &self,
        header: ControllerSupportArgHeader,
        enable_text: bool,
        identification_colors: Vec<IdentificationColor>,
        text: Vec<ExplainText>,
    ) -> ControllerParameters {
        let style_set = NpadStyleSet::from_bits_retain(self.controller_private_arg.style_set);

        ControllerParameters {
            min_players: header.player_count_min.max(1),
            max_players: header.player_count_max,
            keep_controllers_connected: header.enable_take_over_connection,
            enable_single_mode: header.enable_single_mode,
            enable_border_color: header.enable_identification_color,
            border_colors: identification_colors,
            enable_explain_text: enable_text,
            explain_text: text,
            allow_pro_controller: style_set.contains(NpadStyleSet::FULLKEY),
            allow_handheld: style_set.contains(NpadStyleSet::HANDHELD),
            allow_dual_joycons: style_set.contains(NpadStyleSet::JOY_DUAL),
            allow_left_joycon: style_set.contains(NpadStyleSet::JOY_LEFT),
            allow_right_joycon: style_set.contains(NpadStyleSet::JOY_RIGHT),
            allow_gamecube_controller: false,
        }
    }

    fn configuration_complete(
        system: SystemRef,
        applet: &Weak<Mutex<Applet>>,
        broker: &Arc<AppletDataBroker>,
        completion: &Arc<ControllerCompletion>,
        is_single_mode: bool,
        is_success: bool,
    ) {
        let hid_core = system.get().hid_core();
        let hid_core = hid_core.lock();
        let result_info = ControllerSupportResultInfo {
            player_count: if is_single_mode {
                1
            } else {
                hid_core.get_player_count()
            },
            _padding: [0; 3],
            selected_id: hid_core.get_first_npad_id() as u32,
            result: if is_success {
                ControllerSupportResult::Success as u32
            } else {
                ControllerSupportResult::Cancel as u32
            },
        };
        drop(hid_core);

        log::debug!(
            "Result Info: player_count={}, selected_id={}, result={}",
            result_info.player_count,
            result_info.selected_id,
            result_info.result
        );

        let out_data = Self::struct_to_vec(&result_info);
        *completion.out_data.lock().unwrap() = out_data.clone();
        broker.get_out_data().push(out_data);
        completion.complete.store(true, Ordering::Release);

        // A default frontend may invoke the callback inline while the accessor
        // still owns the Applet mutex. In that case the accessor observes
        // `is_complete()` after Execute returns and performs Exit. A graphical
        // frontend invokes it later and must perform upstream's Exit here.
        if !completion.executing.load(Ordering::Acquire) {
            super::applets::exit(system, applet);
        }
    }
}

impl FrontendApplet for Controller {
    fn initialize(&mut self) {
        let common_data = self
            .broker
            .get_in_data()
            .pop()
            .expect("Controller::Initialize missing common arguments");
        let common_args = Self::copy_from_prefix::<CommonArguments>(&common_data)
            .expect("Controller common arguments are too small");

        log::info!("Initializing Controller Applet");
        log::debug!(
            "Initializing Applet with common_args: arg_version={:?}, lib_version={}, \
             play_startup_sound={}, size={:?}, system_tick={}, theme_color={:?}",
            common_args.arguments_version,
            common_args.library_version,
            common_args.play_startup_sound,
            common_args.size,
            common_args.system_tick,
            common_args.theme_color
        );
        self.controller_applet_version = common_args.library_version;

        let private_data = self
            .broker
            .get_in_data()
            .pop()
            .expect("Controller::Initialize missing private arguments");
        assert_eq!(
            private_data.len(),
            std::mem::size_of::<ControllerSupportArgPrivate>()
        );
        self.controller_private_arg = Self::copy_from_prefix(&private_data).unwrap();
        assert_eq!(
            self.controller_private_arg.arg_private_size as usize,
            std::mem::size_of::<ControllerSupportArgPrivate>(),
            "Unknown ControllerSupportArgPrivate revision={} with size={}",
            self.controller_applet_version,
            self.controller_private_arg.arg_private_size
        );

        if self.controller_private_arg.mode >= ControllerSupportMode::MaxControllerSupportMode as u8
        {
            self.controller_private_arg.mode = match self.controller_private_arg.arg_size as usize {
                size if size == std::mem::size_of::<ControllerSupportArgOld>()
                    || size == std::mem::size_of::<ControllerSupportArgNew>() =>
                {
                    ControllerSupportMode::ShowControllerSupport as u8
                }
                size if size == std::mem::size_of::<ControllerUpdateFirmwareArg>() => {
                    ControllerSupportMode::ShowControllerFirmwareUpdate as u8
                }
                size if size == std::mem::size_of::<ControllerKeyRemappingArg>() => {
                    ControllerSupportMode::ShowControllerKeyRemappingForSystem as u8
                }
                size => {
                    log::error!(
                        "Unknown ControllerPrivateArg mode={} with arg_size={}",
                        self.controller_private_arg.mode,
                        size
                    );
                    ControllerSupportMode::ShowControllerSupport as u8
                }
            };
        }

        if self.controller_private_arg.caller
            >= ControllerSupportCaller::MaxControllerSupportCaller as u8
        {
            self.controller_private_arg.caller = if self.controller_private_arg.flag_1
                && matches!(
                    self.support_mode(),
                    ControllerSupportMode::ShowControllerFirmwareUpdate
                        | ControllerSupportMode::ShowControllerKeyRemappingForSystem
                ) {
                ControllerSupportCaller::System as u8
            } else {
                ControllerSupportCaller::Application as u8
            };
        }

        match self.support_mode() {
            ControllerSupportMode::ShowControllerSupport
            | ControllerSupportMode::ShowControllerStrapGuide => {
                let user_data = self
                    .broker
                    .get_in_data()
                    .pop()
                    .expect("Controller::Initialize missing user arguments");
                match self.controller_applet_version {
                    version
                        if version == ControllerAppletVersion::Version3 as u32
                            || version == ControllerAppletVersion::Version4 as u32
                            || version == ControllerAppletVersion::Version5 as u32 =>
                    {
                        assert_eq!(
                            user_data.len(),
                            std::mem::size_of::<ControllerSupportArgOld>()
                        );
                        self.controller_user_arg_old = Self::copy_from_prefix(&user_data).unwrap();
                    }
                    version
                        if version == ControllerAppletVersion::Version7 as u32
                            || version == ControllerAppletVersion::Version8 as u32 =>
                    {
                        assert_eq!(
                            user_data.len(),
                            std::mem::size_of::<ControllerSupportArgNew>()
                        );
                        self.controller_user_arg_new = Self::copy_from_prefix(&user_data).unwrap();
                    }
                    version => {
                        log::error!(
                            "Unknown ControllerSupportArg revision={} with size={}",
                            version,
                            self.controller_private_arg.arg_size
                        );
                        assert!(user_data.len() >= std::mem::size_of::<ControllerSupportArgNew>());
                        self.controller_user_arg_new = Self::copy_from_prefix(&user_data).unwrap();
                    }
                }
            }
            ControllerSupportMode::ShowControllerFirmwareUpdate => {
                let data = self
                    .broker
                    .get_in_data()
                    .pop()
                    .expect("Controller::Initialize missing firmware update arguments");
                assert_eq!(
                    data.len(),
                    std::mem::size_of::<ControllerUpdateFirmwareArg>()
                );
                self.controller_update_arg = Self::copy_from_prefix(&data).unwrap();
            }
            ControllerSupportMode::ShowControllerKeyRemappingForSystem => {
                let data = self
                    .broker
                    .get_in_data()
                    .pop()
                    .expect("Controller::Initialize missing key remapping arguments");
                assert_eq!(data.len(), std::mem::size_of::<ControllerKeyRemappingArg>());
                self.controller_key_remapping_arg = Self::copy_from_prefix(&data).unwrap();
            }
            ControllerSupportMode::MaxControllerSupportMode => unreachable!(),
        }

        self.initialized = true;
    }

    fn get_status(&self) -> ResultCode {
        self.status
    }

    fn execute_interactive(&mut self) {
        panic!("Attempted to call interactive execution on non-interactive applet");
    }

    fn execute(&mut self) {
        self.completion.executing.store(true, Ordering::Release);
        match self.support_mode() {
            ControllerSupportMode::ShowControllerSupport => {
                let parameters = match self.controller_applet_version {
                    version
                        if version == ControllerAppletVersion::Version3 as u32
                            || version == ControllerAppletVersion::Version4 as u32
                            || version == ControllerAppletVersion::Version5 as u32 =>
                    {
                        self.convert_to_frontend_parameters(
                            self.controller_user_arg_old.header,
                            self.controller_user_arg_old.enable_explain_text,
                            self.controller_user_arg_old.identification_colors.to_vec(),
                            self.controller_user_arg_old.explain_text.to_vec(),
                        )
                    }
                    _ => self.convert_to_frontend_parameters(
                        self.controller_user_arg_new.header,
                        self.controller_user_arg_new.enable_explain_text,
                        self.controller_user_arg_new.identification_colors.to_vec(),
                        self.controller_user_arg_new.explain_text.to_vec(),
                    ),
                };
                self.is_single_mode = parameters.enable_single_mode;

                log::debug!("Controller Parameters: {:?}", parameters);

                let system = self.system;
                let applet = self.applet.clone();
                let broker = Arc::clone(&self.broker);
                let completion = Arc::clone(&self.completion);
                let is_single_mode = self.is_single_mode;
                let callback: ReconfigureCallback = Box::new(move |is_success| {
                    Self::configuration_complete(
                        system,
                        &applet,
                        &broker,
                        &completion,
                        is_single_mode,
                        is_success,
                    );
                });
                self.frontend.reconfigure_controllers(callback, &parameters);
            }
            ControllerSupportMode::ShowControllerStrapGuide
            | ControllerSupportMode::ShowControllerFirmwareUpdate
            | ControllerSupportMode::ShowControllerKeyRemappingForSystem => {
                log::error!(
                    "ControllerSupportMode={:?} is not implemented",
                    self.support_mode()
                );
                Self::configuration_complete(
                    self.system,
                    &self.applet,
                    &self.broker,
                    &self.completion,
                    self.is_single_mode,
                    true,
                );
            }
            ControllerSupportMode::MaxControllerSupportMode => unreachable!(),
        }
        self.completion.executing.store(false, Ordering::Release);
    }

    fn request_exit(&mut self) {
        self.completion.executing.store(true, Ordering::Release);
        self.frontend.close();
        self.completion.executing.store(false, Ordering::Release);
    }

    fn get_library_applet_mode(&self) -> LibraryAppletMode {
        self.applet_mode
    }

    fn is_initialized(&self) -> bool {
        self.initialized
    }

    fn is_complete(&self) -> bool {
        self.completion.complete.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use crate::core::System;
    use crate::hle::service::am::am_types::AppletId;
    use crate::hle::service::am::applet::Applet;
    use crate::hle::service::os::process::Process;

    use super::*;

    struct DeferredControllerApplet {
        callback: Arc<Mutex<Option<ReconfigureCallback>>>,
    }

    impl crate::frontend::applets::applet::Applet for DeferredControllerApplet {
        fn close(&self) {
            let callback = self.callback.lock().unwrap().take();
            if let Some(callback) = callback { callback(true); }
        }
    }

    impl ControllerApplet for DeferredControllerApplet {
        fn reconfigure_controllers(
            &self,
            callback: ReconfigureCallback,
            _parameters: &ControllerParameters,
        ) {
            *self.callback.lock().unwrap() = Some(callback);
        }
    }

    fn bytes_of<T>(value: &T) -> Vec<u8> {
        unsafe {
            std::slice::from_raw_parts((value as *const T).cast::<u8>(), std::mem::size_of::<T>())
                .to_vec()
        }
    }

    #[test]
    fn controller_applet_reconfigures_and_returns_success() {
        let system = System::new();
        let system_ref = SystemRef::from_ref(&system);
        let broker = Arc::new(AppletDataBroker::new());

        let mut common = CommonArguments::default();
        common.library_version = ControllerAppletVersion::Version8 as u32;
        let private = ControllerSupportArgPrivate {
            arg_private_size: std::mem::size_of::<ControllerSupportArgPrivate>() as u32,
            arg_size: std::mem::size_of::<ControllerSupportArgNew>() as u32,
            mode: ControllerSupportMode::ShowControllerSupport as u8,
            caller: ControllerSupportCaller::Application as u8,
            style_set: NpadStyleSet::FULLKEY.bits(),
            ..ControllerSupportArgPrivate::default()
        };
        let mut user = ControllerSupportArgNew::default();
        user.header.player_count_min = 1;
        user.header.player_count_max = 4;

        broker.get_in_data().push(bytes_of(&common));
        broker.get_in_data().push(bytes_of(&private));
        broker.get_in_data().push(bytes_of(&user));

        let mut applet = system
            .frontend_applet_holder()
            .get_applet(
                system_ref,
                Weak::new(),
                Arc::clone(&broker),
                AppletId::Controller,
                LibraryAppletMode::AllForeground,
            )
            .expect("Controller frontend must be registered");

        applet.initialize();
        applet.execute();

        assert!(applet.is_initialized());
        assert_eq!(applet.get_status(), RESULT_SUCCESS);
        let output = broker.get_out_data().pop().unwrap();
        assert_eq!(
            output.len(),
            std::mem::size_of::<ControllerSupportResultInfo>()
        );
        let result = Controller::copy_from_prefix::<ControllerSupportResultInfo>(&output).unwrap();
        assert_eq!(result.player_count, 1);
        assert_eq!(result.selected_id, 0);
        assert_eq!(result.result, ControllerSupportResult::Success as u32);
    }

    #[test]
    fn controller_applet_completes_after_deferred_frontend_callback() {
        for close_during_request in [false, true] {
        let system = System::new();
        let system_ref = SystemRef::from_ref(&system);
        let applet = Arc::new(Mutex::new(Applet::new(system_ref, Process::new(), false)));
        let parent = Arc::new(Mutex::new(Applet::new(system_ref, Process::new(), true)));
        applet.lock().unwrap().caller_applet = Arc::downgrade(&parent);
        parent.lock().unwrap().child_applets.push(Arc::clone(&applet));
        let broker = Arc::new(AppletDataBroker::new());
        let callback = Arc::new(Mutex::new(None));
        let frontend: Arc<dyn ControllerApplet> = Arc::new(DeferredControllerApplet {
            callback: Arc::clone(&callback),
        });

        let mut common = CommonArguments::default();
        common.library_version = ControllerAppletVersion::Version8 as u32;
        let private = ControllerSupportArgPrivate {
            arg_private_size: std::mem::size_of::<ControllerSupportArgPrivate>() as u32,
            arg_size: std::mem::size_of::<ControllerSupportArgNew>() as u32,
            mode: ControllerSupportMode::ShowControllerSupport as u8,
            caller: ControllerSupportCaller::Application as u8,
            style_set: NpadStyleSet::FULLKEY.bits(),
            ..ControllerSupportArgPrivate::default()
        };
        let mut user = ControllerSupportArgNew::default();
        user.header.player_count_min = 1;
        user.header.player_count_max = 4;

        broker.get_in_data().push(bytes_of(&common));
        broker.get_in_data().push(bytes_of(&private));
        broker.get_in_data().push(bytes_of(&user));

        let mut controller = Controller::new(
            system_ref,
            Arc::downgrade(&applet),
            Arc::clone(&broker),
            LibraryAppletMode::AllForeground,
            frontend,
        );
        controller.initialize();
        controller.execute();

        assert!(!controller.is_complete());
        assert!(!applet.lock().unwrap().is_completed);

        if close_during_request {
            {
                let guard = applet.lock().unwrap();
                controller.request_exit();
                assert!(!guard.is_completed);
            }
            super::super::applets::exit(system_ref, &Arc::downgrade(&applet));
        } else {
            callback.lock().unwrap().take().unwrap()(true);
        }

        assert!(controller.is_complete());
        assert!(applet.lock().unwrap().is_completed);
        assert!(parent.lock().unwrap().child_applets.is_empty());
        assert!(broker.get_out_data().pop().is_ok());
        }
    }
}
