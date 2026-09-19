// SPDX-License-Identifier: GPL-3.0-or-later
//
// GTK compatibility adapters for APIs that GTK 4.10 replaced with
// AlertDialog and FileDialog. Keep only toolkit mechanics here; the owning
// frontend modules retain their actions and response handling.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib, ButtonsType, FileChooserAction, MessageType, ResponseType};
use crate::util::controller_navigation::{ControllerNavigation, NavigationKey};

/// Open an external URI using the platform's default application.
///
/// GIO's Windows build does not always provide a launcher for `https` URIs.
/// Eden reaches the default browser through `QDesktopServices`; use the
/// equivalent native Windows shell path there and retain GIO elsewhere.
#[cfg(target_os = "windows")]
pub fn open_external_uri(uri: &str) -> Result<(), String> {
    use std::ptr;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    if uri.encode_utf16().any(|unit| unit == 0) {
        return Err("URI contains an embedded NUL character".to_owned());
    }
    let uri: Vec<u16> = uri.encode_utf16().chain(std::iter::once(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            ptr::null(),
            uri.as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if result <= 32 {
        Err(format!("Windows ShellExecuteW failed with code {result}"))
    } else {
        Ok(())
    }
}

#[cfg(not(target_os = "windows"))]
pub fn open_external_uri(uri: &str) -> Result<(), String> {
    gio::AppInfo::launch_default_for_uri(uri, gio::AppLaunchContext::NONE)
        .map_err(|error| error.to_string())
}

/// Show a modal informational message using the GTK 4.0 MessageDialog API.
pub fn show_message<P: IsA<gtk::Window>>(parent: Option<&P>, message: &str, detail: &str) {
    show_message_with_type(parent, message, detail, MessageType::Info, false);
}

/// Show a modal informational message and run `callback` once it is closed.
pub fn show_message_then<P: IsA<gtk::Window>>(
    parent: Option<&P>,
    message: &str,
    detail: &str,
    callback: impl FnOnce() + 'static,
) {
    let message = crate::i18n::tr(message);
    let detail = crate::i18n::tr(detail);
    show_pretranslated_message_with_type_then(
        parent,
        &message,
        &detail,
        MessageType::Info,
        false,
        callback,
    );
}

/// Show an informational message whose title and detail were already passed
/// through the translation layer. This preserves dynamic values such as
/// emulator names and filesystem paths from brand normalization.
pub fn show_pretranslated_message<P: IsA<gtk::Window>>(
    parent: Option<&P>,
    message: &str,
    detail: &str,
) {
    show_pretranslated_message_with_type(parent, message, detail, MessageType::Info, false);
}

/// Show a modal warning using the GTK 4.0 MessageDialog API.
pub fn show_warning<P: IsA<gtk::Window>>(parent: Option<&P>, message: &str, detail: &str) {
    show_message_with_type(parent, message, detail, MessageType::Warning, false);
}

/// QMessageBox::warning with a continuation instead of Qt's nested event loop.
pub fn show_warning_then<P: IsA<gtk::Window>>(
    parent: Option<&P>, message: &str, detail: &str, callback: impl FnOnce() + 'static,
) {
    show_pretranslated_message_with_type_then(
        parent, &crate::i18n::tr(message), &crate::i18n::tr(detail),
        MessageType::Warning, false, callback,
    );
}

/// Show a modal error using the GTK 4.0 MessageDialog API.
pub fn show_error<P: IsA<gtk::Window>>(parent: Option<&P>, message: &str, detail: &str) {
    show_message_with_type(parent, message, detail, MessageType::Error, false);
}

/// Show a translated error and run `callback` once the user dismisses it.
pub fn show_pretranslated_error_then<P: IsA<gtk::Window>>(
    parent: Option<&P>,
    message: &str,
    detail: &str,
    callback: impl FnOnce() + 'static,
) {
    show_pretranslated_message_with_type_then(
        parent,
        message,
        detail,
        MessageType::Error,
        false,
        callback,
    );
}

fn show_message_with_type<P: IsA<gtk::Window>>(
    parent: Option<&P>,
    message: &str,
    detail: &str,
    message_type: MessageType,
    detail_uses_markup: bool,
) {
    let message = crate::i18n::tr(message);
    let detail = crate::i18n::tr(detail);
    show_pretranslated_message_with_type(
        parent,
        &message,
        &detail,
        message_type,
        detail_uses_markup,
    );
}

fn show_pretranslated_message_with_type<P: IsA<gtk::Window>>(
    parent: Option<&P>,
    message: &str,
    detail: &str,
    message_type: MessageType,
    detail_uses_markup: bool,
) {
    show_pretranslated_message_with_type_then(
        parent,
        message,
        detail,
        message_type,
        detail_uses_markup,
        || {},
    );
}

fn show_pretranslated_message_with_type_then<P: IsA<gtk::Window>>(
    parent: Option<&P>,
    message: &str,
    detail: &str,
    message_type: MessageType,
    detail_uses_markup: bool,
    callback: impl FnOnce() + 'static,
) {
    let detail = if detail_uses_markup {
        // Qt rich text uses HTML line breaks; GtkLabel consumes Pango markup.
        detail.replace("<br>", "\n").replace("<br/>", "\n")
    } else {
        detail.to_owned()
    };
    let dialog = gtk::MessageDialog::builder()
        .modal(true)
        .message_type(message_type)
        .buttons(ButtonsType::Ok)
        .text(message)
        .secondary_text(&detail)
        .secondary_use_markup(detail_uses_markup)
        .build();
    if let Some(parent) = parent {
        dialog.set_transient_for(Some(parent));
    }
    let callback = RefCell::new(Some(callback));
    dialog.connect_response(move |dialog, _| {
        dialog.close();
        if let Some(callback) = callback.borrow_mut().take() {
            callback();
        }
    });
    dialog.present();
    focus_dialog_response(&dialog, ResponseType::Ok);
}

/// Make a dialog response both the Enter-key default and the focused widget.
/// GTK's generated `ButtonsType::Ok` button is not guaranteed to receive
/// keyboard focus when a modal dialog is presented.
pub(crate) fn focus_dialog_response<D: IsA<gtk::Dialog> + Clone + 'static>(
    dialog: &D,
    response: ResponseType,
) {
    dialog.set_default_response(response);
    if let Some(widget) = dialog.widget_for_response(response) {
        widget.grab_focus();
    }

    let dialog = dialog.clone().upcast::<gtk::Dialog>();
    dialog.set_focus_visible(true);
    glib::idle_add_local_once(move || {
        if let Some(widget) = dialog.widget_for_response(response) {
            gtk::prelude::GtkWindowExt::set_focus(&dialog, Some(&widget));
            widget.grab_focus();
            dialog.set_focus_visible(true);
        }
    });
}

/// Show a two-button modal question and report whether the accept button won.
pub fn ask_question<P: IsA<gtk::Window>>(
    parent: Option<&P>,
    message: &str,
    detail: &str,
    cancel_label: &str,
    accept_label: &str,
    callback: impl FnOnce(bool) + 'static,
) {
    ask_question_with_navigation(
        parent, message, detail, cancel_label, accept_label, None, callback,
    );
}

/// GTK modal equivalent of ControllerNavigation's keyboard events. The caller
/// owns the decision to enable controller input; no emulated game is required.
pub fn ask_question_with_navigation<P: IsA<gtk::Window>>(
    parent: Option<&P>,
    message: &str,
    detail: &str,
    cancel_label: &str,
    accept_label: &str,
    navigation: Option<ControllerNavigation>,
    callback: impl FnOnce(bool) + 'static,
) -> gtk::MessageDialog {
    let title = message;
    let message = crate::i18n::tr(message);
    let detail = crate::i18n::tr(detail);
    let cancel_label = crate::i18n::tr(cancel_label);
    let accept_label = crate::i18n::tr(accept_label);
    let mut builder = gtk::MessageDialog::builder()
        .modal(true)
        .message_type(MessageType::Question)
        .buttons(ButtonsType::None)
        .text(&message)
        .secondary_text(&detail);
    if let Some(title) = question_window_title(title) {
        builder = builder.title(title);
    }
    let dialog = builder.build();
    if let Some(parent) = parent {
        dialog.set_transient_for(Some(parent));
    }
    dialog.add_button(&cancel_label, ResponseType::Cancel);
    dialog.add_button(&accept_label, ResponseType::Accept);
    dialog.set_default_response(ResponseType::Accept);

    let callback: Rc<RefCell<Option<Box<dyn FnOnce(bool)>>>> =
        Rc::new(RefCell::new(Some(Box::new(callback))));
    dialog.connect_response({
        let callback = Rc::clone(&callback);
        move |dialog, response| {
            // Qt returns from its modal question before the caller opens the
            // next dialog. Take the continuation before close-request's reject
            // fallback, then dismiss this window before handing focus onward.
            let callback = callback.borrow_mut().take();
            dialog.close();
            if let Some(callback) = callback {
                callback(response == ResponseType::Accept);
            }
        }
    });
    dialog.connect_close_request(move |_| {
        // QMessageBox::question returns the rejecting answer when its window is
        // dismissed. GTK does not guarantee a `response` signal when a modal
        // MessageDialog disappears with its parent, so complete the callback
        // here as well. `complete_question` is one-shot, making the normal
        // response-then-close path harmless.
        complete_question(&callback, false);
        glib::Propagation::Proceed
    });
    if let Some(navigation) = navigation {
        install_question_navigation(&dialog, navigation);
    }
    dialog.present();
    focus_dialog_response(&dialog, ResponseType::Accept);
    dialog
}

fn install_question_navigation(dialog: &gtk::MessageDialog, navigation: ControllerNavigation) {
    dialog.add_css_class("ruzu-applet-navigation");
    // Match the profile-selection applet: HID callbacks queue input, GTK drains
    // it on its main thread, and dropping the source unregisters both callbacks.
    let weak = dialog.downgrade();
    glib::timeout_add_local(std::time::Duration::from_millis(30), move || {
        let Some(dialog) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if !dialog.is_visible() {
            return glib::ControlFlow::Break;
        }
        if !dialog.is_active() {
            navigation.discard_pending_keys();
            return glib::ControlFlow::Continue;
        }
        for key in navigation.take_pending_keys() {
            if question_navigation_key(&dialog, key) {
                return glib::ControlFlow::Break;
            }
        }
        glib::ControlFlow::Continue
    });
}

/// Return true after responding, so later queued input cannot reach a closed
/// dialog or the file chooser/next startup question opened by its continuation.
fn question_navigation_key(dialog: &gtk::MessageDialog, key: NavigationKey) -> bool {
    let checkbox = dialog.message_area().last_child()
        .and_then(|widget| widget.downcast::<gtk::CheckButton>().ok());
    if let Some(checkbox) = checkbox {
        match key {
            NavigationKey::Up | NavigationKey::Previous
            | NavigationKey::Down | NavigationKey::Next => {
                // MessageDialog's built-in traversal can stay confined to its
                // action area. Include the optional content checkbox explicitly.
                let controls = [
                    Some(checkbox.upcast::<gtk::Widget>()),
                    dialog.widget_for_response(ResponseType::Cancel),
                    dialog.widget_for_response(ResponseType::Accept),
                ];
                let focus = gtk::prelude::GtkWindowExt::focus(dialog);
                let current = controls.iter().position(|control| *control == focus).unwrap_or(2);
                let next = if matches!(key, NavigationKey::Up | NavigationKey::Previous) {
                    (current + 2) % 3
                } else {
                    (current + 1) % 3
                };
                if let Some(control) = &controls[next] {
                    gtk::prelude::GtkWindowExt::set_focus(dialog, Some(control));
                    control.grab_focus();
                }
                dialog.set_focus_visible(true);
                return false;
            }
            NavigationKey::Enter => {
                if let Some(checkbox) = gtk::prelude::GtkWindowExt::focus(dialog)
                    .and_then(|widget| widget.downcast::<gtk::CheckButton>().ok())
                {
                    checkbox.set_active(!checkbox.is_active());
                    return false;
                }
            }
            _ => {}
        }
    }
    match key {
        NavigationKey::Left | NavigationKey::Up | NavigationKey::Previous => {
            focus_question_response(dialog, ResponseType::Cancel);
        }
        NavigationKey::Right | NavigationKey::Down | NavigationKey::Next => {
            focus_question_response(dialog, ResponseType::Accept);
        }
        NavigationKey::Enter => {
            let focused = gtk::prelude::GtkWindowExt::focus(dialog);
            let cancel = dialog.widget_for_response(ResponseType::Cancel);
            let response = if focused.is_some() && focused == cancel {
                ResponseType::Cancel
            } else {
                ResponseType::Accept
            };
            dialog.response(response);
            return true;
        }
        NavigationKey::Escape => {
            dialog.response(ResponseType::Cancel);
            return true;
        }
        NavigationKey::Menu => {}
    }
    false
}

fn focus_question_response(dialog: &gtk::MessageDialog, response: ResponseType) {
    dialog.set_default_response(response);
    if let Some(widget) = dialog.widget_for_response(response) {
        gtk::prelude::GtkWindowExt::set_focus(dialog, Some(&widget));
        widget.grab_focus();
    }
    dialog.set_focus_visible(true);
}

fn question_window_title(title: &str) -> Option<&str> {
    // GTK renders its own client-side title on Linux. Repeating the same text
    // as the primary MessageDialog label produces two adjacent headings there.
    // Windows and macOS use a distinct native title bar, where retaining the
    // window title keeps the existing platform presentation.
    if cfg!(target_os = "linux") {
        None
    } else {
        Some(title)
    }
}

type QuestionCallback = Rc<RefCell<Option<Box<dyn FnOnce(bool)>>>>;

fn complete_question(callback: &QuestionCallback, accepted: bool) {
    if let Some(callback) = callback.borrow_mut().take() {
        callback(accepted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    #[ignore = "requires GTK and desktop focus; run alone with --test-threads=1"]
    fn controller_file_chooser_browses_folders_and_completes_once() {
        use crate::util::controller_navigation::navigate_window;
        gtk::init().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let child = directory.path().join("nested");
        std::fs::create_dir(&child).unwrap();
        std::fs::create_dir(directory.path().join("other")).unwrap();
        let replies = Rc::new(RefCell::new(Vec::new()));
        run_controller_file_chooser(None::<&gtk::Window>, "Ruzu controller folder test",
            FileChooserAction::SelectFolder, false, Some(directory.path()), None, &[], None,
            { let replies = replies.clone(); move |files| replies.borrow_mut().push(files) });
        let dialog = gtk::Window::list_toplevels().into_iter()
            .find_map(|w| w.downcast::<gtk::FileChooserDialog>().ok()).unwrap();
        let pump_until = |condition: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !condition() && std::time::Instant::now() < deadline {
                glib::MainContext::default().iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert!(condition(), "GTK chooser did not reach the expected state (desktop focus required)");
        };
        pump_until(&|| dialog.is_active() && dialog.current_folder().and_then(|f| f.path()).as_deref() == Some(directory.path()));
        fn find_view(widget: &gtk::Widget) -> Option<gtk::Widget> {
            if !widget.is_visible() { return None; }
            if widget.is::<gtk::TreeView>() || widget.is::<gtk::ColumnView>() { return Some(widget.clone()); }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(view) = find_view(&widget) { return Some(view); }
                child = widget.next_sibling();
            }
            None
        }
        pump_until(&|| find_view(dialog.upcast_ref()).is_some());
        let view = find_view(dialog.upcast_ref()).unwrap();
        if let Some(tree) = view.downcast_ref::<gtk::TreeView>() {
            pump_until(&|| tree.model().is_some_and(|m| m.iter_n_children(None) == 2));
            tree.grab_focus();
            gtk::prelude::TreeViewExt::set_cursor(tree, &gtk::TreePath::from_indices(&[0]), None, false);
        } else {
            let column = view.downcast_ref::<gtk::ColumnView>().unwrap();
            pump_until(&|| column.model().is_some_and(|m| m.n_items() == 2));
            column.grab_focus();
            column.model().unwrap().select_item(0, true);
            navigate_window(dialog.upcast_ref(), NavigationKey::Down);
            assert!(column.model().unwrap().is_selected(1));
            navigate_window(dialog.upcast_ref(), NavigationKey::Up);
            assert!(column.model().unwrap().is_selected(0));
        }
        navigate_window(dialog.upcast_ref(), NavigationKey::Enter);
        pump_until(&|| dialog.current_folder().and_then(|f| f.path()).as_deref() == Some(child.as_path()));
        let accept = dialog.widget_for_response(ResponseType::Accept).unwrap();
        accept.grab_focus();
        navigate_window(dialog.upcast_ref(), NavigationKey::Enter);
        pump_until(&|| !replies.borrow().is_empty());
        assert_eq!(replies.borrow().len(), 1);
        assert_eq!(replies.borrow()[0][0].path(), Some(child));
        assert!(!dialog.is_visible());
        dialog.close();
        assert_eq!(replies.borrow().len(), 1);
        dialog.destroy();

        let cancelled = Rc::new(Cell::new(0));
        run_controller_file_chooser(None::<&gtk::Window>, "Ruzu controller cancel test",
            FileChooserAction::Open, false, Some(directory.path()), None, &[], None,
            { let cancelled = cancelled.clone(); move |files| { assert!(files.is_empty()); cancelled.set(cancelled.get() + 1); } });
        let dialog = gtk::Window::list_toplevels().into_iter()
            .find_map(|w| w.downcast::<gtk::FileChooserDialog>().ok()).unwrap();
        navigate_window(dialog.upcast_ref(), NavigationKey::Escape);
        pump_until(&|| cancelled.get() == 1);
        assert!(!dialog.is_visible());
        dialog.destroy();
        assert_eq!(cancelled.get(), 1);
    }

    #[test]
    #[ignore = "requires GTK and a display; run alone with --test-threads=1"]
    fn question_focus_and_controller_responses() {
        use input_common::drivers::virtual_gamepad::VirtualButton;
        gtk::init().unwrap();
        let parent = gtk::Window::new();
        parent.present();
        let context = glib::MainContext::default();
        for (keys, expected) in [
            (vec![NavigationKey::Enter], true),
            (vec![NavigationKey::Left, NavigationKey::Enter], false),
            (vec![NavigationKey::Left, NavigationKey::Right, NavigationKey::Enter], true),
            (vec![NavigationKey::Up, NavigationKey::Down, NavigationKey::Enter], true),
            (vec![NavigationKey::Escape], false),
        ] {
            let replies = Rc::new(RefCell::new(Vec::new()));
            let reply = Rc::clone(&replies);
            ask_question(Some(&parent), "Question navigation test", "Choose an action",
                         "No", "Yes", move |value| reply.borrow_mut().push(value));
            let dialog = gtk::Window::list_toplevels().into_iter()
                .find_map(|widget| widget.downcast::<gtk::MessageDialog>().ok()).unwrap();
            for _ in 0..100 {
                if !context.pending() { break; }
                context.iteration(false);
            }
            assert_eq!(gtk::prelude::GtkWindowExt::focus(&dialog),
                       dialog.widget_for_response(ResponseType::Accept));
            assert!(dialog.gets_focus_visible());
            for key in keys {
                let completed = question_navigation_key(&dialog, key);
                assert_eq!(completed, matches!(key, NavigationKey::Enter | NavigationKey::Escape));
            }
            assert_eq!(&*replies.borrow(), &[expected]);
            assert!(!dialog.is_visible());
            dialog.destroy();
        }
        // The optional startup checkbox is reachable and toggles without
        // answering the question; cancelling still completes it exactly once.
        let replies = Rc::new(RefCell::new(Vec::new()));
        let reply = Rc::clone(&replies);
        let dialog = ask_question_with_navigation(Some(&parent), "Checkbox question test",
            "Choose an action", "No", "Yes", None,
            move |accepted| reply.borrow_mut().push(accepted));
        let checkbox = gtk::CheckButton::with_label("Don't show again");
        dialog.message_area().downcast::<gtk::Box>().unwrap().append(&checkbox);
        for _ in 0..100 {
            if !context.pending() { break; }
            context.iteration(false);
        }
        focus_question_response(&dialog, ResponseType::Cancel);
        for _ in 0..4 {
            if gtk::prelude::GtkWindowExt::focus(&dialog).as_ref() == Some(checkbox.upcast_ref()) {
                break;
            }
            question_navigation_key(&dialog, NavigationKey::Up);
        }
        assert_eq!(gtk::prelude::GtkWindowExt::focus(&dialog).as_ref(), Some(checkbox.upcast_ref()));
        assert!(!question_navigation_key(&dialog, NavigationKey::Enter));
        assert!(checkbox.is_active());
        assert!(replies.borrow().is_empty());
        assert!(!question_navigation_key(&dialog, NavigationKey::Down));
        assert_ne!(gtk::prelude::GtkWindowExt::focus(&dialog).as_ref(), Some(checkbox.upcast_ref()));
        assert!(question_navigation_key(&dialog, NavigationKey::Escape));
        assert_eq!(&*replies.borrow(), &[false]);
        dialog.destroy();

        // Exercise the real input-engine -> HID -> ControllerNavigation ->
        // modal timer path before any emulation session exists.
        let mut input = input_common::InputSubsystem::new();
        input.initialize();
        let hid = std::sync::Arc::new(parking_lot::Mutex::new(hid_core::hid_core::HIDCore::new()));
        hid.lock().reload_input_devices();
        hid.lock().get_emulated_controller(hid_core::hid_types::NpadIdType::Player1)
            .lock().set_npad_style_index(hid_core::hid_types::NpadStyleIndex::Fullkey);
        let replies = Rc::new(RefCell::new(Vec::new()));
        let reply = Rc::clone(&replies);
        ask_question_with_navigation(Some(&parent), "Controller question test", "Choose an action",
            "No", "Yes", Some(ControllerNavigation::new(&hid)),
            move |value| reply.borrow_mut().push(value));
        let dialog = gtk::Window::list_toplevels().into_iter()
            .find_map(|widget| widget.downcast::<gtk::MessageDialog>().ok()).unwrap();
        let pump_until = |condition: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !condition() && std::time::Instant::now() < deadline {
                context.iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert!(condition());
        };
        pump_until(&|| dialog.is_active());
        input.get_virtual_gamepad_mut().unwrap().set_button_state(0, VirtualButton::ButtonLeft, true);
        pump_until(&|| gtk::prelude::GtkWindowExt::focus(&dialog)
            == dialog.widget_for_response(ResponseType::Cancel));
        assert!(dialog.gets_focus_visible());
        input.get_virtual_gamepad_mut().unwrap().set_button_state(0, VirtualButton::ButtonLeft, false);
        input.get_virtual_gamepad_mut().unwrap().set_button_state(0, VirtualButton::ButtonA, true);
        pump_until(&|| !replies.borrow().is_empty());
        assert_eq!(&*replies.borrow(), &[false]);
        assert!(!dialog.is_visible());
        input.get_virtual_gamepad_mut().unwrap().set_button_state(0, VirtualButton::ButtonA, false);
        dialog.destroy();
        hid.lock().unload_input_devices();
        input.shutdown();
        // Closing a controller-enabled dialog must not be prevented by the
        // polling callback retaining the GTK window.
        let hid = std::sync::Arc::new(parking_lot::Mutex::new(hid_core::hid_core::HIDCore::new()));
        let dialog = gtk::MessageDialog::new(None::<&gtk::Window>, gtk::DialogFlags::MODAL,
                                           MessageType::Question, ButtonsType::None, "Lifecycle");
        let weak = dialog.downgrade();
        install_question_navigation(&dialog, ControllerNavigation::new(&hid));
        dialog.destroy();
        drop(dialog);
        assert!(weak.upgrade().is_none());
        parent.destroy();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_uri_launcher_rejects_embedded_nul() {
        assert!(open_external_uri("https://example.invalid/\0suffix").is_err());
    }

    #[test]
    fn question_completion_is_one_shot_when_response_closes_dialog() {
        let calls = Rc::new(Cell::new(0));
        let accepted = Rc::new(Cell::new(false));
        let callback: QuestionCallback = Rc::new(RefCell::new(Some(Box::new({
            let calls = Rc::clone(&calls);
            let accepted = Rc::clone(&accepted);
            move |value| {
                calls.set(calls.get() + 1);
                accepted.set(value);
            }
        }))));

        complete_question(&callback, true);
        complete_question(&callback, false);

        assert_eq!(calls.get(), 1);
        assert!(accepted.get());
    }

    #[test]
    fn question_title_is_not_duplicated_by_linux_client_side_decorations() {
        if cfg!(target_os = "linux") {
            assert_eq!(question_window_title("ruzu"), None);
        } else {
            assert_eq!(question_window_title("ruzu"), Some("ruzu"));
        }
    }
}

/// Open a native file chooser and return the selected file, or `None` when
/// cancelled. This is the pre-4.10 counterpart of `FileDialog::open`.
pub fn open_file<P: IsA<gtk::Window>>(
    parent: Option<&P>,
    title: &str,
    filters: &[gtk::FileFilter],
    default_filter: Option<&gtk::FileFilter>,
    callback: impl FnOnce(Option<gio::File>) + 'static,
) {
    if *common::settings::values().controller_navigation.get_value() {
        run_controller_file_chooser(parent, title, FileChooserAction::Open, false,
            None, None, filters, default_filter, move |files| callback(files.into_iter().next()));
        return;
    }
    let title = crate::i18n::tr(title);
    let dialog = gtk::FileChooserNative::new(
        Some(&title),
        parent,
        FileChooserAction::Open,
        Some(&crate::i18n::tr("Open")),
        Some(&crate::i18n::tr("Cancel")),
    );
    dialog.set_modal(true);
    for filter in filters {
        dialog.add_filter(filter);
    }
    if let Some(filter) = default_filter {
        dialog.set_filter(filter);
    }
    // Unlike a GtkWindow, NativeDialog is not retained as an application
    // toplevel. Keep a strong reference until the response signal fires.
    let keep_alive = dialog.clone();
    dialog.run_async(move |dialog, response| {
        let file = (response == ResponseType::Accept)
            .then(|| dialog.file())
            .flatten();
        dialog.destroy();
        drop(keep_alive);
        callback(file);
    });
}

/// Open a native multi-file chooser. This is the pre-4.10 counterpart of
/// `FileDialog::open_multiple` and the GTK equivalent of Qt's
/// `QFileDialog::getOpenFileNames`.
pub fn open_files<P: IsA<gtk::Window>>(
    parent: Option<&P>,
    title: &str,
    initial_folder: Option<&std::path::Path>,
    filters: &[gtk::FileFilter],
    default_filter: Option<&gtk::FileFilter>,
    callback: impl FnOnce(Vec<gio::File>) + 'static,
) {
    if *common::settings::values().controller_navigation.get_value() {
        run_controller_file_chooser(parent, title, FileChooserAction::Open, true,
            initial_folder, None, filters, default_filter, callback);
        return;
    }
    let title = crate::i18n::tr(title);
    let dialog = gtk::FileChooserNative::new(
        Some(&title),
        parent,
        FileChooserAction::Open,
        Some(&crate::i18n::tr("Open")),
        Some(&crate::i18n::tr("Cancel")),
    );
    dialog.set_modal(true);
    dialog.set_select_multiple(true);
    for filter in filters {
        dialog.add_filter(filter);
    }
    if let Some(filter) = default_filter {
        dialog.set_filter(filter);
    }
    if let Some(initial_folder) = initial_folder {
        if let Err(error) = dialog.set_current_folder(Some(&gio::File::for_path(initial_folder))) {
            log::debug!(
                "Could not select initial file-chooser folder {}: {error}",
                initial_folder.display()
            );
        }
    }

    let keep_alive = dialog.clone();
    dialog.run_async(move |dialog, response| {
        let files = if response == ResponseType::Accept {
            let model = dialog.files();
            (0..model.n_items())
                .filter_map(|index| model.item(index)?.downcast::<gio::File>().ok())
                .collect()
        } else {
            Vec::new()
        };
        dialog.destroy();
        drop(keep_alive);
        callback(files);
    });
}

/// Open a native save-file chooser and return the selected file, or `None`
/// when cancelled. This is the pre-4.10 counterpart of `FileDialog::save`.
#[cfg(target_os = "windows")]
pub fn save_file<P: IsA<gtk::Window>>(
    parent: Option<&P>,
    title: &str,
    initial_file: &std::path::Path,
    filters: &[gtk::FileFilter],
    default_filter: Option<&gtk::FileFilter>,
    callback: impl FnOnce(Option<gio::File>) + 'static,
) {
    if *common::settings::values().controller_navigation.get_value() {
        run_controller_file_chooser(parent, title, FileChooserAction::Save, false,
            initial_file.parent(), initial_file.file_name().and_then(|name| name.to_str()),
            filters, default_filter, move |files| callback(files.into_iter().next()));
        return;
    }
    let title = crate::i18n::tr(title);
    let dialog = gtk::FileChooserNative::new(
        Some(&title),
        parent,
        FileChooserAction::Save,
        Some(&crate::i18n::tr("Save")),
        Some(&crate::i18n::tr("Cancel")),
    );
    dialog.set_modal(true);
    for filter in filters {
        dialog.add_filter(filter);
    }
    if let Some(filter) = default_filter {
        dialog.set_filter(filter);
    }
    if let Some(parent) = initial_file.parent() {
        let folder = gio::File::for_path(parent);
        if let Err(error) = dialog.set_current_folder(Some(&folder)) {
            log::warn!("Failed to select initial screenshot directory: {error}");
        }
    }
    if let Some(name) = initial_file.file_name().and_then(|name| name.to_str()) {
        dialog.set_current_name(name);
    }

    let keep_alive = dialog.clone();
    dialog.run_async(move |dialog, response| {
        let file = (response == ResponseType::Accept)
            .then(|| dialog.file())
            .flatten();
        dialog.destroy();
        drop(keep_alive);
        callback(file);
    });
}

/// Open a native directory chooser and return the selected folder, or `None`
/// when cancelled. This is the pre-4.10 counterpart of
/// `FileDialog::select_folder`.
pub fn select_folder<P: IsA<gtk::Window>>(
    parent: Option<&P>,
    title: &str,
    callback: impl FnOnce(Option<gio::File>) + 'static,
) {
    if *common::settings::values().controller_navigation.get_value() {
        run_controller_file_chooser(parent, title, FileChooserAction::SelectFolder, false,
            None, None, &[], None, move |files| callback(files.into_iter().next()));
        return;
    }
    let title = crate::i18n::tr(title);
    let dialog = gtk::FileChooserNative::new(
        Some(&title),
        parent,
        FileChooserAction::SelectFolder,
        Some(&crate::i18n::tr("Select")),
        Some(&crate::i18n::tr("Cancel")),
    );
    dialog.set_modal(true);
    // Unlike a GtkWindow, NativeDialog is not retained as an application
    // toplevel. Keep a strong reference until the response signal fires.
    let keep_alive = dialog.clone();
    dialog.run_async(move |dialog, response| {
        let folder = (response == ResponseType::Accept)
            .then(|| dialog.file())
            .flatten();
        dialog.destroy();
        drop(keep_alive);
        callback(folder);
    });
}

/// Native/portal file pickers may run outside our process and expose no GTK
/// focus tree. With controller navigation enabled, keep the standard GTK file
/// chooser in-process. GTK still owns filesystem browsing and validation;
/// frontend callers retain installation/scanning and response ownership.
#[allow(clippy::too_many_arguments)]
fn run_controller_file_chooser<P: IsA<gtk::Window>>(
    parent: Option<&P>, title: &str, action: FileChooserAction, multiple: bool,
    initial_folder: Option<&std::path::Path>, initial_name: Option<&str>,
    filters: &[gtk::FileFilter], default_filter: Option<&gtk::FileFilter>,
    callback: impl FnOnce(Vec<gio::File>) + 'static,
) {
    let accept = crate::i18n::tr(match action {
        FileChooserAction::SelectFolder => "Select",
        FileChooserAction::Save => "Save",
        _ => "Open",
    });
    let cancel = crate::i18n::tr("Cancel");
    let dialog = gtk::FileChooserDialog::new(
        Some(&crate::i18n::tr(title)), parent, action,
        &[(&cancel, ResponseType::Cancel), (&accept, ResponseType::Accept)],
    );
    dialog.set_modal(true);
    dialog.set_default_size(900, 600);
    dialog.set_default_response(ResponseType::Accept);
    dialog.set_select_multiple(multiple);
    for filter in filters { dialog.add_filter(filter); }
    if let Some(filter) = default_filter { dialog.set_filter(filter); }
    if let Some(folder) = initial_folder {
        if let Err(error) = dialog.set_current_folder(Some(&gio::File::for_path(folder))) {
            log::debug!("Could not select initial file-chooser folder: {error}");
        }
    }
    if let Some(name) = initial_name { dialog.set_current_name(name); }
    let callback = Rc::new(RefCell::new(Some(callback)));
    dialog.connect_response({
        let callback = callback.clone();
        move |dialog, response| {
            let files = if response == ResponseType::Accept {
                let model = dialog.files();
                (0..model.n_items())
                    .filter_map(|i| model.item(i)?.downcast::<gio::File>().ok()).collect()
            } else { Vec::new() };
            let callback = callback.borrow_mut().take();
            dialog.close();
            if let Some(callback) = callback { callback(files); }
        }
    });
    dialog.connect_close_request(move |_| {
        let callback = callback.borrow_mut().take();
        if let Some(callback) = callback { callback(Vec::new()); }
        glib::Propagation::Proceed
    });
    dialog.connect_map(|dialog| {
        glib::idle_add_local_once(glib::clone!(#[weak] dialog, move || {
            dialog.set_focus_visible(true);
        }));
    });
    dialog.present();
}
