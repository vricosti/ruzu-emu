// SPDX-FileCopyrightText: Copyright 2019 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `core/frontend/applets/error.{h,cpp}`.
//! Error display applet interface.

use super::applet::Applet;
use crate::hle::result::ResultCode;

/// Callback type for when error display is finished.
///
/// Corresponds to upstream `ErrorApplet::FinishedCallback`.
pub type FinishedCallback = Box<dyn Fn() + Send + Sync>;

/// Error applet trait.
///
/// Corresponds to upstream `Core::Frontend::ErrorApplet`.
pub trait ErrorApplet: Applet {
    fn show_error(&self, error: ResultCode, finished: FinishedCallback);

    fn show_error_with_timestamp(
        &self,
        error: ResultCode,
        time_seconds: i64,
        finished: FinishedCallback,
    );

    fn show_custom_error_text(
        &self,
        error: ResultCode,
        dialog_text: String,
        fullscreen_text: String,
        finished: FinishedCallback,
    );
}

/// Default (stub) error applet implementation.
///
/// Corresponds to upstream `Core::Frontend::DefaultErrorApplet`.
pub struct DefaultErrorApplet;

impl Applet for DefaultErrorApplet {
    fn close(&self) {}
}

impl ErrorApplet for DefaultErrorApplet {
    fn show_error(&self, error: ResultCode, finished: FinishedCallback) {
        log::error!(
            "Application requested error display: {:04}-{:04} (raw={:08X})",
            error.get_module_raw(),
            error.get_description(),
            error.get_inner_value()
        );
        finished();
    }

    fn show_error_with_timestamp(&self, error: ResultCode, time: i64, finished: FinishedCallback) {
        log::error!(
            "Application requested error display: {:04X}-{:04X} (raw={:08X}) with timestamp={:016X}",
            error.get_module_raw(),
            error.get_description(),
            error.get_inner_value(),
            time
        );
        finished();
    }

    fn show_custom_error_text(
        &self,
        error: ResultCode,
        main_text: String,
        detail_text: String,
        finished: FinishedCallback,
    ) {
        log::error!(
            "Application requested custom error with error_code={:04X}-{:04X} (raw={:08X})",
            error.get_module_raw(),
            error.get_description(),
            error.get_inner_value()
        );
        log::error!("    Main Text: {}", main_text);
        log::error!("    Detail Text: {}", detail_text);
        finished();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;

    #[test]
    fn default_frontend_completes_after_logging() {
        let completion_count = Arc::new(AtomicUsize::new(0));
        let callback_count = Arc::clone(&completion_count);

        DefaultErrorApplet.show_error(
            ResultCode::new(110 | (42 << 9)),
            Box::new(move || {
                callback_count.fetch_add(1, Ordering::Relaxed);
            }),
        );

        assert_eq!(completion_count.load(Ordering::Relaxed), 1);
    }
}
