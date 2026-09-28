// SPDX-FileCopyrightText: Copyright 2019 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `core/hle/service/am/frontend/applet_general.{h,cpp}`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use crate::core::SystemRef;
use crate::frontend::applets::general::{
    FinishedCallback, ParentalControlsApplet, PhotoViewerApplet, VerifyPinCallback,
};
use crate::hle::result::{ErrorModule, ResultCode, RESULT_SUCCESS};
use crate::hle::service::am::am_types::{AppletId, CommonArguments, LibraryAppletMode};
use crate::hle::service::am::applet::Applet;
use crate::hle::service::am::applet_data_broker::{AppletDataBroker, AppletStorageChannel};

use super::applets::FrontendApplet;

const ERROR_INVALID_PIN: ResultCode = ResultCode::from_module_description(ErrorModule::PCTL, 221);

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AuthAppletType(pub u32);

impl AuthAppletType {
    pub const SHOW_PARENTAL_AUTHENTICATION: Self = Self(0);
    pub const REGISTER_PARENTAL_PASSCODE: Self = Self(1);
    pub const CHANGE_PARENTAL_PASSCODE: Self = Self(2);
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AuthAppletArg {
    _padding_0: [u8; 4],
    auth_type: AuthAppletType,
    arg0: u8,
    arg1: u8,
    arg2: u8,
    _padding_1: [u8; 1],
}
const _: () = assert!(std::mem::size_of::<AuthAppletArg>() == 0xC);

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PhotoViewerAppletMode(pub u8);

impl PhotoViewerAppletMode {
    pub const CURRENT_APP: Self = Self(0);
    pub const ALL_APPS: Self = Self(1);
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

fn pop_common_arguments(broker: &AppletDataBroker, applet_name: &str) {
    let common_data = broker
        .get_in_data()
        .pop()
        .unwrap_or_else(|_| panic!("{applet_name} missing common arguments"));
    copy_from_prefix::<CommonArguments>(&common_data)
        .unwrap_or_else(|| panic!("{applet_name} common arguments are too small"));
}


struct AuthCallbackState {
    complete: AtomicBool,
    successful: AtomicBool,
    frontend_executing: AtomicBool,
}

pub struct Auth {
    #[allow(dead_code)]
    system: SystemRef,
    applet: Weak<Mutex<Applet>>,
    broker: Arc<AppletDataBroker>,
    applet_mode: LibraryAppletMode,
    frontend: Arc<dyn ParentalControlsApplet>,
    initialized: bool,
    callback_state: Arc<AuthCallbackState>,
    auth_type: AuthAppletType,
    arg0: u8,
    arg1: u8,
    arg2: u8,
}

impl Auth {
    pub fn new(
        system: SystemRef,
        applet: Weak<Mutex<Applet>>,
        broker: Arc<AppletDataBroker>,
        applet_mode: LibraryAppletMode,
        frontend: Arc<dyn ParentalControlsApplet>,
    ) -> Self {
        Self {
            system,
            applet,
            broker,
            applet_mode,
            frontend,
            initialized: false,
            callback_state: Arc::new(AuthCallbackState {
                complete: AtomicBool::new(false),
                successful: AtomicBool::new(false),
                frontend_executing: AtomicBool::new(false),
            }),
            auth_type: AuthAppletType::SHOW_PARENTAL_AUTHENTICATION,
            arg0: 0,
            arg1: 0,
            arg2: 0,
        }
    }

    fn status(callback_state: &AuthCallbackState) -> ResultCode {
        if callback_state.successful.load(Ordering::Acquire) {
            RESULT_SUCCESS
        } else {
            ERROR_INVALID_PIN
        }
    }

    fn auth_finished(
        system: SystemRef,
        is_successful: bool,
        applet: &Weak<Mutex<Applet>>,
        broker: &AppletDataBroker,
        callback_state: &AuthCallbackState,
    ) {
        callback_state
            .successful
            .store(is_successful, Ordering::Release);
        broker.get_out_data().push(
            Self::status(callback_state)
                .get_inner_value()
                .to_le_bytes()
                .to_vec(),
        );
        callback_state.complete.store(true, Ordering::Release);
        if !callback_state.frontend_executing.load(Ordering::Acquire) {
            super::applets::exit(system, applet);
        }
    }

    fn verify_callback(&self) -> VerifyPinCallback {
        let system = self.system;
        let applet = self.applet.clone();
        let broker = Arc::clone(&self.broker);
        let callback_state = Arc::clone(&self.callback_state);
        Box::new(move |successful| {
            Self::auth_finished(system, successful, &applet, &broker, &callback_state)
        })
    }

    fn successful_callback(&self) -> FinishedCallback {
        let system = self.system;
        let applet = self.applet.clone();
        let broker = Arc::clone(&self.broker);
        let callback_state = Arc::clone(&self.callback_state);
        Box::new(move || Self::auth_finished(system, true, &applet, &broker, &callback_state))
    }

    fn log_unimplemented(&self) {
        log::error!(
            "Unimplemented Auth applet type for type={:08X}, arg0={:02X}, arg1={:02X}, arg2={:02X}",
            self.auth_type.0,
            self.arg0,
            self.arg1,
            self.arg2
        );
    }
}

impl FrontendApplet for Auth {
    fn initialize(&mut self) {
        pop_common_arguments(&self.broker, "Auth::Initialize");
        self.callback_state.complete.store(false, Ordering::Release);

        let data = self
            .broker
            .get_in_data()
            .pop()
            .expect("Auth::Initialize missing arguments");
        let args = copy_from_prefix::<AuthAppletArg>(&data)
            .expect("Auth arguments must be at least 0xC bytes");
        self.auth_type = args.auth_type;
        self.arg0 = args.arg0;
        self.arg1 = args.arg1;
        self.arg2 = args.arg2;
        self.initialized = true;
    }

    fn get_status(&self) -> ResultCode {
        Self::status(&self.callback_state)
    }

    fn execute_interactive(&mut self) {
        panic!("Unexpected interactive applet data.");
    }

    fn execute(&mut self) {
        if self.callback_state.complete.load(Ordering::Acquire) {
            return;
        }

        self.callback_state
            .frontend_executing
            .store(true, Ordering::Release);
        match self.auth_type {
            AuthAppletType::SHOW_PARENTAL_AUTHENTICATION => {
                if self.arg0 == 1 && self.arg1 == 0 && self.arg2 == 1 {
                    self.frontend
                        .verify_pin_for_settings(self.verify_callback());
                } else if self.arg1 == 0 && self.arg2 == 0 {
                    self.frontend
                        .verify_pin(self.verify_callback(), self.arg0 != 0);
                } else {
                    self.log_unimplemented();
                }
            }
            AuthAppletType::REGISTER_PARENTAL_PASSCODE => {
                if self.arg0 == 0 && self.arg1 == 0 && self.arg2 == 0 {
                    self.frontend.register_pin(self.successful_callback());
                } else {
                    self.log_unimplemented();
                }
            }
            AuthAppletType::CHANGE_PARENTAL_PASSCODE => {
                if self.arg0 == 0 && self.arg1 == 0 && self.arg2 == 0 {
                    self.frontend.change_pin(self.successful_callback());
                } else {
                    self.log_unimplemented();
                }
            }
            _ => self.log_unimplemented(),
        }
        self.callback_state
            .frontend_executing
            .store(false, Ordering::Release);
    }

    fn request_exit(&mut self) {
        self.callback_state.frontend_executing.store(true, Ordering::Release);
        self.frontend.close();
        self.callback_state.frontend_executing.store(false, Ordering::Release);
    }

    fn get_library_applet_mode(&self) -> LibraryAppletMode {
        self.applet_mode
    }

    fn is_initialized(&self) -> bool {
        self.initialized
    }

    fn is_complete(&self) -> bool {
        self.callback_state.complete.load(Ordering::Acquire)
    }
}

struct PhotoCallbackState {
    complete: AtomicBool,
    frontend_executing: AtomicBool,
}

pub struct PhotoViewer {
    system: SystemRef,
    applet: Weak<Mutex<Applet>>,
    broker: Arc<AppletDataBroker>,
    applet_mode: LibraryAppletMode,
    frontend: Arc<dyn PhotoViewerApplet>,
    initialized: bool,
    callback_state: Arc<PhotoCallbackState>,
    mode: PhotoViewerAppletMode,
}

impl PhotoViewer {
    pub fn new(
        system: SystemRef,
        applet: Weak<Mutex<Applet>>,
        broker: Arc<AppletDataBroker>,
        applet_mode: LibraryAppletMode,
        frontend: Arc<dyn PhotoViewerApplet>,
    ) -> Self {
        Self {
            system,
            applet,
            broker,
            applet_mode,
            frontend,
            initialized: false,
            callback_state: Arc::new(PhotoCallbackState {
                complete: AtomicBool::new(false),
                frontend_executing: AtomicBool::new(false),
            }),
            mode: PhotoViewerAppletMode::CURRENT_APP,
        }
    }

    fn view_finished(
        system: SystemRef,
        applet: &Weak<Mutex<Applet>>,
        broker: &AppletDataBroker,
        callback_state: &PhotoCallbackState,
    ) {
        broker.get_out_data().push(Vec::new());
        callback_state.complete.store(true, Ordering::Release);
        if !callback_state.frontend_executing.load(Ordering::Acquire) {
            super::applets::exit(system, applet);
        }
    }

    fn finished_callback(&self) -> FinishedCallback {
        let system = self.system;
        let applet = self.applet.clone();
        let broker = Arc::clone(&self.broker);
        let callback_state = Arc::clone(&self.callback_state);
        Box::new(move || Self::view_finished(system, &applet, &broker, &callback_state))
    }
}

impl FrontendApplet for PhotoViewer {
    fn initialize(&mut self) {
        pop_common_arguments(&self.broker, "PhotoViewer::Initialize");
        self.callback_state.complete.store(false, Ordering::Release);

        let data = self
            .broker
            .get_in_data()
            .pop()
            .expect("PhotoViewer::Initialize missing mode");
        assert!(!data.is_empty(), "PhotoViewer mode is empty");
        self.mode = PhotoViewerAppletMode(data[0]);
        self.initialized = true;
    }

    fn get_status(&self) -> ResultCode {
        RESULT_SUCCESS
    }

    fn execute_interactive(&mut self) {
        panic!("Unexpected interactive applet data.");
    }

    fn execute(&mut self) {
        if self.callback_state.complete.load(Ordering::Acquire) {
            return;
        }

        self.callback_state
            .frontend_executing
            .store(true, Ordering::Release);
        match self.mode {
            PhotoViewerAppletMode::CURRENT_APP => self.frontend.show_photos_for_application(
                self.system.get().runtime_program_id(),
                self.finished_callback(),
            ),
            PhotoViewerAppletMode::ALL_APPS => {
                self.frontend.show_all_photos(self.finished_callback())
            }
            mode => log::error!("Unimplemented PhotoViewer applet mode={:02X}!", mode.0),
        }
        self.callback_state
            .frontend_executing
            .store(false, Ordering::Release);
    }

    fn request_exit(&mut self) {
        self.callback_state.frontend_executing.store(true, Ordering::Release);
        self.frontend.close();
        self.callback_state.frontend_executing.store(false, Ordering::Release);
    }

    fn get_library_applet_mode(&self) -> LibraryAppletMode {
        self.applet_mode
    }

    fn is_initialized(&self) -> bool {
        self.initialized
    }

    fn is_complete(&self) -> bool {
        self.callback_state.complete.load(Ordering::Acquire)
    }
}

pub struct StubApplet {
    #[allow(dead_code)]
    system: SystemRef,
    #[allow(dead_code)]
    applet: Weak<Mutex<Applet>>,
    broker: Arc<AppletDataBroker>,
    applet_mode: LibraryAppletMode,
    #[allow(dead_code)]
    id: AppletId,
    initialized: bool,
    complete: bool,
}

impl StubApplet {
    pub fn new(
        system: SystemRef,
        applet: Weak<Mutex<Applet>>,
        broker: Arc<AppletDataBroker>,
        id: AppletId,
        applet_mode: LibraryAppletMode,
    ) -> Self {
        Self {
            system,
            applet,
            broker,
            applet_mode,
            id,
            initialized: false,
            complete: false,
        }
    }

    fn log_channel(channel: &AppletStorageChannel, prefix: &str, kind: &str) {
        while let Ok(data) = channel.pop() {
            log::info!(
                "called (STUBBED), during {} received {} data with size={:08X}, data={}",
                prefix,
                kind,
                data.len(),
                hex::encode_upper(data)
            );
        }
    }

    fn log_current_storage(&self, prefix: &str) {
        Self::log_channel(self.broker.get_in_data(), prefix, "normal");
        Self::log_channel(self.broker.get_interactive_in_data(), prefix, "interactive");
    }

    fn push_stub_outputs(&mut self) {
        self.broker.get_out_data().push(vec![0; 0x1000]);
        self.broker.get_interactive_out_data().push(vec![0; 0x1000]);
        self.complete = true;
    }
}

impl FrontendApplet for StubApplet {
    fn initialize(&mut self) {
        log::warn!("called (STUBBED)");
        pop_common_arguments(&self.broker, "StubApplet::Initialize");
        self.initialized = true;
        self.log_current_storage("Initialize");
    }

    fn get_status(&self) -> ResultCode {
        log::warn!("called (STUBBED)");
        RESULT_SUCCESS
    }

    fn execute_interactive(&mut self) {
        log::warn!("called (STUBBED)");
        self.log_current_storage("ExecuteInteractive");
        self.push_stub_outputs();
    }

    fn execute(&mut self) {
        log::warn!("called (STUBBED)");
        self.log_current_storage("Execute");
        self.push_stub_outputs();
    }

    fn request_exit(&mut self) {}

    fn get_library_applet_mode(&self) -> LibraryAppletMode {
        self.applet_mode
    }

    fn is_initialized(&self) -> bool {
        self.initialized
    }

    fn is_complete(&self) -> bool {
        self.complete
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::System;
    use crate::frontend::applets::applet::Applet as FrontendUiApplet;
    use crate::frontend::applets::general::{
        DefaultParentalControlsApplet, DefaultPhotoViewerApplet,
    };
    use crate::hle::service::os::process::Process;

    fn owner(system: SystemRef) -> Arc<Mutex<Applet>> {
        Arc::new(Mutex::new(Applet::new(system, Process::new(), false)))
    }

    struct RejectingParentalControls;

    impl FrontendUiApplet for RejectingParentalControls {
        fn close(&self) {}
    }

    impl ParentalControlsApplet for RejectingParentalControls {
        fn verify_pin(
            &self,
            finished: VerifyPinCallback,
            _suspend_future_verification_temporarily: bool,
        ) {
            finished(false);
        }

        fn verify_pin_for_settings(&self, finished: VerifyPinCallback) {
            finished(false);
        }

        fn register_pin(&self, finished: FinishedCallback) {
            finished();
        }

        fn change_pin(&self, finished: FinishedCallback) {
            finished();
        }
    }

    #[test]
    fn default_auth_returns_success_and_completes() {
        let system = System::new();
        let system_ref = SystemRef::from_ref(&system);
        let owner = owner(system_ref);
        let broker = Arc::new(AppletDataBroker::new());
        let input = AuthAppletArg::default();
        broker
            .get_in_data()
            .push(unsafe { bytes_of(&CommonArguments::default()) });
        broker.get_in_data().push(unsafe { bytes_of(&input) });

        let mut applet = Auth::new(
            system_ref,
            Arc::downgrade(&owner),
            Arc::clone(&broker),
            LibraryAppletMode::AllForeground,
            Arc::new(DefaultParentalControlsApplet),
        );
        applet.initialize();
        applet.execute();

        assert_eq!(applet.get_status(), RESULT_SUCCESS);
        assert!(applet.is_complete());
        assert_eq!(broker.get_out_data().pop().unwrap(), vec![0; 4]);
        assert!(!owner.lock().unwrap().is_completed);
    }

    #[test]
    fn rejected_auth_returns_invalid_pin() {
        let system = System::new();
        let system_ref = SystemRef::from_ref(&system);
        let owner = owner(system_ref);
        let broker = Arc::new(AppletDataBroker::new());
        broker
            .get_in_data()
            .push(unsafe { bytes_of(&CommonArguments::default()) });
        broker
            .get_in_data()
            .push(unsafe { bytes_of(&AuthAppletArg::default()) });

        let mut applet = Auth::new(
            system_ref,
            Arc::downgrade(&owner),
            Arc::clone(&broker),
            LibraryAppletMode::AllForeground,
            Arc::new(RejectingParentalControls),
        );
        applet.initialize();
        applet.execute();

        assert_eq!(applet.get_status(), ERROR_INVALID_PIN);
        assert_eq!(
            broker.get_out_data().pop().unwrap(),
            ERROR_INVALID_PIN.get_inner_value().to_le_bytes()
        );
    }

    #[test]
    fn default_photo_viewer_returns_empty_storage() {
        let system = System::new();
        let system_ref = SystemRef::from_ref(&system);
        let owner = owner(system_ref);
        let broker = Arc::new(AppletDataBroker::new());
        broker
            .get_in_data()
            .push(unsafe { bytes_of(&CommonArguments::default()) });
        broker
            .get_in_data()
            .push(vec![PhotoViewerAppletMode::ALL_APPS.0]);

        let mut applet = PhotoViewer::new(
            system_ref,
            Arc::downgrade(&owner),
            Arc::clone(&broker),
            LibraryAppletMode::AllForeground,
            Arc::new(DefaultPhotoViewerApplet),
        );
        applet.initialize();
        applet.execute();

        assert!(applet.is_complete());
        assert_eq!(broker.get_out_data().pop().unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn stub_drains_input_and_produces_both_outputs() {
        let system = System::new();
        let system_ref = SystemRef::from_ref(&system);
        let owner = owner(system_ref);
        let broker = Arc::new(AppletDataBroker::new());
        broker
            .get_in_data()
            .push(unsafe { bytes_of(&CommonArguments::default()) });
        broker.get_in_data().push(vec![1, 2, 3]);
        broker.get_interactive_in_data().push(vec![4, 5]);

        let mut applet = StubApplet::new(
            system_ref,
            Arc::downgrade(&owner),
            Arc::clone(&broker),
            AppletId::Settings,
            LibraryAppletMode::AllForeground,
        );
        applet.initialize();
        applet.execute();

        assert!(applet.is_complete());
        assert_eq!(broker.get_out_data().pop().unwrap(), vec![0; 0x1000]);
        assert_eq!(
            broker.get_interactive_out_data().pop().unwrap(),
            vec![0; 0x1000]
        );
        assert!(broker.get_in_data().pop().is_err());
        assert!(broker.get_interactive_in_data().pop().is_err());
    }

    unsafe fn bytes_of<T>(value: &T) -> Vec<u8> {
        unsafe {
            std::slice::from_raw_parts((value as *const T).cast::<u8>(), std::mem::size_of::<T>())
                .to_vec()
        }
    }
}
