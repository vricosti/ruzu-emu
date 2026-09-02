// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GTK counterpart of Eden `yuzu/applets/qt_error.{h,cpp}`.
//!
//! Error applet calls originate on the emulation thread.  Like Eden's queued
//! Qt signals, the channel keeps all window ownership on GTK's main thread.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use gtk::glib;
use hid_core::hid_core::HIDCore;
use parking_lot::Mutex;
use ruzu_core::frontend::applets::applet::Applet;
use ruzu_core::frontend::applets::error::{ErrorApplet, FinishedCallback};
use ruzu_core::hle::result::ResultCode;

pub(crate) enum ErrorAppletRequest {
    Display {
        error_code: String,
        error_text: String,
        finished: FinishedCallback,
    },
    Close,
}

/// Frontend object installed into `FrontendAppletHolder` for GUI boots.
pub(crate) struct GtkErrorDisplay {
    sender: Sender<ErrorAppletRequest>,
}

impl GtkErrorDisplay {
    pub(crate) fn new() -> (Arc<Self>, Receiver<ErrorAppletRequest>) {
        let (sender, receiver) = mpsc::channel();
        (Arc::new(Self { sender }), receiver)
    }

    fn format_error_code(error: ResultCode) -> String {
        format!(
            "Error Code: {:04}-{:04} (0x{:08X})",
            error.get_module_raw() + 2000,
            error.get_description(),
            error.get_inner_value()
        )
    }

    fn display(&self, error: ResultCode, error_text: String, finished: FinishedCallback) {
        let request = ErrorAppletRequest::Display {
            error_code: Self::format_error_code(error),
            error_text,
            finished,
        };
        if let Err(error) = self.sender.send(request) {
            log::error!("Error applet request receiver is no longer available");
            if let ErrorAppletRequest::Display { finished, .. } = error.0 {
                finished();
            }
        }
    }
}

impl Applet for GtkErrorDisplay {
    fn close(&self) {
        let _ = self.sender.send(ErrorAppletRequest::Close);
    }
}

impl ErrorApplet for GtkErrorDisplay {
    fn show_error(&self, error: ResultCode, finished: FinishedCallback) {
        self.display(
            error,
            "An error has occurred.\nPlease try again or contact the developer of the software."
                .to_owned(),
            finished,
        );
    }

    fn show_error_with_timestamp(
        &self,
        error: ResultCode,
        time_seconds: i64,
        finished: FinishedCallback,
    ) {
        self.display(
            error,
            format!(
                "An error occurred at Unix timestamp {time_seconds}.\n\
                 Please try again or contact the developer of the software."
            ),
            finished,
        );
    }

    fn show_custom_error_text(
        &self,
        error: ResultCode,
        dialog_text: String,
        fullscreen_text: String,
        finished: FinishedCallback,
    ) {
        self.display(
            error,
            format!("An error has occurred.\n\n{dialog_text}\n\n{fullscreen_text}"),
            finished,
        );
    }
}

struct ActiveDialog {
    dialog: Rc<crate::overlay_dialog::ErrorOverlayDialog>,
    finished: Rc<RefCell<Option<FinishedCallback>>>,
}

/// GTK-main-thread owner corresponding to Eden's `MainWindow` error-display
/// slots and `OverlayDialog` lifetime.
pub(crate) struct ErrorAppletFrontend {
    parent: gtk::ApplicationWindow,
    hid_core: Arc<Mutex<HIDCore>>,
    receiver: Receiver<ErrorAppletRequest>,
    active: RefCell<Option<ActiveDialog>>,
}

impl ErrorAppletFrontend {
    pub(crate) fn new(
        parent: &gtk::ApplicationWindow,
        hid_core: Arc<Mutex<HIDCore>>,
        receiver: Receiver<ErrorAppletRequest>,
    ) -> Rc<Self> {
        Rc::new(Self {
            parent: parent.clone(),
            hid_core,
            receiver,
            active: RefCell::new(None),
        })
    }

    pub(crate) fn start(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
            while let Ok(request) = this.receiver.try_recv() {
                this.handle_request(request);
            }
            glib::ControlFlow::Continue
        });
    }

    fn handle_request(self: &Rc<Self>, request: ErrorAppletRequest) {
        match request {
            ErrorAppletRequest::Display {
                error_code,
                error_text,
                finished,
            } => self.open_dialog(error_code, error_text, finished),
            ErrorAppletRequest::Close => self.finish_active(false),
        }
    }

    fn open_dialog(
        self: &Rc<Self>,
        error_code: String,
        error_text: String,
        finished: FinishedCallback,
    ) {
        self.finish_active(false);

        let dialog = crate::overlay_dialog::ErrorOverlayDialog::new(
            &self.parent,
            Arc::clone(&self.hid_core),
            &error_code,
            &error_text,
        );
        let finished = Rc::new(RefCell::new(Some(finished)));

        let weak = Rc::downgrade(self);
        dialog.connect_accepted(move || {
            if let Some(this) = weak.upgrade() {
                this.finish_active(true);
            }
        });

        *self.active.borrow_mut() = Some(ActiveDialog {
            dialog: dialog.clone(),
            finished,
        });
    }

    fn finish_active(&self, invoke_callback: bool) {
        let Some(active) = self.active.borrow_mut().take() else {
            return;
        };
        active.dialog.close();
        if invoke_callback {
            if let Some(finished) = active.finished.borrow_mut().take() {
                finished();
            }
        } else {
            active.finished.borrow_mut().take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_format_matches_upstream_frontend() {
        let error = ResultCode::new(110 | (42 << 9));
        assert_eq!(
            GtkErrorDisplay::format_error_code(error),
            "Error Code: 2110-0042 (0x0000546E)"
        );
    }
}
