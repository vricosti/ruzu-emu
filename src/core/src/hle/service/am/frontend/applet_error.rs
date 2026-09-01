// SPDX-FileCopyrightText: Copyright 2019 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `core/hle/service/am/frontend/applet_error.{h,cpp}`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use crate::core::SystemRef;
use crate::frontend::applets::error::{ErrorApplet, FinishedCallback};
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::am::am_types::{CommonArguments, LibraryAppletMode};
use crate::hle::service::am::applet::Applet;
use crate::hle::service::am::applet_data_broker::AppletDataBroker;

use super::applets::FrontendApplet;

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ErrorAppletMode(pub u8);

impl ErrorAppletMode {
    pub const SHOW_ERROR: Self = Self(0);
    pub const SHOW_SYSTEM_ERROR: Self = Self(1);
    pub const SHOW_APPLICATION_ERROR: Self = Self(2);
    pub const SHOW_EULA: Self = Self(3);
    pub const SHOW_ERROR_PCTL: Self = Self(4);
    pub const SHOW_ERROR_RECORD: Self = Self(5);
    pub const SHOW_UPDATE_EULA: Self = Self(8);
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ErrorCode {
    error_category: u32,
    error_number: u32,
}
const _: () = assert!(std::mem::size_of::<ErrorCode>() == 0x8);

impl ErrorCode {
    fn from_u64(error_code: u64) -> Self {
        Self {
            error_category: (error_code >> 32) as u32,
            error_number: error_code as u32,
        }
    }

    fn to_result(self) -> ResultCode {
        let module = self.error_category.wrapping_sub(2000) & 0x1ff;
        let description = self.error_number & 0x1fff;
        ResultCode::new(module | (description << 9))
    }
}

#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct ShowError {
    mode: u8,
    jump: u8,
    _padding_0: [u8; 4],
    use_64bit_error_code: u8,
    _padding_1: [u8; 1],
    error_code_64: u64,
    error_code_32: u32,
}
const _: () = assert!(std::mem::size_of::<ShowError>() == 0x14);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ShowErrorRecord {
    mode: u8,
    jump: u8,
    _padding: [u8; 6],
    error_code_64: u64,
    posix_time: u64,
}
const _: () = assert!(std::mem::size_of::<ShowErrorRecord>() == 0x18);

#[repr(C)]
#[derive(Clone, Copy)]
struct SystemErrorArg {
    mode: u8,
    jump: u8,
    _padding: [u8; 6],
    error_code_64: u64,
    language_code: [u8; 8],
    main_text: [u8; 0x800],
    detail_text: [u8; 0x800],
}
const _: () = assert!(std::mem::size_of::<SystemErrorArg>() == 0x1018);

#[repr(C)]
#[derive(Clone, Copy)]
struct ApplicationErrorArg {
    mode: u8,
    jump: u8,
    _padding: [u8; 6],
    error_code: u32,
    language_code: [u8; 8],
    main_text: [u8; 0x800],
    detail_text: [u8; 0x800],
}
const _: () = assert!(std::mem::size_of::<ApplicationErrorArg>() == 0x1014);

#[repr(C)]
union ErrorArguments {
    error: ShowError,
    error_record: ShowErrorRecord,
    system_error: SystemErrorArg,
    application_error: ApplicationErrorArg,
    raw: [u8; 0x1018],
}
const _: () = assert!(std::mem::size_of::<ErrorArguments>() == 0x1018);

impl Default for ErrorArguments {
    fn default() -> Self {
        Self { raw: [0; 0x1018] }
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

#[cfg(test)]
fn struct_to_vec<T>(value: &T) -> Vec<u8> {
    unsafe {
        std::slice::from_raw_parts((value as *const T).cast::<u8>(), std::mem::size_of::<T>())
            .to_vec()
    }
}

fn fixed_zero_terminated_string(data: &[u8]) -> String {
    let length = data
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(data.len());
    String::from_utf8_lossy(&data[..length]).into_owned()
}

fn decode_64_bit_error(error: u64) -> ResultCode {
    ErrorCode::from_u64(error).to_result()
}

struct CompletionState {
    complete: AtomicBool,
    frontend_executing: AtomicBool,
}

pub struct Error {
    system: SystemRef,
    applet: Weak<Mutex<Applet>>,
    broker: Arc<AppletDataBroker>,
    applet_mode: LibraryAppletMode,
    frontend: Arc<dyn ErrorApplet>,
    initialized: bool,
    error_code: ResultCode,
    mode: ErrorAppletMode,
    args: ErrorArguments,
    completion: Arc<CompletionState>,
}

impl Error {
    pub fn new(
        system: SystemRef,
        applet: Weak<Mutex<Applet>>,
        broker: Arc<AppletDataBroker>,
        applet_mode: LibraryAppletMode,
        frontend: Arc<dyn ErrorApplet>,
    ) -> Self {
        Self {
            system,
            applet,
            broker,
            applet_mode,
            frontend,
            initialized: false,
            error_code: RESULT_SUCCESS,
            mode: ErrorAppletMode::SHOW_ERROR,
            args: ErrorArguments::default(),
            completion: Arc::new(CompletionState {
                complete: AtomicBool::new(false),
                frontend_executing: AtomicBool::new(false),
            }),
        }
    }

    fn exit(applet: &Weak<Mutex<Applet>>) {
        let Some(applet) = applet.upgrade() else {
            return;
        };
        let mut applet = applet.lock().unwrap();
        applet.is_completed = true;
        applet.signal_state_changed_event_without_process();
    }

    fn display_completed(
        applet: &Weak<Mutex<Applet>>,
        broker: &AppletDataBroker,
        completion: &CompletionState,
    ) {
        completion.complete.store(true, Ordering::Release);
        broker.get_out_data().push(vec![0; 0x1000]);
        if !completion.frontend_executing.load(Ordering::Acquire) {
            Self::exit(applet);
        }
    }

    fn finished_callback(&self) -> FinishedCallback {
        let applet = self.applet.clone();
        let broker = Arc::clone(&self.broker);
        let completion = Arc::clone(&self.completion);
        Box::new(move || Self::display_completed(&applet, &broker, &completion))
    }

    fn finish_frontend_execution(&self) {
        self.completion
            .frontend_executing
            .store(false, Ordering::Release);
        if self.completion.complete.load(Ordering::Acquire) {
            Self::exit(&self.applet);
        }
    }
}

impl FrontendApplet for Error {
    fn initialize(&mut self) {
        let common_data = self
            .broker
            .get_in_data()
            .pop()
            .expect("Error::Initialize missing common arguments");
        copy_from_prefix::<CommonArguments>(&common_data)
            .expect("Error common arguments are too small");

        self.args = ErrorArguments::default();
        self.completion.complete.store(false, Ordering::Release);

        let data = self
            .broker
            .get_in_data()
            .pop()
            .expect("Error::Initialize missing error arguments");
        assert!(!data.is_empty(), "Error arguments are empty");
        self.mode = ErrorAppletMode(data[0]);

        match self.mode {
            ErrorAppletMode::SHOW_ERROR => {
                let args = copy_from_prefix::<ShowError>(&data)
                    .expect("ShowError arguments must be at least 0x14 bytes");
                self.error_code = if args.use_64bit_error_code != 0 {
                    decode_64_bit_error(args.error_code_64)
                } else {
                    ResultCode::new(args.error_code_32)
                };
                self.args.error = args;
            }
            ErrorAppletMode::SHOW_SYSTEM_ERROR => {
                let args = copy_from_prefix::<SystemErrorArg>(&data)
                    .expect("SystemError arguments must be at least 0x1018 bytes");
                self.error_code = decode_64_bit_error(args.error_code_64);
                self.args.system_error = args;
            }
            ErrorAppletMode::SHOW_APPLICATION_ERROR => {
                let args = copy_from_prefix::<ApplicationErrorArg>(&data)
                    .expect("ApplicationError arguments must be at least 0x1014 bytes");
                self.error_code = ResultCode::new(args.error_code);
                self.args.application_error = args;
            }
            ErrorAppletMode::SHOW_ERROR_PCTL | ErrorAppletMode::SHOW_ERROR_RECORD => {
                let args = copy_from_prefix::<ShowErrorRecord>(&data)
                    .expect("ShowErrorRecord arguments must be at least 0x18 bytes");
                self.error_code = decode_64_bit_error(args.error_code_64);
                self.args.error_record = args;
            }
            mode => log::error!("Unimplemented LibAppletError mode={:02X}!", mode.0),
        }

        self.initialized = true;
    }

    fn get_status(&self) -> ResultCode {
        RESULT_SUCCESS
    }

    fn execute_interactive(&mut self) {
        panic!("Unexpected interactive applet data!");
    }

    fn execute(&mut self) {
        if self.completion.complete.load(Ordering::Acquire) {
            return;
        }

        self.completion
            .frontend_executing
            .store(true, Ordering::Release);
        let title_id = self.system.get().runtime_program_id();
        let reporter = self.system.get_reporter();

        match self.mode {
            ErrorAppletMode::SHOW_ERROR => {
                reporter.save_error_report(title_id, self.error_code.get_inner_value(), None, None);
                self.frontend
                    .show_error(self.error_code, self.finished_callback());
            }
            ErrorAppletMode::SHOW_SYSTEM_ERROR | ErrorAppletMode::SHOW_APPLICATION_ERROR => {
                let (main_text, detail_text) = unsafe {
                    if self.mode == ErrorAppletMode::SHOW_SYSTEM_ERROR {
                        (
                            fixed_zero_terminated_string(&self.args.system_error.main_text),
                            fixed_zero_terminated_string(&self.args.system_error.detail_text),
                        )
                    } else {
                        (
                            fixed_zero_terminated_string(&self.args.application_error.main_text),
                            fixed_zero_terminated_string(&self.args.application_error.detail_text),
                        )
                    }
                };
                reporter.save_error_report(
                    title_id,
                    self.error_code.get_inner_value(),
                    Some(&main_text),
                    Some(&detail_text),
                );
                self.frontend.show_custom_error_text(
                    self.error_code,
                    main_text,
                    detail_text,
                    self.finished_callback(),
                );
            }
            ErrorAppletMode::SHOW_ERROR_PCTL | ErrorAppletMode::SHOW_ERROR_RECORD => {
                let posix_time = unsafe { self.args.error_record.posix_time };
                let timestamp = format!("{posix_time:016X}");
                reporter.save_error_report(
                    title_id,
                    self.error_code.get_inner_value(),
                    Some(&timestamp),
                    None,
                );
                self.frontend.show_error_with_timestamp(
                    self.error_code,
                    posix_time as i64,
                    self.finished_callback(),
                );
            }
            mode => {
                log::error!("Unimplemented LibAppletError mode={:02X}!", mode.0);
                Self::display_completed(&self.applet, &self.broker, &self.completion);
            }
        }

        self.finish_frontend_execution();
    }

    fn request_exit(&mut self) {
        self.frontend.close();
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
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::core::System;
    use crate::frontend::applets::applet::Applet as FrontendUiApplet;
    use crate::hle::service::os::process::Process;

    struct CompletingFrontend(AtomicU32);

    struct CustomTextFrontend(Mutex<Option<(u32, String, String)>>);

    impl FrontendUiApplet for CompletingFrontend {
        fn close(&self) {}
    }

    impl ErrorApplet for CompletingFrontend {
        fn show_error(&self, error: ResultCode, finished: FinishedCallback) {
            self.0.store(error.get_inner_value(), Ordering::Release);
            finished();
        }

        fn show_error_with_timestamp(
            &self,
            _error: ResultCode,
            _time_seconds: i64,
            finished: FinishedCallback,
        ) {
            finished();
        }

        fn show_custom_error_text(
            &self,
            _error: ResultCode,
            _dialog_text: String,
            _fullscreen_text: String,
            finished: FinishedCallback,
        ) {
            finished();
        }
    }

    impl FrontendUiApplet for CustomTextFrontend {
        fn close(&self) {}
    }

    impl ErrorApplet for CustomTextFrontend {
        fn show_error(&self, _error: ResultCode, finished: FinishedCallback) {
            finished();
        }

        fn show_error_with_timestamp(
            &self,
            _error: ResultCode,
            _time_seconds: i64,
            finished: FinishedCallback,
        ) {
            finished();
        }

        fn show_custom_error_text(
            &self,
            error: ResultCode,
            dialog_text: String,
            fullscreen_text: String,
            finished: FinishedCallback,
        ) {
            *self.0.lock().unwrap() = Some((error.get_inner_value(), dialog_text, fullscreen_text));
            finished();
        }
    }

    #[test]
    fn show_error_decodes_64_bit_code_and_completes() {
        let system = System::new();
        let system_ref = SystemRef::from_ref(&system);
        let owner = Arc::new(Mutex::new(Applet::new(system_ref, Process::new(), false)));
        let broker = Arc::new(AppletDataBroker::new());
        let frontend = Arc::new(CompletingFrontend(AtomicU32::new(0)));

        let common = CommonArguments::default();
        let input = ShowError {
            mode: ErrorAppletMode::SHOW_ERROR.0,
            use_64bit_error_code: 1,
            error_code_64: ((2000 + 128) << 32) | 42,
            ..ShowError::default()
        };
        broker.get_in_data().push(struct_to_vec(&common));
        broker.get_in_data().push(struct_to_vec(&input));

        let mut applet = Error::new(
            system_ref,
            Arc::downgrade(&owner),
            Arc::clone(&broker),
            LibraryAppletMode::AllForeground,
            Arc::clone(&frontend) as Arc<dyn ErrorApplet>,
        );
        applet.initialize();
        applet.execute();

        let expected = 128 | (42 << 9);
        assert_eq!(frontend.0.load(Ordering::Acquire), expected);
        assert!(applet.is_initialized());
        assert!(applet.is_complete());
        assert!(owner.lock().unwrap().is_completed);
        assert_eq!(broker.get_out_data().pop().unwrap(), vec![0; 0x1000]);
    }

    #[test]
    fn system_error_decodes_fixed_text_buffers() {
        let system = System::new();
        let system_ref = SystemRef::from_ref(&system);
        let owner = Arc::new(Mutex::new(Applet::new(system_ref, Process::new(), false)));
        let broker = Arc::new(AppletDataBroker::new());
        let frontend = Arc::new(CustomTextFrontend(Mutex::new(None)));

        let mut input: SystemErrorArg = unsafe { std::mem::zeroed() };
        input.mode = ErrorAppletMode::SHOW_SYSTEM_ERROR.0;
        input.error_code_64 = ((2000 + 128) << 32) | 7;
        input.main_text[..6].copy_from_slice(b"dialog");
        input.detail_text[..6].copy_from_slice(b"detail");
        broker
            .get_in_data()
            .push(struct_to_vec(&CommonArguments::default()));
        broker.get_in_data().push(struct_to_vec(&input));

        let mut applet = Error::new(
            system_ref,
            Arc::downgrade(&owner),
            Arc::clone(&broker),
            LibraryAppletMode::AllForeground,
            Arc::clone(&frontend) as Arc<dyn ErrorApplet>,
        );
        applet.initialize();
        applet.execute();

        let captured = frontend.0.lock().unwrap().clone().unwrap();
        assert_eq!(captured, (128 | (7 << 9), "dialog".into(), "detail".into()));
        assert!(applet.is_complete());
    }
}
