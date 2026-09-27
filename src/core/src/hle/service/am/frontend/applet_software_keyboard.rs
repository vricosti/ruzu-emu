// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/frontend/applet_software_keyboard.h
//! Port of zuyu/src/core/hle/service/am/frontend/applet_software_keyboard.cpp

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use crate::core::SystemRef;
use crate::frontend::applets::software_keyboard::{
    InlineAppearParameters, InlineTextParameters, KeyboardInitializeParameters,
    SoftwareKeyboardApplet, SubmitInlineCallback, SubmitNormalCallback,
};
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::am::am_types::{CommonArguments, LibraryAppletMode};
use crate::hle::service::am::applet::Applet;
use crate::hle::service::am::applet_data_broker::AppletDataBroker;

use super::applet_software_keyboard_types::*;
use super::applets::FrontendApplet;

const DEFAULT_MAX_TEXT_LENGTH: u32 = 500;
const REPLY_BASE_SIZE: usize = std::mem::size_of::<u32>() * 2;
const REPLY_UTF8_SIZE: usize = 0x7D4;
const REPLY_UTF16_SIZE: usize = 0x3EC;

#[derive(Debug)]
enum FrontendSubmission {
    Normal(SwkbdResult, String, bool),
    Inline(SwkbdReplyType, String, i32),
}

fn read_prefix<T: Copy>(data: &[u8]) -> Option<T> {
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

fn bytes_of<T>(value: &T) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts((value as *const T).cast::<u8>(), std::mem::size_of::<T>())
    }
}

fn utf16_fixed(data: &[u16]) -> String {
    let end = data
        .iter()
        .position(|&value| value == 0)
        .unwrap_or(data.len());
    String::from_utf16_lossy(&data[..end])
}

fn utf16_units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn version_is_old(version: u32) -> bool {
    matches!(
        version,
        x if x == SwkbdAppletVersion::Version5 as u32
            || x == SwkbdAppletVersion::Version65542 as u32
    )
}

fn version_is_old2(version: u32) -> bool {
    matches!(
        version,
        x if x == SwkbdAppletVersion::Version196615 as u32
            || x == SwkbdAppletVersion::Version262152 as u32
            || x == SwkbdAppletVersion::Version327689 as u32
    )
}

pub struct SoftwareKeyboard {
    // Flattened counterpart of `FrontendApplet::system`. Eden's
    // SoftwareKeyboard does not access it directly after base construction,
    // but every frontend applet retains the base owner.
    #[allow(dead_code)]
    system: SystemRef,
    applet: Weak<Mutex<Applet>>,
    broker: Arc<AppletDataBroker>,
    applet_mode: LibraryAppletMode,
    frontend: Arc<dyn SoftwareKeyboardApplet>,
    initialized: bool,
    swkbd_applet_version: u32,
    swkbd_config_common: SwkbdConfigCommon,
    swkbd_config_old: SwkbdConfigOld,
    swkbd_config_old2: SwkbdConfigOld2,
    swkbd_config_new: SwkbdConfigNew,
    initial_text: Vec<u16>,
    swkbd_state: SwkbdState,
    swkbd_initialize_arg: SwkbdInitializeArg,
    swkbd_calc_arg_common: SwkbdCalcArgCommon,
    swkbd_calc_arg_old: SwkbdCalcArgOld,
    swkbd_calc_arg_new: SwkbdCalcArgNew,
    use_changed_string_v2: bool,
    use_moved_cursor_v2: bool,
    inline_use_utf8: bool,
    current_cursor_position: i32,
    current_text: Vec<u16>,
    is_background: bool,
    complete: bool,
    status: ResultCode,
    frontend_submissions: Arc<Mutex<VecDeque<FrontendSubmission>>>,
    frontend_executing: Arc<AtomicBool>,
    normal_keyboard_shown: bool,
}

impl SoftwareKeyboard {
    pub fn new(
        system: SystemRef,
        applet: Weak<Mutex<Applet>>,
        broker: Arc<AppletDataBroker>,
        applet_mode: LibraryAppletMode,
        frontend: Arc<dyn SoftwareKeyboardApplet>,
    ) -> Self {
        Self {
            system,
            applet,
            broker,
            applet_mode,
            frontend,
            initialized: false,
            swkbd_applet_version: 0,
            swkbd_config_common: SwkbdConfigCommon::default(),
            swkbd_config_old: SwkbdConfigOld::default(),
            swkbd_config_old2: SwkbdConfigOld2::default(),
            swkbd_config_new: SwkbdConfigNew::default(),
            initial_text: Vec::new(),
            swkbd_state: SwkbdState::NotInitialized,
            swkbd_initialize_arg: SwkbdInitializeArg::default(),
            swkbd_calc_arg_common: SwkbdCalcArgCommon::default(),
            swkbd_calc_arg_old: SwkbdCalcArgOld::default(),
            swkbd_calc_arg_new: SwkbdCalcArgNew::default(),
            use_changed_string_v2: false,
            use_moved_cursor_v2: false,
            inline_use_utf8: false,
            current_cursor_position: 0,
            current_text: Vec::new(),
            is_background: false,
            complete: false,
            status: RESULT_SUCCESS,
            frontend_submissions: Arc::new(Mutex::new(VecDeque::new())),
            frontend_executing: Arc::new(AtomicBool::new(false)),
            normal_keyboard_shown: false,
        }
    }

    /// Queue a frontend callback and, when it arrived asynchronously, resume
    /// the owning frontend applet immediately. Upstream callbacks invoke
    /// `SubmitTextNormal` / `SubmitTextInline` directly; the queue is the Rust
    /// adaptation needed to avoid borrowing the applet across a GUI callback.
    fn enqueue_frontend_submission(
        system: SystemRef,
        applet: &Weak<Mutex<Applet>>,
        queue: &Arc<Mutex<VecDeque<FrontendSubmission>>>,
        executing: &Arc<AtomicBool>,
        submission: FrontendSubmission,
    ) {
        queue.lock().unwrap().push_back(submission);

        // A synchronous callback is consumed before the current frontend call
        // returns. Trying to lock the Applet here would deadlock because the
        // accessor already owns its mutex.
        if executing.load(Ordering::Acquire) {
            return;
        }

        let Some(owner) = applet.upgrade() else {
            return;
        };
        let mut guard = owner.lock().unwrap();
        let complete = if let Some(frontend) = guard.frontend.as_mut() {
            frontend.execute();
            frontend.is_complete()
        } else {
            false
        };
        drop(guard);
        if complete {
            super::applets::exit(system, applet);
        }
    }

    fn initialize_foreground(&mut self) {
        log::info!("Initializing Normal Software Keyboard Applet");
        self.is_background = false;

        let config_data = self
            .broker
            .get_in_data()
            .pop()
            .expect("SoftwareKeyboard missing foreground config");
        assert!(config_data.len() >= std::mem::size_of::<SwkbdConfigCommon>());
        self.swkbd_config_common = read_prefix(&config_data).unwrap();
        let extension = &config_data[std::mem::size_of::<SwkbdConfigCommon>()..];
        if version_is_old(self.swkbd_applet_version) {
            assert_eq!(extension.len(), std::mem::size_of::<SwkbdConfigOld>());
            self.swkbd_config_old = read_prefix(extension).unwrap();
        } else if version_is_old2(self.swkbd_applet_version) {
            assert_eq!(extension.len(), std::mem::size_of::<SwkbdConfigOld2>());
            self.swkbd_config_old2 = read_prefix(extension).unwrap();
        } else {
            if !matches!(
                self.swkbd_applet_version,
                x if x == SwkbdAppletVersion::Version393227 as u32
                    || x == SwkbdAppletVersion::Version524301 as u32
            ) {
                log::error!(
                    "Unknown SwkbdConfig revision={} with size={}",
                    self.swkbd_applet_version,
                    config_data.len()
                );
            }
            assert!(extension.len() >= std::mem::size_of::<SwkbdConfigNew>());
            self.swkbd_config_new = read_prefix(extension).unwrap();
        }

        let work_buffer = self
            .broker
            .get_in_data()
            .pop()
            .expect("SoftwareKeyboard missing work buffer");
        if self.swkbd_config_common.initial_string_length != 0 {
            let offset = self.swkbd_config_common.initial_string_offset as usize;
            let byte_len = self.swkbd_config_common.initial_string_length as usize * 2;
            assert!(offset + byte_len <= work_buffer.len());
            self.initial_text = work_buffer[offset..offset + byte_len]
                .chunks_exact(2)
                .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
                .take_while(|&unit| unit != 0)
                .collect();
        }
        self.initialize_frontend_normal_keyboard();
    }

    fn initialize_partial_foreground(&mut self, mode: LibraryAppletMode) {
        log::info!("Initializing Inline Software Keyboard Applet");
        self.is_background = true;
        let data = self
            .broker
            .get_in_data()
            .pop()
            .expect("SoftwareKeyboard missing inline initialize argument");
        assert_eq!(data.len(), std::mem::size_of::<SwkbdInitializeArg>());
        self.swkbd_initialize_arg = read_prefix(&data).unwrap();
        if self.swkbd_initialize_arg.library_applet_mode_flag {
            assert_eq!(mode, LibraryAppletMode::PartialForeground);
        } else {
            assert_eq!(mode, LibraryAppletMode::PartialForegroundIndirectDisplay);
        }
    }

    fn process_text_check(&mut self) {
        let data = self
            .broker
            .get_interactive_in_data()
            .pop()
            .expect("SoftwareKeyboard missing text-check data");
        assert_eq!(data.len(), std::mem::size_of::<SwkbdTextCheck>());
        let text_check: SwkbdTextCheck = read_prefix(&data).unwrap();
        let result = SwkbdTextCheckResult::from_raw(text_check.text_check_result);
        let message = if matches!(
            result,
            SwkbdTextCheckResult::Failure | SwkbdTextCheckResult::Confirm
        ) {
            if self.swkbd_config_common.use_utf8 {
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        text_check.text_check_message.as_ptr().cast::<u8>(),
                        text_check.text_check_message.len() * 2,
                    )
                };
                let end = bytes
                    .iter()
                    .position(|&byte| byte == 0)
                    .unwrap_or(bytes.len());
                String::from_utf8_lossy(&bytes[..end]).into_owned()
            } else {
                utf16_fixed(&text_check.text_check_message)
            }
        } else {
            String::new()
        };

        match result {
            SwkbdTextCheckResult::Success => {
                self.submit_normal_output_and_exit(SwkbdResult::Ok, self.current_text.clone())
            }
            SwkbdTextCheckResult::Failure | SwkbdTextCheckResult::Confirm => {
                self.show_text_check_dialog(result, message)
            }
            SwkbdTextCheckResult::Silent => {}
        }
    }

    fn process_inline_keyboard_request(&mut self) {
        let data = self
            .broker
            .get_interactive_in_data()
            .pop()
            .expect("SoftwareKeyboard missing inline request");
        assert!(data.len() >= 4);
        let command =
            SwkbdRequestCommand::from_raw(u32::from_le_bytes(data[..4].try_into().unwrap()));
        match command {
            Some(SwkbdRequestCommand::Finalize) => self.request_finalize(&data),
            Some(SwkbdRequestCommand::SetUserWordInfo) => self.request_set_user_word_info(&data),
            Some(SwkbdRequestCommand::SetCustomizeDic) => self.request_set_customize_dic(&data),
            Some(SwkbdRequestCommand::Calc) => self.request_calc(&data),
            Some(SwkbdRequestCommand::SetCustomizedDictionaries) => {
                self.request_set_customized_dictionaries(&data)
            }
            Some(SwkbdRequestCommand::UnsetCustomizedDictionaries) => {
                self.request_unset_customized_dictionaries(&data)
            }
            Some(SwkbdRequestCommand::SetChangedStringV2Flag) => {
                self.request_set_changed_string_v2_flag(&data)
            }
            Some(SwkbdRequestCommand::SetMovedCursorV2Flag) => {
                self.request_set_moved_cursor_v2_flag(&data)
            }
            None => log::error!(
                "Unknown SwkbdRequestCommand=0x{:X}",
                u32::from_le_bytes(data[..4].try_into().unwrap())
            ),
        }
    }

    fn submit_text_normal(&mut self, result: SwkbdResult, text: String, confirmed: bool) {
        if self.complete {
            return;
        }
        let text = utf16_units(&text);
        if self.swkbd_config_common.use_text_check && result == SwkbdResult::Ok {
            if confirmed {
                self.submit_normal_output_and_exit(result, text);
            } else {
                self.submit_for_text_check(text);
            }
        } else {
            self.submit_normal_output_and_exit(result, text);
        }
    }

    fn submit_text_inline(&mut self, mut reply_type: SwkbdReplyType, text: String, cursor: i32) {
        if self.complete {
            return;
        }
        self.current_text = utf16_units(&text);
        self.current_cursor_position = cursor;
        if self.inline_use_utf8 {
            reply_type = match reply_type {
                SwkbdReplyType::ChangedString => SwkbdReplyType::ChangedStringUtf8,
                SwkbdReplyType::MovedCursor => SwkbdReplyType::MovedCursorUtf8,
                SwkbdReplyType::DecidedEnter => SwkbdReplyType::DecidedEnterUtf8,
                other => other,
            };
        }
        if self.use_changed_string_v2 {
            reply_type = match reply_type {
                SwkbdReplyType::ChangedString => SwkbdReplyType::ChangedStringV2,
                SwkbdReplyType::ChangedStringUtf8 => SwkbdReplyType::ChangedStringUtf8V2,
                other => other,
            };
        }
        if self.use_moved_cursor_v2 {
            reply_type = match reply_type {
                SwkbdReplyType::MovedCursor => SwkbdReplyType::MovedCursorV2,
                SwkbdReplyType::MovedCursorUtf8 => SwkbdReplyType::MovedCursorUtf8V2,
                other => other,
            };
        }
        self.send_reply(reply_type);
    }

    fn finish_frontend_execution(&mut self) {
        loop {
            let submissions = {
                let mut queue = self.frontend_submissions.lock().unwrap();
                if queue.is_empty() {
                    // Keep this transition under the queue lock: a callback
                    // either observes `executing` and leaves work for this
                    // loop, or observes false and resumes the applet itself.
                    self.frontend_executing.store(false, Ordering::Release);
                    return;
                }
                queue.drain(..).collect::<Vec<_>>()
            };
            for submission in submissions {
                match submission {
                    FrontendSubmission::Normal(result, text, confirmed) => {
                        self.submit_text_normal(result, text, confirmed)
                    }
                    FrontendSubmission::Inline(reply, text, cursor) => {
                        self.submit_text_inline(reply, text, cursor)
                    }
                }
            }
        }
    }

    fn submit_normal_output_and_exit(&mut self, result: SwkbdResult, text: Vec<u16>) {
        let mut output = vec![0; std::mem::size_of::<u32>() + STRING_BUFFER_SIZE];
        output[..4].copy_from_slice(&(result as u32).to_le_bytes());
        if self.swkbd_config_common.use_utf8 {
            let utf8 = String::from_utf16_lossy(&text).into_bytes();
            let count = utf8.len().min(STRING_BUFFER_SIZE);
            output[4..4 + count].copy_from_slice(&utf8[..count]);
        } else {
            let count = text.len().min(STRING_BUFFER_SIZE / 2);
            for (index, unit) in text[..count].iter().enumerate() {
                output[4 + index * 2..6 + index * 2].copy_from_slice(&unit.to_le_bytes());
            }
        }
        self.broker.get_out_data().push(output);
        self.exit_keyboard();
    }

    fn submit_for_text_check(&mut self, text: Vec<u16>) {
        self.current_text = text;
        let mut output = vec![0; 8 + STRING_BUFFER_SIZE];
        if self.swkbd_config_common.use_utf8 {
            let utf8 = String::from_utf16_lossy(&self.current_text).into_bytes();
            output[..8].copy_from_slice(&((utf8.len() + 1) as u64).to_le_bytes());
            let count = utf8.len().min(STRING_BUFFER_SIZE);
            output[8..8 + count].copy_from_slice(&utf8[..count]);
        } else {
            output[..8]
                .copy_from_slice(&(((self.current_text.len() + 1) * 2) as u64).to_le_bytes());
            let count = self.current_text.len().min(STRING_BUFFER_SIZE / 2);
            for (index, unit) in self.current_text[..count].iter().enumerate() {
                output[8 + index * 2..10 + index * 2].copy_from_slice(&unit.to_le_bytes());
            }
        }
        self.broker.get_interactive_out_data().push(output);
    }

    fn send_reply(&mut self, reply_type: SwkbdReplyType) {
        match reply_type {
            SwkbdReplyType::FinishedInitialize => self.reply_finished_initialize(),
            SwkbdReplyType::Default => self.reply_default(),
            SwkbdReplyType::ChangedString => self.reply_changed_string(),
            SwkbdReplyType::MovedCursor => self.reply_moved_cursor(),
            SwkbdReplyType::MovedTab => self.reply_moved_tab(),
            SwkbdReplyType::DecidedEnter => self.reply_decided_enter(),
            SwkbdReplyType::DecidedCancel => self.reply_decided_cancel(),
            SwkbdReplyType::ChangedStringUtf8 => self.reply_changed_string_utf8(),
            SwkbdReplyType::MovedCursorUtf8 => self.reply_moved_cursor_utf8(),
            SwkbdReplyType::DecidedEnterUtf8 => self.reply_decided_enter_utf8(),
            SwkbdReplyType::UnsetCustomizeDic => self.reply_unset_customize_dic(),
            SwkbdReplyType::ReleasedUserWordInfo => self.reply_released_user_word_info(),
            SwkbdReplyType::UnsetCustomizedDictionaries => {
                self.reply_unset_customized_dictionaries()
            }
            SwkbdReplyType::ChangedStringV2 => self.reply_changed_string_v2(),
            SwkbdReplyType::MovedCursorV2 => self.reply_moved_cursor_v2(),
            SwkbdReplyType::ChangedStringUtf8V2 => self.reply_changed_string_utf8_v2(),
            SwkbdReplyType::MovedCursorUtf8V2 => self.reply_moved_cursor_utf8_v2(),
        }
    }

    fn change_state(&mut self, state: SwkbdState) {
        self.swkbd_state = state;
        self.reply_default();
    }

    fn initialize_frontend_normal_keyboard(&mut self) {
        let max =
            if (1..=DEFAULT_MAX_TEXT_LENGTH).contains(&self.swkbd_config_common.max_text_length) {
                self.swkbd_config_common.max_text_length
            } else {
                DEFAULT_MAX_TEXT_LENGTH
            };
        let min = if self.swkbd_config_common.min_text_length <= max {
            self.swkbd_config_common.min_text_length
        } else {
            0
        };
        let cursor = if self.swkbd_config_common.initial_cursor_position
            == SwkbdInitialCursorPosition::End as u32
        {
            self.initial_text.len() as i32
        } else {
            0
        };
        let requested_draw = SwkbdTextDrawType::from_raw(self.swkbd_config_common.text_draw_type);
        let draw = match requested_draw {
            SwkbdTextDrawType::Line => {
                if max <= 32 {
                    SwkbdTextDrawType::Line
                } else {
                    SwkbdTextDrawType::Box
                }
            }
            other => other,
        };
        let parameters = KeyboardInitializeParameters {
            ok_text: utf16_fixed(&self.swkbd_config_common.ok_text),
            header_text: utf16_fixed(&self.swkbd_config_common.header_text),
            sub_text: utf16_fixed(&self.swkbd_config_common.sub_text),
            guide_text: utf16_fixed(&self.swkbd_config_common.guide_text),
            initial_text: String::from_utf16_lossy(&self.initial_text),
            left_optional_symbol_key: self.swkbd_config_common.left_optional_symbol_key,
            right_optional_symbol_key: self.swkbd_config_common.right_optional_symbol_key,
            max_text_length: max,
            min_text_length: min,
            initial_cursor_position: cursor,
            swkbd_type: SwkbdType::from_raw(self.swkbd_config_common.swkbd_type),
            password_mode: SwkbdPasswordMode::from_raw(self.swkbd_config_common.password_mode),
            text_draw_type: draw,
            key_disable_flags: self.swkbd_config_common.key_disable_flags,
            use_blur_background: self.swkbd_config_common.use_blur_background,
            enable_backspace_button: true,
            enable_return_button: draw == SwkbdTextDrawType::Box
                && self.swkbd_config_common.enable_return_button,
            disable_cancel_button: self.swkbd_applet_version
                >= SwkbdAppletVersion::Version393227 as u32
                && self.swkbd_config_new.disable_cancel_button,
        };
        let system = self.system;
        let normal_applet = self.applet.clone();
        let inline_applet = self.applet.clone();
        let normal_queue = Arc::clone(&self.frontend_submissions);
        let inline_queue = Arc::clone(&self.frontend_submissions);
        let normal_executing = Arc::clone(&self.frontend_executing);
        let inline_executing = Arc::clone(&self.frontend_executing);
        let normal: SubmitNormalCallback = Box::new(move |result, text, confirmed| {
            Self::enqueue_frontend_submission(
                system,
                &normal_applet,
                &normal_queue,
                &normal_executing,
                FrontendSubmission::Normal(result, text, confirmed),
            );
        });
        let inline: SubmitInlineCallback = Box::new(move |reply, text, cursor| {
            Self::enqueue_frontend_submission(
                system,
                &inline_applet,
                &inline_queue,
                &inline_executing,
                FrontendSubmission::Inline(reply, text, cursor),
            );
        });
        self.frontend
            .initialize_keyboard(false, parameters, normal, inline);
    }

    fn initialize_frontend_inline_keyboard(&mut self, parameters: KeyboardInitializeParameters) {
        let system = self.system;
        let normal_applet = self.applet.clone();
        let inline_applet = self.applet.clone();
        let normal_queue = Arc::clone(&self.frontend_submissions);
        let inline_queue = Arc::clone(&self.frontend_submissions);
        let normal_executing = Arc::clone(&self.frontend_executing);
        let inline_executing = Arc::clone(&self.frontend_executing);
        self.frontend.initialize_keyboard(
            true,
            parameters,
            Box::new(move |result, text, confirmed| {
                Self::enqueue_frontend_submission(
                    system,
                    &normal_applet,
                    &normal_queue,
                    &normal_executing,
                    FrontendSubmission::Normal(result, text, confirmed),
                );
            }),
            Box::new(move |reply, text, cursor| {
                Self::enqueue_frontend_submission(
                    system,
                    &inline_applet,
                    &inline_queue,
                    &inline_executing,
                    FrontendSubmission::Inline(reply, text, cursor),
                );
            }),
        );
    }

    fn inline_parameters_old(&self) -> KeyboardInitializeParameters {
        let appear = &self.swkbd_calc_arg_old.appear_arg;
        self.make_inline_parameters(
            appear.swkbd_type,
            &appear.ok_text,
            appear.left_optional_symbol_key,
            appear.right_optional_symbol_key,
            appear.max_text_length,
            appear.min_text_length,
            appear.key_disable_flags,
            self.swkbd_calc_arg_old.enable_backspace_button,
            appear.enable_return_button,
            appear.disable_cancel_button,
        )
    }

    fn inline_parameters_new(&self) -> KeyboardInitializeParameters {
        let appear = &self.swkbd_calc_arg_new.appear_arg;
        self.make_inline_parameters(
            appear.swkbd_type,
            &appear.ok_text,
            appear.left_optional_symbol_key,
            appear.right_optional_symbol_key,
            appear.max_text_length,
            appear.min_text_length,
            appear.key_disable_flags,
            self.swkbd_calc_arg_new.enable_backspace_button,
            appear.enable_return_button,
            appear.disable_cancel_button,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn make_inline_parameters(
        &self,
        swkbd_type: u32,
        ok_text: &[u16],
        left_symbol: u16,
        right_symbol: u16,
        requested_max: u32,
        requested_min: u32,
        key_disable_flags: SwkbdKeyDisableFlags,
        enable_backspace_button: bool,
        enable_return_button: bool,
        disable_cancel_button: bool,
    ) -> KeyboardInitializeParameters {
        let max = if (1..=DEFAULT_MAX_TEXT_LENGTH).contains(&requested_max) {
            requested_max
        } else {
            DEFAULT_MAX_TEXT_LENGTH
        };
        let min = if requested_min <= max {
            requested_min
        } else {
            0
        };
        KeyboardInitializeParameters {
            ok_text: utf16_fixed(ok_text),
            initial_text: String::from_utf16_lossy(&self.current_text),
            left_optional_symbol_key: left_symbol,
            right_optional_symbol_key: right_symbol,
            max_text_length: max,
            min_text_length: min,
            initial_cursor_position: self.current_cursor_position.max(0),
            swkbd_type: SwkbdType::from_raw(swkbd_type),
            password_mode: SwkbdPasswordMode::Disabled,
            text_draw_type: if max <= 32 {
                SwkbdTextDrawType::Line
            } else {
                SwkbdTextDrawType::Box
            },
            key_disable_flags,
            enable_backspace_button,
            enable_return_button,
            disable_cancel_button,
            ..KeyboardInitializeParameters::default()
        }
    }

    fn show_normal_keyboard(&mut self) {
        self.frontend.show_normal_keyboard();
    }

    fn show_text_check_dialog(&mut self, result: SwkbdTextCheckResult, message: String) {
        self.frontend.show_text_check_dialog(result, message);
    }

    fn show_inline_keyboard(&mut self, parameters: InlineAppearParameters) {
        self.frontend.show_inline_keyboard(parameters);
        self.change_state(SwkbdState::InitializedIsShown);
    }

    fn show_inline_keyboard_old(&mut self) {
        if self.swkbd_state != SwkbdState::InitializedIsHidden {
            return;
        }
        self.change_state(SwkbdState::InitializedIsAppearing);
        let appear = &self.swkbd_calc_arg_old.appear_arg;
        let max = if (1..=DEFAULT_MAX_TEXT_LENGTH).contains(&appear.max_text_length) {
            appear.max_text_length
        } else {
            DEFAULT_MAX_TEXT_LENGTH
        };
        let min = if appear.min_text_length <= max {
            appear.min_text_length
        } else {
            0
        };
        self.show_inline_keyboard(InlineAppearParameters {
            max_text_length: max,
            min_text_length: min,
            key_top_scale_x: self.swkbd_calc_arg_old.key_top_scale_x,
            key_top_scale_y: self.swkbd_calc_arg_old.key_top_scale_y,
            key_top_translate_x: self.swkbd_calc_arg_old.key_top_translate_x,
            key_top_translate_y: self.swkbd_calc_arg_old.key_top_translate_y,
            swkbd_type: SwkbdType::from_raw(appear.swkbd_type),
            key_disable_flags: appear.key_disable_flags,
            key_top_as_floating: self.swkbd_calc_arg_old.key_top_as_floating,
            enable_backspace_button: self.swkbd_calc_arg_old.enable_backspace_button,
            enable_return_button: appear.enable_return_button,
            disable_cancel_button: appear.disable_cancel_button,
        });
    }

    fn show_inline_keyboard_new(&mut self) {
        if self.swkbd_state != SwkbdState::InitializedIsHidden {
            return;
        }
        self.change_state(SwkbdState::InitializedIsAppearing);
        let appear = &self.swkbd_calc_arg_new.appear_arg;
        let max = if (1..=DEFAULT_MAX_TEXT_LENGTH).contains(&appear.max_text_length) {
            appear.max_text_length
        } else {
            DEFAULT_MAX_TEXT_LENGTH
        };
        let min = if appear.min_text_length <= max {
            appear.min_text_length
        } else {
            0
        };
        self.show_inline_keyboard(InlineAppearParameters {
            max_text_length: max,
            min_text_length: min,
            key_top_scale_x: self.swkbd_calc_arg_new.key_top_scale_x,
            key_top_scale_y: self.swkbd_calc_arg_new.key_top_scale_y,
            key_top_translate_x: self.swkbd_calc_arg_new.key_top_translate_x,
            key_top_translate_y: self.swkbd_calc_arg_new.key_top_translate_y,
            swkbd_type: SwkbdType::from_raw(appear.swkbd_type),
            key_disable_flags: appear.key_disable_flags,
            key_top_as_floating: self.swkbd_calc_arg_new.key_top_as_floating,
            enable_backspace_button: self.swkbd_calc_arg_new.enable_backspace_button,
            enable_return_button: appear.enable_return_button,
            disable_cancel_button: appear.disable_cancel_button,
        });
    }

    fn hide_inline_keyboard(&mut self) {
        if self.swkbd_state != SwkbdState::InitializedIsShown {
            return;
        }
        self.change_state(SwkbdState::InitializedIsDisappearing);
        self.frontend.hide_inline_keyboard();
        self.change_state(SwkbdState::InitializedIsHidden);
    }

    fn inline_text_changed(&mut self) {
        self.frontend.inline_text_changed(InlineTextParameters {
            input_text: String::from_utf16_lossy(&self.current_text),
            cursor_position: self.current_cursor_position,
        });
    }

    fn exit_keyboard(&mut self) {
        self.complete = true;
        self.status = RESULT_SUCCESS;
        self.frontend.exit_keyboard();
    }

    fn request_finalize(&mut self, _data: &[u8]) {
        self.change_state(SwkbdState::NotInitialized);
        self.exit_keyboard();
    }

    fn request_set_user_word_info(&mut self, _data: &[u8]) {
        log::warn!("SetUserWordInfo is not implemented");
        self.reply_released_user_word_info();
    }

    fn request_set_customize_dic(&mut self, _data: &[u8]) {
        log::warn!("SetCustomizeDic is not implemented");
    }

    fn request_calc(&mut self, data: &[u8]) {
        assert!(data.len() >= 4 + std::mem::size_of::<SwkbdCalcArgCommon>());
        self.swkbd_calc_arg_common = read_prefix(&data[4..]).unwrap();
        let extension = &data[4 + std::mem::size_of::<SwkbdCalcArgCommon>()..];
        match self.swkbd_calc_arg_common.calc_arg_size as usize {
            size if size
                == std::mem::size_of::<SwkbdCalcArgCommon>()
                    + std::mem::size_of::<SwkbdCalcArgOld>() =>
            {
                assert_eq!(extension.len(), std::mem::size_of::<SwkbdCalcArgOld>());
                self.swkbd_calc_arg_old = read_prefix(extension).unwrap();
                self.request_calc_old();
            }
            size if size
                == std::mem::size_of::<SwkbdCalcArgCommon>()
                    + std::mem::size_of::<SwkbdCalcArgNew>() =>
            {
                assert_eq!(extension.len(), std::mem::size_of::<SwkbdCalcArgNew>());
                self.swkbd_calc_arg_new = read_prefix(extension).unwrap();
                self.request_calc_new();
            }
            size => {
                log::error!("Unknown SwkbdCalcArg size={size}");
                assert!(extension.len() >= std::mem::size_of::<SwkbdCalcArgNew>());
                self.swkbd_calc_arg_new = read_prefix(extension).unwrap();
                self.request_calc_new();
            }
        }
    }

    fn request_calc_old(&mut self) {
        let flags = self.swkbd_calc_arg_common.flags;
        if flags.set_input_text() {
            self.current_text = utf16_units(&utf16_fixed(&self.swkbd_calc_arg_old.input_text));
        }
        if flags.set_cursor_position() {
            self.current_cursor_position = self.swkbd_calc_arg_old.cursor_position;
        }
        if flags.set_utf8_mode() {
            self.inline_use_utf8 = self.swkbd_calc_arg_old.utf8_mode;
        }
        if self.swkbd_state <= SwkbdState::InitializedIsHidden && flags.unset_customize_dic() {
            self.reply_unset_customize_dic();
        }
        if self.swkbd_state <= SwkbdState::InitializedIsHidden && flags.unset_user_word_info() {
            self.reply_released_user_word_info();
        }
        if self.swkbd_state == SwkbdState::NotInitialized && flags.set_initialize_arg() {
            let parameters = self.inline_parameters_old();
            self.initialize_frontend_inline_keyboard(parameters);
            self.change_state(SwkbdState::InitializedIsHidden);
            self.reply_finished_initialize();
        }
        if !flags.set_initialize_arg() && (flags.set_input_text() || flags.set_cursor_position()) {
            self.inline_text_changed();
        }
        if self.swkbd_state == SwkbdState::InitializedIsHidden && flags.appear() {
            self.show_inline_keyboard_old();
            return;
        }
        if self.swkbd_state == SwkbdState::InitializedIsShown && flags.disappear() {
            self.hide_inline_keyboard();
        }
    }

    fn request_calc_new(&mut self) {
        let flags = self.swkbd_calc_arg_common.flags;
        if flags.set_input_text() {
            self.current_text = utf16_units(&utf16_fixed(&self.swkbd_calc_arg_new.input_text));
        }
        if flags.set_cursor_position() {
            self.current_cursor_position = self.swkbd_calc_arg_new.cursor_position;
        }
        if flags.set_utf8_mode() {
            self.inline_use_utf8 = self.swkbd_calc_arg_new.utf8_mode;
        }
        if self.swkbd_state <= SwkbdState::InitializedIsHidden && flags.unset_customize_dic() {
            self.reply_unset_customize_dic();
        }
        if self.swkbd_state <= SwkbdState::InitializedIsHidden && flags.unset_user_word_info() {
            self.reply_released_user_word_info();
        }
        if self.swkbd_state == SwkbdState::NotInitialized && flags.set_initialize_arg() {
            let parameters = self.inline_parameters_new();
            self.initialize_frontend_inline_keyboard(parameters);
            self.change_state(SwkbdState::InitializedIsHidden);
            self.reply_finished_initialize();
        }
        if !flags.set_initialize_arg() && (flags.set_input_text() || flags.set_cursor_position()) {
            self.inline_text_changed();
        }
        if self.swkbd_state == SwkbdState::InitializedIsHidden && flags.appear() {
            self.show_inline_keyboard_new();
            return;
        }
        if self.swkbd_state == SwkbdState::InitializedIsShown && flags.disappear() {
            self.hide_inline_keyboard();
        }
    }

    fn request_set_customized_dictionaries(&mut self, _data: &[u8]) {
        log::warn!("SetCustomizedDictionaries is not implemented");
    }
    fn request_unset_customized_dictionaries(&mut self, _data: &[u8]) {
        log::warn!("(STUBBED) Processing Request: UnsetCustomizedDictionaries");
        self.reply_unset_customized_dictionaries();
    }
    fn request_set_changed_string_v2_flag(&mut self, data: &[u8]) {
        assert_eq!(data.len(), 5);
        self.use_changed_string_v2 = data[4] != 0;
    }
    fn request_set_moved_cursor_v2_flag(&mut self, data: &[u8]) {
        assert_eq!(data.len(), 5);
        self.use_moved_cursor_v2 = data[4] != 0;
    }

    fn reply_base(&self, reply_type: SwkbdReplyType, extra: usize) -> Vec<u8> {
        let mut reply = vec![0; REPLY_BASE_SIZE + extra];
        reply[..4].copy_from_slice(&(self.swkbd_state as u32).to_le_bytes());
        reply[4..8].copy_from_slice(&(reply_type as u32).to_le_bytes());
        reply
    }

    fn push_reply(&self, reply: Vec<u8>) {
        self.broker.get_interactive_out_data().push(reply);
    }
    fn copy_utf16_reply(&self, reply: &mut [u8]) {
        for (index, unit) in self
            .current_text
            .iter()
            .take(REPLY_UTF16_SIZE / 2)
            .enumerate()
        {
            reply[REPLY_BASE_SIZE + index * 2..REPLY_BASE_SIZE + index * 2 + 2]
                .copy_from_slice(&unit.to_le_bytes());
        }
    }
    fn copy_utf8_reply(&self, reply: &mut [u8]) {
        let text = String::from_utf16_lossy(&self.current_text).into_bytes();
        let count = text.len().min(REPLY_UTF8_SIZE);
        reply[REPLY_BASE_SIZE..REPLY_BASE_SIZE + count].copy_from_slice(&text[..count]);
    }
    fn append_struct<T>(&self, reply: &mut [u8], offset: usize, value: &T) {
        reply[offset..offset + std::mem::size_of::<T>()].copy_from_slice(bytes_of(value));
    }

    fn reply_finished_initialize(&self) {
        self.push_reply(self.reply_base(SwkbdReplyType::FinishedInitialize, 1));
    }
    fn reply_default(&self) {
        self.push_reply(self.reply_base(SwkbdReplyType::Default, 0));
    }

    fn reply_changed_string(&self) {
        let mut reply = self.reply_base(SwkbdReplyType::ChangedString, REPLY_UTF16_SIZE + 0x10);
        self.copy_utf16_reply(&mut reply);
        let arg = SwkbdChangedStringArg {
            text_length: self.current_text.len() as u32,
            dictionary_start_cursor_position: -1,
            dictionary_end_cursor_position: -1,
            cursor_position: self.current_cursor_position,
        };
        self.append_struct(&mut reply, REPLY_BASE_SIZE + REPLY_UTF16_SIZE, &arg);
        self.push_reply(reply);
    }

    fn reply_moved_cursor(&self) {
        self.reply_moved_cursor_kind(SwkbdReplyType::MovedCursor, false);
    }
    fn reply_moved_tab(&self) {
        self.reply_moved_cursor_kind(SwkbdReplyType::MovedTab, false);
    }
    fn reply_moved_cursor_kind(&self, kind: SwkbdReplyType, v2: bool) {
        let mut reply = self.reply_base(kind, REPLY_UTF16_SIZE + 8 + usize::from(v2));
        self.copy_utf16_reply(&mut reply);
        let arg = SwkbdMovedCursorArg {
            text_length: self.current_text.len() as u32,
            cursor_position: self.current_cursor_position,
        };
        self.append_struct(&mut reply, REPLY_BASE_SIZE + REPLY_UTF16_SIZE, &arg);
        self.push_reply(reply);
    }

    fn reply_decided_enter(&mut self) {
        let mut reply = self.reply_base(SwkbdReplyType::DecidedEnter, REPLY_UTF16_SIZE + 4);
        self.copy_utf16_reply(&mut reply);
        let arg = SwkbdDecidedEnterArg {
            text_length: self.current_text.len() as u32,
        };
        self.append_struct(&mut reply, REPLY_BASE_SIZE + REPLY_UTF16_SIZE, &arg);
        self.push_reply(reply);
        self.hide_inline_keyboard();
    }

    fn reply_decided_cancel(&mut self) {
        self.push_reply(self.reply_base(SwkbdReplyType::DecidedCancel, 0));
        self.hide_inline_keyboard();
    }
    fn reply_changed_string_utf8(&self) {
        self.reply_changed_string_utf8_kind(SwkbdReplyType::ChangedStringUtf8, false);
    }
    fn reply_moved_cursor_utf8(&self) {
        self.reply_moved_cursor_utf8_kind(SwkbdReplyType::MovedCursorUtf8, false);
    }

    fn reply_decided_enter_utf8(&mut self) {
        let mut reply = self.reply_base(SwkbdReplyType::DecidedEnterUtf8, REPLY_UTF8_SIZE + 4);
        self.copy_utf8_reply(&mut reply);
        let arg = SwkbdDecidedEnterArg {
            text_length: self.current_text.len() as u32,
        };
        self.append_struct(&mut reply, REPLY_BASE_SIZE + REPLY_UTF8_SIZE, &arg);
        self.push_reply(reply);
        self.hide_inline_keyboard();
    }

    fn reply_unset_customize_dic(&self) {
        self.push_reply(self.reply_base(SwkbdReplyType::UnsetCustomizeDic, 0));
    }
    fn reply_released_user_word_info(&self) {
        self.push_reply(self.reply_base(SwkbdReplyType::ReleasedUserWordInfo, 0));
    }
    fn reply_unset_customized_dictionaries(&self) {
        self.push_reply(self.reply_base(SwkbdReplyType::UnsetCustomizedDictionaries, 0));
    }

    fn reply_changed_string_v2(&self) {
        let mut reply =
            self.reply_base(SwkbdReplyType::ChangedStringV2, REPLY_UTF16_SIZE + 0x10 + 1);
        self.copy_utf16_reply(&mut reply);
        let arg = SwkbdChangedStringArg {
            text_length: self.current_text.len() as u32,
            dictionary_start_cursor_position: -1,
            dictionary_end_cursor_position: -1,
            cursor_position: self.current_cursor_position,
        };
        self.append_struct(&mut reply, REPLY_BASE_SIZE + REPLY_UTF16_SIZE, &arg);
        self.push_reply(reply);
    }

    fn reply_moved_cursor_v2(&self) {
        self.reply_moved_cursor_kind(SwkbdReplyType::MovedCursorV2, true);
    }
    fn reply_changed_string_utf8_v2(&self) {
        self.reply_changed_string_utf8_kind(SwkbdReplyType::ChangedStringUtf8V2, true);
    }
    fn reply_moved_cursor_utf8_v2(&self) {
        self.reply_moved_cursor_utf8_kind(SwkbdReplyType::MovedCursorUtf8V2, true);
    }

    fn reply_changed_string_utf8_kind(&self, kind: SwkbdReplyType, v2: bool) {
        let mut reply = self.reply_base(kind, REPLY_UTF8_SIZE + 0x10 + usize::from(v2));
        self.copy_utf8_reply(&mut reply);
        let arg = SwkbdChangedStringArg {
            text_length: self.current_text.len() as u32,
            dictionary_start_cursor_position: -1,
            dictionary_end_cursor_position: -1,
            cursor_position: self.current_cursor_position,
        };
        self.append_struct(&mut reply, REPLY_BASE_SIZE + REPLY_UTF8_SIZE, &arg);
        self.push_reply(reply);
    }

    fn reply_moved_cursor_utf8_kind(&self, kind: SwkbdReplyType, v2: bool) {
        let mut reply = self.reply_base(kind, REPLY_UTF8_SIZE + 8 + usize::from(v2));
        self.copy_utf8_reply(&mut reply);
        let arg = SwkbdMovedCursorArg {
            text_length: self.current_text.len() as u32,
            cursor_position: self.current_cursor_position,
        };
        self.append_struct(&mut reply, REPLY_BASE_SIZE + REPLY_UTF8_SIZE, &arg);
        self.push_reply(reply);
    }
}

impl FrontendApplet for SoftwareKeyboard {
    fn initialize(&mut self) {
        self.frontend_executing.store(true, Ordering::Release);
        let common_data = self
            .broker
            .get_in_data()
            .pop()
            .expect("SoftwareKeyboard missing common arguments");
        let common: CommonArguments =
            read_prefix(&common_data).expect("SoftwareKeyboard common arguments are too small");
        self.swkbd_applet_version = common.library_version;
        match self.applet_mode {
            LibraryAppletMode::AllForeground => self.initialize_foreground(),
            LibraryAppletMode::PartialForeground
            | LibraryAppletMode::PartialForegroundIndirectDisplay => {
                self.initialize_partial_foreground(self.applet_mode)
            }
            mode => panic!("Invalid LibraryAppletMode={mode:?}"),
        }
        self.initialized = true;
        self.finish_frontend_execution();
    }

    fn get_status(&self) -> ResultCode {
        self.status
    }

    fn execute_interactive(&mut self) {
        self.frontend_executing.store(true, Ordering::Release);
        if self.complete {
            self.finish_frontend_execution();
            return;
        }
        if self.is_background {
            self.process_inline_keyboard_request();
        } else {
            self.process_text_check();
        }
        self.finish_frontend_execution();
    }

    fn execute(&mut self) {
        self.frontend_executing.store(true, Ordering::Release);
        if self.complete || self.is_background {
            self.finish_frontend_execution();
            return;
        }
        if !self.normal_keyboard_shown {
            self.normal_keyboard_shown = true;
            self.show_normal_keyboard();
        }
        self.finish_frontend_execution();
    }

    fn request_exit(&mut self) {
        self.frontend_executing.store(true, Ordering::Release);
        self.frontend.close();
        self.finish_frontend_execution();
    }
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
    use crate::core::System;
    use crate::frontend::applets::applet::Applet as FrontendUiApplet;
    use crate::frontend::applets::software_keyboard::DefaultSoftwareKeyboardApplet;
    use crate::hle::service::am::am_types::{AppletId, CommonArgumentVersion};
    use crate::hle::service::os::process::Process;

    use super::*;

    fn owned_bytes<T>(value: &T) -> Vec<u8> {
        bytes_of(value).to_vec()
    }

    struct DeferredSoftwareKeyboardApplet {
        submit_normal: Mutex<Option<SubmitNormalCallback>>,
    }

    impl FrontendUiApplet for DeferredSoftwareKeyboardApplet {
        fn close(&self) {}
    }

    impl SoftwareKeyboardApplet for DeferredSoftwareKeyboardApplet {
        fn initialize_keyboard(
            &self,
            _is_inline: bool,
            _initialize_parameters: KeyboardInitializeParameters,
            submit_normal_callback: SubmitNormalCallback,
            _submit_inline_callback: SubmitInlineCallback,
        ) {
            *self.submit_normal.lock().unwrap() = Some(submit_normal_callback);
        }

        fn show_normal_keyboard(&self) {}

        fn show_text_check_dialog(
            &self,
            _text_check_result: SwkbdTextCheckResult,
            _text_check_message: String,
        ) {
        }

        fn show_inline_keyboard(&self, _appear_parameters: InlineAppearParameters) {}

        fn hide_inline_keyboard(&self) {}

        fn inline_text_changed(&self, _text_parameters: InlineTextParameters) {}

        fn exit_keyboard(&self) {}
    }

    #[test]
    fn deferred_normal_submission_completes_and_signals_owning_applet() {
        let system = System::new();
        let system_ref = SystemRef::from_ref(&system);
        let owner = Arc::new(Mutex::new(Applet::new(system_ref, Process::new(), false)));
        let parent = Arc::new(Mutex::new(Applet::new(system_ref, Process::new(), true)));
        owner.lock().unwrap().caller_applet = Arc::downgrade(&parent);
        parent.lock().unwrap().child_applets.push(Arc::clone(&owner));
        let broker = Arc::new(AppletDataBroker::new());
        let frontend_impl = Arc::new(DeferredSoftwareKeyboardApplet {
            submit_normal: Mutex::new(None),
        });
        let frontend: Arc<dyn SoftwareKeyboardApplet> = frontend_impl.clone();

        let common = CommonArguments {
            arguments_version: CommonArgumentVersion::Version3,
            library_version: SwkbdAppletVersion::Version524301 as u32,
            ..CommonArguments::default()
        };
        let mut config = owned_bytes(&SwkbdConfigCommon::default());
        config.extend_from_slice(bytes_of(&SwkbdConfigNew::default()));
        broker.get_in_data().push(owned_bytes(&common));
        broker.get_in_data().push(config);
        broker.get_in_data().push(Vec::new());

        let keyboard = SoftwareKeyboard::new(
            system_ref,
            Arc::downgrade(&owner),
            Arc::clone(&broker),
            LibraryAppletMode::AllForeground,
            frontend,
        );
        {
            let mut owner = owner.lock().unwrap();
            owner.applet_id = AppletId::SoftwareKeyboard;
            owner.frontend = Some(Box::new(keyboard));
            let frontend = owner.frontend.as_mut().unwrap();
            frontend.initialize();
            frontend.execute();
            assert!(!frontend.is_complete());
        }

        frontend_impl
            .submit_normal
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()(SwkbdResult::Ok, "ABC".to_string(), false);

        let owner = owner.lock().unwrap();
        assert!(owner.is_completed);
        assert!(owner.frontend.as_ref().unwrap().is_complete());
        drop(owner);
        assert!(parent.lock().unwrap().child_applets.is_empty());

        let output = broker.get_out_data().pop().unwrap();
        assert_eq!(
            u32::from_le_bytes(output[..4].try_into().unwrap()),
            SwkbdResult::Ok as u32
        );
        assert_eq!(
            u16::from_le_bytes(output[4..6].try_into().unwrap()),
            'A' as u16
        );
        assert_eq!(
            u16::from_le_bytes(output[6..8].try_into().unwrap()),
            'B' as u16
        );
        assert_eq!(
            u16::from_le_bytes(output[8..10].try_into().unwrap()),
            'C' as u16
        );
    }

    #[test]
    fn inline_calc_initializes_replies_and_only_finalize_completes() {
        let system = System::new();
        let broker = Arc::new(AppletDataBroker::new());
        let frontend: Arc<dyn SoftwareKeyboardApplet> =
            Arc::new(DefaultSoftwareKeyboardApplet::new());
        let mut applet = SoftwareKeyboard::new(
            SystemRef::from_ref(&system),
            Weak::new(),
            Arc::clone(&broker),
            LibraryAppletMode::PartialForegroundIndirectDisplay,
            frontend,
        );

        let common = CommonArguments {
            arguments_version: CommonArgumentVersion::Version3,
            library_version: SwkbdAppletVersion::Version524301 as u32,
            ..CommonArguments::default()
        };
        let initialize = SwkbdInitializeArg {
            library_applet_mode_flag: false,
            is_above_hos_500: true,
            ..SwkbdInitializeArg::default()
        };
        broker.get_in_data().push(owned_bytes(&common));
        broker.get_in_data().push(owned_bytes(&initialize));
        applet.initialize();
        applet.execute();
        assert!(!applet.is_complete());

        let mut calc_common = SwkbdCalcArgCommon::default();
        calc_common.calc_arg_size = (std::mem::size_of::<SwkbdCalcArgCommon>()
            + std::mem::size_of::<SwkbdCalcArgNew>()) as u16;
        calc_common.flags.raw = (1 << 0) | (1 << 2);
        let mut calc = SwkbdCalcArgNew::default();
        calc.appear_arg.swkbd_type = SwkbdType::Qwerty as u32;
        calc.appear_arg.max_text_length = 32;
        calc.appear_arg.enable_return_button = true;
        let mut request = (SwkbdRequestCommand::Calc as u32).to_le_bytes().to_vec();
        request.extend_from_slice(bytes_of(&calc_common));
        request.extend_from_slice(bytes_of(&calc));
        broker.get_interactive_in_data().push(request);
        applet.execute_interactive();

        let first = broker.get_interactive_out_data().pop().unwrap();
        assert_eq!(
            u32::from_le_bytes(first[..4].try_into().unwrap()),
            SwkbdState::InitializedIsHidden as u32
        );
        assert!(!applet.is_complete());

        broker.get_interactive_in_data().push(
            (SwkbdRequestCommand::Finalize as u32)
                .to_le_bytes()
                .to_vec(),
        );
        applet.execute_interactive();
        assert!(applet.is_complete());
    }
}
