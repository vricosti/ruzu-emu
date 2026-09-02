// SPDX-License-Identifier: GPL-3.0-or-later
//
// GTK counterpart of Eden's `src/yuzu/util/overlay_dialog.{h,cpp}`.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use common::settings_input::{native_analog, native_button};
use gtk::glib::Propagation;
use gtk::prelude::*;
use hid_core::hid_core::HIDCore;
use parking_lot::Mutex;

const BASE_PARENT_WIDTH: i32 = 1280;
const BASE_PARENT_HEIGHT: i32 = 720;
const BASE_PANEL_WIDTH: i32 = 780;
const BASE_PANEL_HEIGHT: i32 = 300;
const BASE_ACTION_HEIGHT: i32 = 82;

/// Borderless, window-modal status panel displayed while emulation shuts down.
pub struct OverlayDialog {
    window: gtk::Window,
    close_request_handler: gtk::glib::SignalHandlerId,
}

/// Interactive regular-text overlay used by the HLE error applet.
///
/// Eden deliberately uses `OverlayDialog` here instead of a native
/// `QMessageBox`: the action occupies the full bottom row, owns an explicit
/// focus border, and accepts controller A/B while emulation is running.
pub struct ErrorOverlayDialog {
    window: gtk::Window,
    action: gtk::Button,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControllerAction {
    Activate,
    Focus,
}

struct OverlayControllerInput {
    hid_core: Arc<Mutex<HIDCore>>,
    previous_a: Cell<bool>,
    previous_b: Cell<bool>,
    previous_left: Cell<bool>,
    previous_right: Cell<bool>,
}

impl OverlayDialog {
    /// Eden `MainWindow::OnShutdownBeginDialog`:
    /// `OverlayDialog(..., tr("Closing software..."), ..., AlignCenter)`.
    pub fn closing_software(parent: &gtk::ApplicationWindow) -> Self {
        install_css();

        let (width, height) = panel_size(parent.width(), parent.height());
        let label = gtk::Label::new(Some(&crate::i18n::tr("Closing software...")));
        label.set_hexpand(true);
        label.set_vexpand(true);
        label.set_halign(gtk::Align::Center);
        label.set_valign(gtk::Align::Center);
        label.set_justify(gtk::Justification::Center);
        label.set_wrap(true);
        label.add_css_class("ruzu-overlay-dialog-text");

        let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
        panel.set_hexpand(true);
        panel.set_vexpand(true);
        panel.add_css_class("ruzu-overlay-dialog-panel");
        panel.append(&label);

        let window = gtk::Window::builder()
            .modal(true)
            .transient_for(parent)
            .decorated(false)
            .resizable(false)
            .default_width(width)
            .default_height(height)
            .child(&panel)
            .build();

        // Eden ignores Escape when the overlay has no buttons. Prevent the
        // compositor's close shortcut from dismissing this status-only panel.
        let close_request_handler = window.connect_close_request(|_| Propagation::Stop);
        window.present();

        Self {
            window,
            close_request_handler,
        }
    }

    pub fn close(self) {
        // `Window::close` emits `close-request` too. Eden ignores only the
        // user's Escape/WM request while the status dialog is active; its
        // `deleteLater` from `OnEmulationStopped` must still destroy it.
        self.window.disconnect(self.close_request_handler);
        self.window.close();
    }
}

impl ErrorOverlayDialog {
    pub fn new(
        parent: &gtk::ApplicationWindow,
        hid_core: Arc<Mutex<HIDCore>>,
        title: &str,
        body: &str,
    ) -> Rc<Self> {
        install_css();

        let parent_width = effective_dimension(parent.width(), BASE_PARENT_WIDTH);
        let parent_height = effective_dimension(parent.height(), BASE_PARENT_HEIGHT);
        let (panel_width, panel_height) = panel_size(parent_width, parent_height);
        let action_height = (parent_height * BASE_ACTION_HEIGHT / BASE_PARENT_HEIGHT).max(1);

        let title = gtk::Label::new(Some(title));
        title.set_halign(gtk::Align::Start);
        title.set_wrap(true);
        title.add_css_class("ruzu-overlay-dialog-title");

        let body = gtk::Label::new(Some(body));
        body.set_hexpand(true);
        body.set_vexpand(true);
        body.set_halign(gtk::Align::Fill);
        body.set_valign(gtk::Align::Center);
        body.set_xalign(0.0);
        body.set_wrap(true);
        body.add_css_class("ruzu-overlay-dialog-body");

        let text = gtk::Box::new(gtk::Orientation::Vertical, 16);
        text.set_hexpand(true);
        text.set_vexpand(true);
        text.add_css_class("ruzu-overlay-dialog-content");
        text.append(&title);
        text.append(&body);

        let action = gtk::Button::with_label(&crate::i18n::tr("OK"));
        action.set_hexpand(true);
        action.set_focusable(true);
        action.set_can_focus(true);
        action.set_height_request(action_height);
        action.add_css_class("ruzu-overlay-dialog-action");

        let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
        panel.set_size_request(panel_width, panel_height);
        panel.set_halign(gtk::Align::Center);
        panel.set_valign(gtk::Align::Center);
        panel.add_css_class("ruzu-overlay-dialog-error-panel");
        panel.append(&text);
        panel.append(&action);

        let backdrop = gtk::Box::new(gtk::Orientation::Vertical, 0);
        backdrop.set_hexpand(true);
        backdrop.set_vexpand(true);
        backdrop.add_css_class("ruzu-overlay-dialog-backdrop");

        let root = gtk::Overlay::new();
        root.set_child(Some(&backdrop));
        root.add_overlay(&panel);

        let window = gtk::Window::builder()
            .modal(true)
            .transient_for(parent)
            .decorated(false)
            .resizable(false)
            .default_width(parent_width)
            .default_height(parent_height)
            .child(&root)
            .build();
        window.set_default_widget(Some(&action));

        let dialog = Rc::new(Self { window, action });
        dialog.install_keyboard_navigation();
        dialog.install_controller_navigation(OverlayControllerInput::new(hid_core));
        dialog.window.present();
        dialog.focus_action();
        dialog
    }

    pub fn connect_accepted(&self, callback: impl Fn() + 'static) {
        self.action.connect_clicked(move |_| callback());
    }

    pub fn close(&self) {
        self.window.close();
    }

    fn focus_action(&self) {
        gtk::prelude::GtkWindowExt::set_focus(&self.window, Some(&self.action));
        self.action.grab_focus();

        // `present()` queues the native surface mapping. Repeat focus on the
        // first main-loop turn so the compositor cannot leave it on the render
        // child while the dialog is still being mapped.
        let window = self.window.clone();
        let action = self.action.clone();
        gtk::glib::idle_add_local_once(move || {
            gtk::prelude::GtkWindowExt::set_focus(&window, Some(&action));
            action.grab_focus();
        });
    }

    fn install_keyboard_navigation(self: &Rc<Self>) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let weak = Rc::downgrade(self);
            move |_, key, _, _| {
                if !matches!(
                    key,
                    gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter | gtk::gdk::Key::Escape
                ) {
                    return Propagation::Proceed;
                }
                if let Some(dialog) = weak.upgrade() {
                    dialog.action.emit_by_name::<()>("clicked", &[]);
                }
                Propagation::Stop
            }
        });
        self.window.add_controller(keys);
    }

    fn install_controller_navigation(self: &Rc<Self>, controller_input: OverlayControllerInput) {
        let weak = Rc::downgrade(self);
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            let Some(dialog) = weak.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            for action in controller_input.take_actions() {
                match action {
                    ControllerAction::Activate => {
                        dialog.action.emit_by_name::<()>("clicked", &[]);
                    }
                    ControllerAction::Focus => dialog.focus_action(),
                }
            }
            gtk::glib::ControlFlow::Continue
        });
    }
}

impl OverlayControllerInput {
    fn new(hid_core: Arc<Mutex<HIDCore>>) -> Self {
        Self {
            hid_core,
            previous_a: Cell::new(false),
            previous_b: Cell::new(false),
            previous_left: Cell::new(false),
            previous_right: Cell::new(false),
        }
    }

    fn take_actions(&self) -> Vec<ControllerAction> {
        let (player, handheld) = {
            let hid_core = self.hid_core.lock();
            (
                hid_core.get_emulated_controller(hid_core::hid_types::NpadIdType::Player1),
                hid_core.get_emulated_controller(hid_core::hid_types::NpadIdType::Handheld),
            )
        };
        let (player_buttons, player_sticks) = {
            let controller = player.lock();
            (
                controller.get_buttons_values(),
                controller.get_sticks_values(),
            )
        };
        let (handheld_buttons, handheld_sticks) = {
            let controller = handheld.lock();
            (
                controller.get_buttons_values(),
                controller.get_sticks_values(),
            )
        };

        let button = |index: native_button::Values| {
            player_buttons[index as usize].value || handheld_buttons[index as usize].value
        };
        let a = button(native_button::Values::A);
        let b = button(native_button::Values::B);
        let left_stick = native_analog::Values::LStick as usize;
        let left = button(native_button::Values::DLeft)
            || player_sticks[left_stick].left
            || handheld_sticks[left_stick].left;
        let right = button(native_button::Values::DRight)
            || player_sticks[left_stick].right
            || handheld_sticks[left_stick].right;

        let activate = rising_edge(&self.previous_a, a) | rising_edge(&self.previous_b, b);
        let focus =
            rising_edge(&self.previous_left, left) | rising_edge(&self.previous_right, right);
        let mut actions = Vec::with_capacity(2);
        if focus {
            actions.push(ControllerAction::Focus);
        }
        if activate {
            actions.push(ControllerAction::Activate);
        }
        actions
    }
}

fn rising_edge(previous: &Cell<bool>, current: bool) -> bool {
    let rising = current && !previous.get();
    previous.set(current);
    rising
}

fn effective_dimension(value: i32, fallback: i32) -> i32 {
    if value > 0 {
        value
    } else {
        fallback
    }
}

fn panel_size(parent_width: i32, parent_height: i32) -> (i32, i32) {
    let parent_width = if parent_width > 0 {
        parent_width
    } else {
        BASE_PARENT_WIDTH
    };
    let parent_height = if parent_height > 0 {
        parent_height
    } else {
        BASE_PARENT_HEIGHT
    };

    (
        (parent_width * BASE_PANEL_WIDTH / BASE_PARENT_WIDTH).max(1),
        (parent_height * BASE_PANEL_HEIGHT / BASE_PARENT_HEIGHT).max(1),
    )
}

fn install_css() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else {
            return;
        };
        let provider = gtk::CssProvider::new();
        provider.load_from_data(
            ".ruzu-overlay-dialog-panel {\
                 background-color: rgb(240, 240, 240);\
                 border-radius: 6px;\
             }\
             .ruzu-overlay-dialog-text {\
                 color: rgb(44, 44, 44);\
                 font-family: sans-serif;\
                 font-size: 18pt;\
                 font-weight: normal;\
                 padding: 20px 65px;\
             }\
             .ruzu-overlay-dialog-backdrop {\
                 background-color: rgba(35, 35, 35, 0.94);\
             }\
             .ruzu-overlay-dialog-error-panel {\
                 background-color: rgb(243, 243, 243);\
                 border-radius: 6px;\
             }\
             .ruzu-overlay-dialog-content {\
                 padding: 36px 65px 24px 65px;\
             }\
             .ruzu-overlay-dialog-title {\
                 color: rgb(128, 128, 128);\
                 font-family: sans-serif;\
                 font-size: 14pt;\
             }\
             .ruzu-overlay-dialog-body {\
                 color: rgb(12, 12, 12);\
                 font-family: sans-serif;\
                 font-size: 18pt;\
             }\
             button.ruzu-overlay-dialog-action {\
                 background: rgb(255, 255, 255);\
                 border: 3px solid transparent;\
                 border-radius: 0 0 6px 6px;\
                 box-shadow: none;\
                 color: rgb(48, 80, 224);\
                 font-family: sans-serif;\
                 font-size: 18pt;\
                 padding: 0;\
             }\
             button.ruzu-overlay-dialog-action:focus,\
             button.ruzu-overlay-dialog-action:focus-visible {\
                 border-color: rgb(102, 229, 180);\
                 outline: none;\
             }",
        );
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn shutdown_panel_uses_edens_regular_overlay_proportions() {
        assert_eq!(panel_size(1280, 720), (780, 300));
        assert_eq!(panel_size(2560, 1440), (1560, 600));
    }

    #[test]
    fn shutdown_panel_falls_back_to_edens_base_geometry_before_map() {
        assert_eq!(panel_size(0, 0), (780, 300));
    }

    #[test]
    fn interactive_controller_actions_only_trigger_on_rising_edges() {
        let previous = Cell::new(false);
        assert!(rising_edge(&previous, true));
        assert!(!rising_edge(&previous, true));
        assert!(!rising_edge(&previous, false));
        assert!(rising_edge(&previous, true));
    }

    #[test]
    fn programmatic_close_bypasses_the_user_close_guard() {
        if gtk::init().is_err() {
            return;
        }
        let window = gtk::Window::new();
        let close_was_blocked = Rc::new(Cell::new(false));
        let close_was_blocked_for_handler = Rc::clone(&close_was_blocked);
        let close_request_handler = window.connect_close_request(move |_| {
            close_was_blocked_for_handler.set(true);
            Propagation::Stop
        });
        OverlayDialog {
            window,
            close_request_handler,
        }
        .close();

        assert!(!close_was_blocked.get());
    }
}
