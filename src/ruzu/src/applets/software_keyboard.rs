// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GTK software keyboard applet.
//!
//! The HLE applet runs on an emulation thread while GTK owns the dialog, so
//! requests cross a channel exactly like `applets::controller` does. The
//! GTK widgets replace Qt, while the applet lifecycle, on-screen key grid and
//! controller bindings follow Eden's `qt_software_keyboard` frontend.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use common::settings_input::native_button;
use gtk::prelude::*;
use gtk::{glib, ResponseType};
use hid_core::hid_core::HIDCore;
use hid_core::hid_types::NpadIdType;
use ruzu_core::frontend::applets::applet::Applet;
use ruzu_core::frontend::applets::software_keyboard::{
    InlineAppearParameters, InlineTextParameters, KeyboardInitializeParameters,
    SoftwareKeyboardApplet, SubmitInlineCallback, SubmitNormalCallback, SwkbdPasswordMode,
    SwkbdReplyType, SwkbdResult, SwkbdTextCheckResult, SwkbdType,
};

/// Work item handed from the emulation thread to the GTK main thread.
pub(crate) enum SoftwareKeyboardRequest {
    ShowNormalKeyboard,
    ShowTextCheckDialog {
        result: SwkbdTextCheckResult,
        message: String,
    },
    UpdateInlineText(InlineTextParameters),
    HideKeyboard,
    Close,
}

/// Shared state written by the applet contract and read when building a dialog.
///
/// The callbacks are shared here rather than travelling with each request,
/// because `initialize_keyboard` installs them once and every later request
/// reuses them.
#[derive(Default)]
struct KeyboardState {
    initialized: bool,
    parameters: KeyboardInitializeParameters,
    is_inline: bool,
    current_text: String,
    submit_normal: Option<Arc<dyn Fn(SwkbdResult, String, bool) + Send + Sync>>,
    submit_inline: Option<Arc<dyn Fn(SwkbdReplyType, String, i32) + Send + Sync>>,
}

/// Frontend object installed into `FrontendAppletSet` for GUI boots.
pub(crate) struct GtkSoftwareKeyboard {
    sender: Sender<SoftwareKeyboardRequest>,
    state: Arc<Mutex<KeyboardState>>,
}

impl GtkSoftwareKeyboard {
    pub(crate) fn new() -> (Arc<Self>, Receiver<SoftwareKeyboardRequest>) {
        let (sender, receiver) = mpsc::channel();
        (
            Arc::new(Self {
                sender,
                state: Arc::new(Mutex::new(KeyboardState::default())),
            }),
            receiver,
        )
    }

    fn send(&self, request: SoftwareKeyboardRequest) {
        if self.sender.send(request).is_err() {
            log::error!("Software keyboard request receiver is no longer available");
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, KeyboardState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

impl Applet for GtkSoftwareKeyboard {
    fn close(&self) {
        self.exit_keyboard();
    }
}

impl SoftwareKeyboardApplet for GtkSoftwareKeyboard {
    fn initialize_keyboard(
        &self,
        is_inline: bool,
        initialize_parameters: KeyboardInitializeParameters,
        submit_normal_callback: SubmitNormalCallback,
        submit_inline_callback: SubmitInlineCallback,
    ) {
        let mut state = self.state();
        if state.initialized {
            log::error!("The software keyboard is already initialized");
            return;
        }
        state.initialized = true;
        state.current_text = initialize_parameters.initial_text.clone();
        state.parameters = initialize_parameters;
        state.is_inline = is_inline;
        state.submit_normal = Some(Arc::from(submit_normal_callback));
        state.submit_inline = Some(Arc::from(submit_inline_callback));
    }

    fn show_normal_keyboard(&self) {
        if !self.state().initialized {
            log::error!("The software keyboard is not initialized");
            return;
        }
        self.send(SoftwareKeyboardRequest::ShowNormalKeyboard);
    }

    fn show_text_check_dialog(
        &self,
        text_check_result: SwkbdTextCheckResult,
        text_check_message: String,
    ) {
        if !self.state().initialized {
            log::error!("The software keyboard is not initialized");
            return;
        }
        self.send(SoftwareKeyboardRequest::ShowTextCheckDialog {
            result: text_check_result,
            message: text_check_message,
        });
    }

    fn show_inline_keyboard(&self, appear_parameters: InlineAppearParameters) {
        // Carry the appear-time constraints into the dialog just as Eden's
        // `QtSoftwareKeyboardDialog::ShowInlineKeyboard` does.
        {
            let mut state = self.state();
            if !state.initialized {
                log::error!("The software keyboard is not initialized");
                return;
            }
            state.is_inline = true;
            state.parameters.max_text_length = appear_parameters.max_text_length;
            state.parameters.min_text_length = appear_parameters.min_text_length;
            state.parameters.swkbd_type = appear_parameters.swkbd_type;
            state.parameters.key_disable_flags = appear_parameters.key_disable_flags;
            state.parameters.enable_backspace_button = appear_parameters.enable_backspace_button;
            state.parameters.enable_return_button = appear_parameters.enable_return_button;
            state.parameters.disable_cancel_button = appear_parameters.disable_cancel_button;
        }
        self.send(SoftwareKeyboardRequest::ShowNormalKeyboard);
    }

    fn hide_inline_keyboard(&self) {
        if !self.state().initialized {
            log::error!("The software keyboard is not initialized");
            return;
        }
        self.send(SoftwareKeyboardRequest::HideKeyboard);
    }

    fn inline_text_changed(&self, text_parameters: InlineTextParameters) {
        let mut state = self.state();
        if !state.initialized {
            log::error!("The software keyboard is not initialized");
            return;
        }
        state.current_text = text_parameters.input_text.clone();
        drop(state);
        self.send(SoftwareKeyboardRequest::UpdateInlineText(text_parameters));
    }

    fn exit_keyboard(&self) {
        self.state().initialized = false;
        self.send(SoftwareKeyboardRequest::Close);
    }
}

/// Styling that reproduces upstream's two-panel look: a dark panel carrying
/// the header and the underlined text field, a light key panel below it, and a
/// blue OK key (see `qt_software_keyboard.ui`).
const KEYBOARD_CSS: &str = "
.swkbd-header { background-color: #4a4a4a; padding: 24px 32px; }
.swkbd-header-text { color: #ffffff; font-size: 20pt; }
.swkbd-entry { background: transparent; color: #ffffff; font-size: 18pt;
               border: none; border-bottom: 1px solid #cfcfcf; border-radius: 0;
               padding: 4px 2px; box-shadow: none; }
.swkbd-counter { color: #d0d0d0; font-size: 11pt; }
.swkbd-keys { background-color: #f2f2f2; padding: 10px; }
.swkbd-keys button { background: #e6e6e6; border: 1px solid #dcdcdc;
                     border-radius: 2px; color: #1a1a1a; font-size: 14pt;
                     box-shadow: none; }
.swkbd-keys button:hover { background: #dadada; }
.swkbd-keys button:disabled { color: #a8a8a8; }
.swkbd-ok { background: #3050e0; color: #ffffff; font-size: 15pt; }
.swkbd-ok:hover { background: #2742c4; }
.swkbd-ok:disabled { background: #9aa8e8; color: #eeeeee; }
.swkbd-selected { outline: 3px solid #3050e0; outline-offset: -3px; }
.swkbd-hints { background-color: #f2f2f2; padding: 6px 12px; color: #333333; }
";

/// Controller actions of upstream `QtSoftwareKeyboardDialog::TranslateButtonPress`
/// (qt_software_keyboard.cpp:1273-1380).
#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyboardAction {
    PressSelected,
    Backspace,
    Cancel,
    Space,
    Shift,
    CursorLeft,
    CursorRight,
    Ok,
    Move(i32, i32),
}

/// Polls the emulated pad for the keyboard's own button set.
///
/// `util::controller_navigation` only carries Enter/Escape/directions, which is
/// what upstream's `ControllerNavigation` exposes; the software keyboard needs
/// Y, X, Plus, L/R and the stick clicks as well, so it reads the controller
/// directly the way upstream's input thread does.
struct KeyboardNavigation {
    hid_core: Arc<parking_lot::Mutex<HIDCore>>,
    previous: RefCell<Vec<bool>>,
    previous_directions: RefCell<[bool; 4]>,
}

impl KeyboardNavigation {
    fn new(hid_core: Arc<parking_lot::Mutex<HIDCore>>) -> Self {
        Self {
            hid_core,
            previous: RefCell::new(vec![false; native_button::NUM_BUTTONS]),
            previous_directions: RefCell::new([false; 4]),
        }
    }

    /// Rising edges since the last poll, translated to keyboard actions.
    fn take_actions(&self) -> Vec<KeyboardAction> {
        let (player, handheld) = {
            let hid_core = self.hid_core.lock();
            (
                hid_core.get_emulated_controller(NpadIdType::Player1),
                hid_core.get_emulated_controller(NpadIdType::Handheld),
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

        let mut previous = self.previous.borrow_mut();
        let mut actions = Vec::new();
        let mut pressed = |index: native_button::Values| -> bool {
            let index = index as usize;
            let value = player_buttons[index].value || handheld_buttons[index].value;
            let rising = value && !previous[index];
            previous[index] = value;
            rising
        };

        use native_button::Values as NB;
        if pressed(NB::A) {
            actions.push(KeyboardAction::PressSelected);
        }
        if pressed(NB::B) {
            actions.push(KeyboardAction::Backspace);
        }
        if pressed(NB::X) {
            actions.push(KeyboardAction::Cancel);
        }
        if pressed(NB::Y) {
            actions.push(KeyboardAction::Space);
        }
        if pressed(NB::LStick) || pressed(NB::RStick) {
            actions.push(KeyboardAction::Shift);
        }
        if pressed(NB::L) {
            actions.push(KeyboardAction::CursorLeft);
        }
        if pressed(NB::R) {
            actions.push(KeyboardAction::CursorRight);
        }
        if pressed(NB::Plus) {
            actions.push(KeyboardAction::Ok);
        }
        if pressed(NB::DLeft) {
            actions.push(KeyboardAction::Move(-1, 0));
        }
        if pressed(NB::DRight) {
            actions.push(KeyboardAction::Move(1, 0));
        }
        if pressed(NB::DUp) {
            actions.push(KeyboardAction::Move(0, -1));
        }
        if pressed(NB::DDown) {
            actions.push(KeyboardAction::Move(0, 1));
        }

        let directions = player_sticks.iter().chain(&handheld_sticks).fold(
            [false; 4],
            |mut directions, stick| {
                directions[0] |= stick.left;
                directions[1] |= stick.right;
                directions[2] |= stick.up;
                directions[3] |= stick.down;
                directions
            },
        );
        let mut previous_directions = self.previous_directions.borrow_mut();
        for (index, action) in [
            KeyboardAction::Move(-1, 0),
            KeyboardAction::Move(1, 0),
            KeyboardAction::Move(0, -1),
            KeyboardAction::Move(0, 1),
        ]
        .into_iter()
        .enumerate()
        {
            if directions[index] && !previous_directions[index] {
                actions.push(action);
            }
        }
        *previous_directions = directions;
        actions
    }
}

/// Key roles of upstream's grid; every other key inserts its own label.
#[derive(Clone, Copy)]
enum Key {
    Char,
    Space,
    Return,
    Backspace,
    Shift,
    Ok,
}

/// Rows 0-3 of the lower-case grid in `qt_software_keyboard.ui`. The keys that
/// span cells (backspace, return, shift, space, OK) are attached separately.
const LOWER_ROWS: [[&str; 11]; 4] = [
    ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "-"],
    ["q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "/"],
    ["a", "s", "d", "f", "g", "h", "j", "k", "l", ":", "'"],
    ["z", "x", "c", "v", "b", "n", "m", ",", ".", "?", "!"],
];

/// Upper-case counterpart, same geometry.
const UPPER_ROWS: [[&str; 11]; 4] = [
    ["#", "[", "]", "$", "%", "^", "&", "*", "(", ")", "_"],
    ["Q", "W", "E", "R", "T", "Y", "U", "I", "O", "P", "@"],
    ["A", "S", "D", "F", "G", "H", "J", "K", "L", ";", "\""],
    ["Z", "X", "C", "V", "B", "N", "M", "<", ">", "+", "="],
];

const KEY_DISABLE_SPACE: u32 = 1 << 1;
const KEY_DISABLE_AT: u32 = 1 << 2;
const KEY_DISABLE_PERCENT: u32 = 1 << 3;
const KEY_DISABLE_SLASH: u32 = 1 << 4;
const KEY_DISABLE_BACKSLASH: u32 = 1 << 5;
const KEY_DISABLE_NUMBERS: u32 = 1 << 6;
const KEY_DISABLE_USERNAME: u32 = 1 << 8;

struct ActiveDialog {
    dialog: gtk::Dialog,
    entry: gtk::Entry,
    keys: gtk::Grid,
    refresh: Rc<dyn Fn()>,
    parameters: KeyboardInitializeParameters,
    ok_slot: Rc<RefCell<Option<gtk::Button>>>,
    shifted: Rc<std::cell::Cell<bool>>,
    caps_lock: Cell<bool>,
    /// Cell -> button map with spanned keys repeated in every cell they cover,
    /// mirroring upstream's `keyboard_buttons` / `numberpad_buttons` arrays.
    grid: Vec<Vec<gtk::Button>>,
    selected: std::cell::Cell<(usize, usize)>,
    shift_button: Option<gtk::Button>,
    space_button: Option<gtk::Button>,
    backspace_button: Option<gtk::Button>,
    ok_button: Option<gtk::Button>,
    submission_pending: Cell<bool>,
}

/// GTK-main-thread owner of the keyboard dialog.
pub(crate) struct SoftwareKeyboardFrontend {
    parent: gtk::ApplicationWindow,
    receiver: Receiver<SoftwareKeyboardRequest>,
    state: Arc<Mutex<KeyboardState>>,
    active: RefCell<Option<ActiveDialog>>,
    navigation: KeyboardNavigation,
}

impl SoftwareKeyboardFrontend {
    pub(crate) fn new(
        parent: &gtk::ApplicationWindow,
        keyboard: &Arc<GtkSoftwareKeyboard>,
        receiver: Receiver<SoftwareKeyboardRequest>,
        hid_core: Arc<parking_lot::Mutex<HIDCore>>,
    ) -> Rc<Self> {
        Rc::new(Self {
            parent: parent.clone(),
            receiver,
            state: Arc::clone(&keyboard.state),
            active: RefCell::new(None),
            navigation: KeyboardNavigation::new(hid_core),
        })
    }

    pub(crate) fn start(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
            // Keep the edge detector current even without a dialog. When a
            // request opens one, discard that tick's actions so the button
            // which opened the keyboard cannot immediately activate a key.
            let actions = this.navigation.take_actions();
            let mut handled_request = false;
            while let Ok(request) = this.receiver.try_recv() {
                handled_request = true;
                this.handle_request(request);
            }
            if !handled_request && this.active.borrow().is_some() {
                for action in actions {
                    this.handle_action(action);
                }
            }
            glib::ControlFlow::Continue
        });
    }

    fn handle_request(self: &Rc<Self>, request: SoftwareKeyboardRequest) {
        match request {
            SoftwareKeyboardRequest::ShowNormalKeyboard => self.open_dialog(false),
            SoftwareKeyboardRequest::ShowTextCheckDialog { result, message } => {
                self.show_text_check(result, message)
            }
            SoftwareKeyboardRequest::UpdateInlineText(parameters) => {
                self.update_inline_text(parameters)
            }
            SoftwareKeyboardRequest::HideKeyboard | SoftwareKeyboardRequest::Close => {
                self.dismiss_without_submitting()
            }
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, KeyboardState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Build the on-screen keyboard, mirroring the key grid of upstream
    /// `qt_software_keyboard.ui`: 5 rows x 12 columns for the alphabetic
    /// layouts, 4 x 4 for the number pad, with the same spans for shift,
    /// space, return and OK.
    fn open_dialog(self: &Rc<Self>, start_shifted: bool) {
        // A second request while a dialog is up replaces it, as a new request
        // means the guest restarted the interaction.
        self.dismiss_without_submitting();

        let (parameters, is_inline, current_text) = {
            let state = self.state();
            (
                state.parameters.clone(),
                state.is_inline,
                state.current_text.clone(),
            )
        };
        log::info!(
            "Opening GTK software keyboard: header={:?} max_len={} inline={}",
            parameters.header_text,
            parameters.max_text_length,
            is_inline
        );

        let provider = gtk::CssProvider::new();
        provider.load_from_data(KEYBOARD_CSS);
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        let dialog = gtk::Dialog::builder()
            .transient_for(&self.parent)
            .modal(true)
            .title("Software Keyboard")
            .build();
        dialog.set_default_size(860, 520);

        let content = dialog.content_area();
        content.set_spacing(0);

        // Upper dark panel: header, the underlined field and the counter.
        let header_panel = gtk::Box::new(gtk::Orientation::Vertical, 12);
        header_panel.add_css_class("swkbd-header");
        header_panel.set_vexpand(true);
        content.append(&header_panel);

        if !parameters.header_text.is_empty() {
            let header = gtk::Label::new(Some(&parameters.header_text));
            header.set_xalign(0.0);
            header.add_css_class("swkbd-header-text");
            header_panel.append(&header);
        }
        if !parameters.sub_text.is_empty() {
            let label = gtk::Label::new(Some(&parameters.sub_text));
            label.set_xalign(0.0);
            label.set_wrap(true);
            label.add_css_class("swkbd-counter");
            header_panel.append(&label);
        }

        let entry = gtk::Entry::new();
        entry.set_text(&current_text);
        if !parameters.guide_text.is_empty() {
            entry.set_placeholder_text(Some(&parameters.guide_text));
        }
        entry.set_hexpand(true);
        if parameters.max_text_length > 0 {
            entry.set_max_length(parameters.max_text_length as i32);
        }
        if !matches!(parameters.password_mode, SwkbdPasswordMode::Disabled) {
            entry.set_visibility(false);
        }
        if parameters.initial_cursor_position >= 0 {
            entry.set_position(gtk_cursor_from_utf16(
                &current_text,
                parameters.initial_cursor_position,
            ));
        }

        entry.add_css_class("swkbd-entry");
        entry.set_has_frame(false);
        EditableExt::set_alignment(&entry, 0.0);

        // Upstream shows a `length/max` counter under the field
        // (qt_software_keyboard.cpp:721).
        let counter = gtk::Label::new(None);
        counter.add_css_class("swkbd-counter");
        counter.set_xalign(1.0);

        let field = gtk::Box::new(gtk::Orientation::Vertical, 2);
        field.set_halign(gtk::Align::Center);
        field.set_valign(gtk::Align::Center);
        field.set_vexpand(true);
        field.set_size_request(560, -1);
        field.append(&entry);
        field.append(&counter);
        header_panel.append(&field);

        let key_panel = gtk::Box::new(gtk::Orientation::Vertical, 6);
        key_panel.add_css_class("swkbd-keys");
        content.append(&key_panel);

        let keys = gtk::Grid::builder()
            .row_spacing(4)
            .column_spacing(4)
            .row_homogeneous(true)
            .column_homogeneous(true)
            .build();
        key_panel.append(&keys);

        let shifted = Rc::new(std::cell::Cell::new(start_shifted));
        let ok_button: Rc<RefCell<Option<gtk::Button>>> = Rc::new(RefCell::new(None));
        let refresh: Rc<dyn Fn()> = {
            let entry = entry.clone();
            let counter = counter.clone();
            let ok_button = Rc::clone(&ok_button);
            let parameters = parameters.clone();
            Rc::new(move || {
                let text = entry.text();
                counter.set_text(&format!(
                    "{}/{}",
                    utf16_len(&text),
                    parameters.max_text_length
                ));
                if let Some(button) = ok_button.borrow().as_ref() {
                    let enabled = if is_inline {
                        utf16_len(&text) >= parameters.min_text_length
                    } else {
                        validate_input_text(&text, &parameters)
                    };
                    button.set_sensitive(enabled);
                }
            })
        };

        let (grid, [shift_button, space_button, backspace_button]) =
            self.populate_keys(&keys, &entry, &refresh, &parameters, &shifted, &ok_button);

        // Bottom hint bar, matching the controller legend upstream draws under
        // the keys: L/R move, Shift, X cancels, A enters.
        let hints = gtk::Box::new(gtk::Orientation::Horizontal, 18);
        hints.add_css_class("swkbd-hints");
        hints.set_halign(gtk::Align::End);
        for hint in ["L \u{2190}", "R \u{2192}", "\u{21e7} Shift"] {
            let label = gtk::Label::new(Some(hint));
            hints.append(&label);
        }
        if !parameters.disable_cancel_button {
            let cancel = gtk::Button::with_label("\u{24cd} Cancel");
            cancel.set_has_frame(false);
            let weak = Rc::downgrade(self);
            cancel.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.finish(false);
                }
            });
            hints.append(&cancel);
        }
        let enter = gtk::Button::with_label("\u{24b6} Enter");
        enter.set_has_frame(false);
        {
            let weak = Rc::downgrade(self);
            let ok_slot = Rc::clone(&ok_button);
            enter.connect_clicked(move |_| {
                let enabled = ok_slot
                    .borrow()
                    .as_ref()
                    .is_none_or(gtk::prelude::WidgetExt::is_sensitive);
                if enabled {
                    if let Some(this) = weak.upgrade() {
                        this.finish(true);
                    }
                }
            });
        }
        hints.append(&enter);
        key_panel.append(&hints);

        refresh();

        let weak = Rc::downgrade(self);
        dialog.connect_response(move |_, response| {
            let Some(this) = weak.upgrade() else {
                return;
            };
            this.finish(response == ResponseType::Accept);
        });

        let weak = Rc::downgrade(self);
        dialog.connect_close_request(move |_| {
            if let Some(this) = weak.upgrade() {
                this.finish(false);
            }
            glib::Propagation::Proceed
        });

        *self.active.borrow_mut() = Some(ActiveDialog {
            dialog: dialog.clone(),
            entry: entry.clone(),
            keys: keys.clone(),
            refresh: Rc::clone(&refresh),
            parameters: parameters.clone(),
            ok_slot: Rc::clone(&ok_button),
            shifted,
            caps_lock: Cell::new(false),
            grid,
            selected: std::cell::Cell::new((0, 0)),
            shift_button,
            space_button,
            backspace_button,
            ok_button: ok_button.borrow().clone(),
            submission_pending: Cell::new(false),
        });
        dialog.present();
        if let Some(active) = self.active.borrow().as_ref() {
            Self::select_key(active, 0, 0);
        }
    }

    /// Fill `keys` with one of upstream's three layouts. Upstream swaps the
    /// grid in place through `ChangeBottomOSKIndex`, so the dialog and the text
    /// stay put when Shift is pressed.
    fn populate_keys(
        self: &Rc<Self>,
        keys: &gtk::Grid,
        entry: &gtk::Entry,
        refresh: &Rc<dyn Fn()>,
        parameters: &KeyboardInitializeParameters,
        shifted_state: &Rc<std::cell::Cell<bool>>,
        ok_button: &Rc<RefCell<Option<gtk::Button>>>,
    ) -> (Vec<Vec<gtk::Button>>, [Option<gtk::Button>; 3]) {
        while let Some(child) = keys.first_child() {
            keys.remove(&child);
        }
        *ok_button.borrow_mut() = None;

        let entry = entry.clone();
        let refresh = Rc::clone(refresh);
        let parameters = parameters.clone();
        let shifted = Rc::clone(shifted_state);
        let ok_button = Rc::clone(ok_button);
        let special: Rc<RefCell<[Option<gtk::Button>; 3]>> =
            Rc::new(RefCell::new([None, None, None]));
        let use_numberpad = matches!(parameters.swkbd_type, SwkbdType::NumberPad);

        let insert = {
            let entry = entry.clone();
            let refresh = Rc::clone(&refresh);
            let weak = Rc::downgrade(self);
            let max_text_length = parameters.max_text_length;
            move |text: &str| {
                if utf16_len(&entry.text()).saturating_add(utf16_len(text)) > max_text_length {
                    return;
                }
                let mut position = entry.position();
                entry.insert_text(text, &mut position);
                entry.set_position(position);
                refresh();
                if let Some(this) = weak.upgrade() {
                    this.submit_inline_update(SwkbdReplyType::ChangedString, &entry);
                    this.update_backspace_sensitive(&entry);
                }
            }
        };

        // Upstream repeats the same button pointer in every cell a key spans;
        // the same trick keeps directional movement simple here.
        let grid_cells: Rc<RefCell<Vec<Vec<Option<gtk::Button>>>>> =
            Rc::new(RefCell::new(vec![vec![None; 12]; 5]));
        let build_key = |label: &str, key: Key, col: i32, row: i32, width: i32, height: i32| {
            let button = gtk::Button::with_label(label);
            button.set_can_focus(true);
            keys.attach(&button, col, row, width, height);
            let flags = parameters.key_disable_flags.raw;
            if use_numberpad && matches!(key, Key::Char) && label.is_empty() {
                button.set_sensitive(false);
                button.set_visible(false);
            } else if !use_numberpad {
                let disabled = match key {
                    Key::Space => flags & KEY_DISABLE_SPACE != 0,
                    Key::Char => {
                        (label == "@" && flags & (KEY_DISABLE_AT | KEY_DISABLE_USERNAME) != 0)
                            || (label == "%"
                                && flags & (KEY_DISABLE_PERCENT | KEY_DISABLE_USERNAME) != 0)
                            || (label == "/" && flags & KEY_DISABLE_SLASH != 0)
                            || (label == "\\"
                                && flags & (KEY_DISABLE_BACKSLASH | KEY_DISABLE_USERNAME) != 0)
                            || (label.chars().all(|character| character.is_ascii_digit())
                                && flags & KEY_DISABLE_NUMBERS != 0)
                    }
                    _ => false,
                };
                button.set_sensitive(!disabled);
            }
            {
                let mut cells = grid_cells.borrow_mut();
                for r in row..row + height {
                    for c in col..col + width {
                        if let Some(cell) = cells
                            .get_mut(r as usize)
                            .and_then(|cells| cells.get_mut(c as usize))
                        {
                            *cell = Some(button.clone());
                        }
                    }
                }
            }
            match key {
                Key::Char => {
                    let insert = insert.clone();
                    let label = label.to_owned();
                    let weak = Rc::downgrade(self);
                    button.connect_clicked(move |_| {
                        insert(&label);
                        if let Some(this) = weak.upgrade() {
                            this.revert_temporary_shift();
                        }
                    });
                }
                Key::Space => {
                    special.borrow_mut()[1] = Some(button.clone());
                    let insert = insert.clone();
                    button.connect_clicked(move |_| insert(" "));
                }
                Key::Return => {
                    let insert = insert.clone();
                    button.connect_clicked(move |_| insert("\n"));
                }
                Key::Backspace => {
                    special.borrow_mut()[2] = Some(button.clone());
                    let entry = entry.clone();
                    let refresh = Rc::clone(&refresh);
                    let weak = Rc::downgrade(self);
                    button
                        .set_sensitive(parameters.enable_backspace_button && entry.position() > 0);
                    button.connect_clicked(move |_| {
                        let position = entry.position();
                        if position > 0 {
                            entry.delete_text(position - 1, position);
                            entry.set_position(position - 1);
                            if let Some(this) = weak.upgrade() {
                                this.submit_inline_update(SwkbdReplyType::ChangedString, &entry);
                                this.update_backspace_sensitive(&entry);
                            }
                        }
                        refresh();
                    });
                }
                Key::Shift => {
                    special.borrow_mut()[0] = Some(button.clone());
                    let weak = Rc::downgrade(self);
                    button.connect_clicked(move |_| {
                        if let Some(this) = weak.upgrade() {
                            this.rebuild_keys();
                        }
                    });
                }
                Key::Ok => {
                    button.add_css_class("swkbd-ok");
                    let weak = Rc::downgrade(self);
                    button.connect_clicked(move |_| {
                        if let Some(this) = weak.upgrade() {
                            this.finish(true);
                        }
                    });
                    *ok_button.borrow_mut() = Some(button.clone());
                }
            }
            button
        };

        if use_numberpad {
            let left = char::from_u32(parameters.left_optional_symbol_key as u32)
                .filter(|c| !c.is_control())
                .map(String::from)
                .unwrap_or_default();
            let right = char::from_u32(parameters.right_optional_symbol_key as u32)
                .filter(|c| !c.is_control())
                .map(String::from)
                .unwrap_or_default();
            let rows = [["1", "2", "3"], ["4", "5", "6"], ["7", "8", "9"]];
            for (row, labels) in rows.iter().enumerate() {
                for (col, label) in labels.iter().enumerate() {
                    build_key(label, Key::Char, col as i32, row as i32, 1, 1);
                }
            }
            build_key(&left, Key::Char, 0, 3, 1, 1);
            build_key("0", Key::Char, 1, 3, 1, 1);
            build_key(&right, Key::Char, 2, 3, 1, 1);
            build_key("\u{232b} \u{24b7}", Key::Backspace, 3, 0, 1, 1);
            let ok_text = (!parameters.ok_text.is_empty())
                .then_some(parameters.ok_text.as_str())
                .unwrap_or("OK \u{271a}");
            build_key(ok_text, Key::Ok, 3, 1, 1, 3);
        } else {
            let rows = if shifted.get() {
                UPPER_ROWS
            } else {
                LOWER_ROWS
            };
            for (col, label) in rows[0].iter().enumerate() {
                build_key(label, Key::Char, col as i32, 0, 1, 1);
            }
            build_key("\u{232b} \u{24b7}", Key::Backspace, 11, 0, 1, 1);
            for (col, label) in rows[1].iter().enumerate() {
                build_key(label, Key::Char, col as i32, 1, 1, 1);
            }
            for (col, label) in rows[2].iter().enumerate() {
                build_key(label, Key::Char, col as i32, 2, 1, 1);
            }
            let return_key = build_key("\u{21b5}", Key::Return, 11, 1, 1, 2);
            return_key.set_sensitive(parameters.enable_return_button);
            for (col, label) in rows[3].iter().enumerate() {
                build_key(label, Key::Char, col as i32, 3, 1, 1);
            }
            build_key("\u{21e7}", Key::Shift, 0, 4, 2, 1);
            build_key("Space \u{24ce}", Key::Space, 2, 4, 9, 1);
            let ok_text = (!parameters.ok_text.is_empty())
                .then_some(parameters.ok_text.as_str())
                .unwrap_or("OK \u{271a}");
            build_key(ok_text, Key::Ok, 11, 3, 1, 2);
        }

        let grid: Vec<Vec<gtk::Button>> = grid_cells
            .borrow()
            .iter()
            .map(|row| row.iter().flatten().cloned().collect())
            .filter(|row: &Vec<gtk::Button>| !row.is_empty())
            .collect();
        let specials = special.borrow().clone();
        log::info!(
            "Software keyboard layout built: numberpad={} rows={} keys={}",
            use_numberpad,
            grid.len(),
            grid.iter().map(Vec::len).sum::<usize>()
        );
        refresh();
        (grid, specials)
    }

    /// Move the highlight, mirroring upstream's `MoveButtonDirection`.
    fn select_key(active: &ActiveDialog, row: usize, col: usize) {
        let (previous_row, previous_col) = active.selected.get();
        if let Some(button) = active
            .grid
            .get(previous_row)
            .and_then(|row| row.get(previous_col))
        {
            button.remove_css_class("swkbd-selected");
        }
        let Some(button) = active.grid.get(row).and_then(|row| row.get(col)) else {
            return;
        };
        active.selected.set((row, col));
        button.add_css_class("swkbd-selected");
        button.grab_focus();
    }

    /// Upstream wraps at every edge and skips both disabled buttons and the
    /// repeated cells occupied by a key spanning several rows or columns.
    fn move_selection(active: &ActiveDialog, dx: i32, dy: i32) {
        if active.grid.is_empty() {
            return;
        }
        let (initial_row, initial_col) = active.selected.get();
        let Some(previous_button) = active
            .grid
            .get(initial_row)
            .and_then(|row| row.get(initial_col))
            .cloned()
        else {
            return;
        };

        let mut row = initial_row;
        let mut col = initial_col;
        let max_steps = active.grid.iter().map(Vec::len).sum::<usize>();
        for _ in 0..max_steps {
            row = (row as i32 + dy).rem_euclid(active.grid.len() as i32) as usize;
            let columns = active.grid[row].len();
            if columns == 0 {
                continue;
            }
            col = (col as i32 + dx).rem_euclid(columns as i32) as usize;
            let button = &active.grid[row][col];
            if button.is_sensitive() && button != &previous_button {
                Self::select_key(active, row, col);
                return;
            }
            if (row, col) == (initial_row, initial_col) {
                return;
            }
        }
    }

    /// Port of `QtSoftwareKeyboardDialog::TranslateButtonPress`
    /// (qt_software_keyboard.cpp:1273): the pad drives the same widgets the
    /// mouse does, so every action ends in a `clicked` emission.
    fn handle_action(self: &Rc<Self>, action: KeyboardAction) {
        let can_handle = self
            .active
            .borrow()
            .as_ref()
            .is_some_and(|active| !active.submission_pending.get());
        if !can_handle {
            return;
        }

        match action {
            KeyboardAction::PressSelected => {
                let button = self.active.borrow().as_ref().and_then(|active| {
                    let (row, col) = active.selected.get();
                    active.grid.get(row).and_then(|row| row.get(col)).cloned()
                });
                if let Some(button) = button.filter(WidgetExt::is_sensitive) {
                    button.emit_clicked();
                }
            }
            KeyboardAction::Backspace => {
                let button = self
                    .active
                    .borrow()
                    .as_ref()
                    .and_then(|active| active.backspace_button.clone());
                if let Some(button) = button.filter(WidgetExt::is_sensitive) {
                    button.emit_clicked();
                }
            }
            KeyboardAction::Space => {
                let button = self
                    .active
                    .borrow()
                    .as_ref()
                    .and_then(|active| active.space_button.clone());
                if let Some(button) = button.filter(WidgetExt::is_sensitive) {
                    button.emit_clicked();
                }
            }
            KeyboardAction::Shift => {
                let button = self
                    .active
                    .borrow()
                    .as_ref()
                    .and_then(|active| active.shift_button.clone());
                if let Some(button) = button.filter(WidgetExt::is_sensitive) {
                    button.emit_clicked();
                }
            }
            KeyboardAction::Ok => {
                let button = self
                    .active
                    .borrow()
                    .as_ref()
                    .and_then(|active| active.ok_button.clone());
                if let Some(button) = button.filter(WidgetExt::is_sensitive) {
                    button.emit_clicked();
                }
            }
            KeyboardAction::CursorLeft => {
                let entry = self
                    .active
                    .borrow()
                    .as_ref()
                    .map(|active| active.entry.clone());
                if let Some(entry) = entry {
                    let position = entry.position();
                    let new_position = (position - 1).max(0);
                    if new_position != position {
                        entry.set_position(new_position);
                        self.submit_inline_update(SwkbdReplyType::MovedCursor, &entry);
                        self.update_backspace_sensitive(&entry);
                    }
                }
            }
            KeyboardAction::CursorRight => {
                let entry = self
                    .active
                    .borrow()
                    .as_ref()
                    .map(|active| active.entry.clone());
                if let Some(entry) = entry {
                    let position = entry.position();
                    entry.set_position(position + 1);
                    if entry.position() != position {
                        self.submit_inline_update(SwkbdReplyType::MovedCursor, &entry);
                        self.update_backspace_sensitive(&entry);
                    }
                }
            }
            KeyboardAction::Cancel => self.finish(false),
            KeyboardAction::Move(dx, dy) => {
                if let Some(active) = self.active.borrow().as_ref() {
                    Self::move_selection(active, dx, dy);
                }
            }
        }
    }

    /// Port of upstream's three-state Shift/Caps Lock transition.
    fn rebuild_keys(self: &Rc<Self>) {
        {
            let active = self.active.borrow();
            let Some(active) = active.as_ref() else {
                return;
            };
            if !active.shifted.get() {
                active.shifted.set(true);
            } else if active.caps_lock.get() {
                active.caps_lock.set(false);
                active.shifted.set(false);
            } else {
                active.caps_lock.set(true);
            }
        }
        self.refresh_key_layout();
    }

    /// A single shifted character returns to lower case; caps lock does not.
    fn revert_temporary_shift(self: &Rc<Self>) {
        let should_rebuild = self.active.borrow().as_ref().is_some_and(|active| {
            if active.shifted.get() && !active.caps_lock.get() {
                active.shifted.set(false);
                true
            } else {
                false
            }
        });
        if should_rebuild {
            self.refresh_key_layout();
        }
    }

    fn refresh_key_layout(self: &Rc<Self>) {
        let parts = {
            let active = self.active.borrow();
            let Some(active) = active.as_ref() else {
                return;
            };
            (
                active.keys.clone(),
                active.entry.clone(),
                Rc::clone(&active.refresh),
                active.parameters.clone(),
                Rc::clone(&active.shifted),
                Rc::clone(&active.ok_slot),
            )
        };
        let (keys, entry, refresh, parameters, shifted, ok_slot) = parts;
        let (grid, [shift_button, space_button, backspace_button]) =
            self.populate_keys(&keys, &entry, &refresh, &parameters, &shifted, &ok_slot);

        let mut active = self.active.borrow_mut();
        let Some(active) = active.as_mut() else {
            return;
        };
        active.grid = grid;
        active.shift_button = shift_button;
        active.space_button = space_button;
        active.backspace_button = backspace_button;
        active.ok_button = ok_slot.borrow().clone();
        let (row, col) = active.selected.get();
        Self::select_key(active, row, col);
    }

    /// Apply a guest-driven inline text/cursor update to the visible widget.
    fn update_inline_text(&self, parameters: InlineTextParameters) {
        let active = self
            .active
            .borrow()
            .as_ref()
            .map(|active| (active.entry.clone(), Rc::clone(&active.refresh)));
        if let Some((entry, refresh)) = active {
            entry.set_text(&parameters.input_text);
            entry.set_position(gtk_cursor_from_utf16(
                &parameters.input_text,
                parameters.cursor_position,
            ));
            refresh();
            self.update_backspace_sensitive(&entry);
        }
    }

    fn update_backspace_sensitive(&self, entry: &gtk::Entry) {
        if let Some(active) = self.active.borrow().as_ref() {
            if let Some(button) = active.backspace_button.as_ref() {
                button.set_sensitive(
                    active.parameters.enable_backspace_button && entry.position() > 0,
                );
            }
        }
    }

    /// Keep the inline applet's frontend state synchronized after an edit and
    /// emit the same reply type as Eden's `InlineKeyboardButtonClicked` and
    /// `MoveTextCursorDirection` paths.
    fn submit_inline_update(&self, reply: SwkbdReplyType, entry: &gtk::Entry) {
        let text = entry.text().to_string();
        let cursor = utf16_cursor(&text, entry.position());
        let callback = {
            let mut state = self.state();
            state.current_text = text.clone();
            state
                .is_inline
                .then(|| state.submit_inline.clone())
                .flatten()
        };
        if let Some(callback) = callback {
            callback(reply, text, cursor);
        }
    }

    /// Port of `QtSoftwareKeyboardDialog::ShowTextCheckDialog`
    /// (qt_software_keyboard.cpp:393): `Success` and `Silent` show nothing,
    /// `Failure` reports the message and leaves the keyboard open, `Confirm`
    /// asks the player and, on acceptance, submits the text as confirmed.
    fn show_text_check(self: &Rc<Self>, result: SwkbdTextCheckResult, message: String) {
        match result {
            SwkbdTextCheckResult::Failure => {
                let dialog = gtk::MessageDialog::builder()
                    .transient_for(&self.parent)
                    .modal(true)
                    .message_type(gtk::MessageType::Warning)
                    .buttons(gtk::ButtonsType::Ok)
                    .text(message)
                    .build();
                let weak = Rc::downgrade(self);
                dialog.connect_response(move |dialog, _| {
                    dialog.close();
                    if let Some(this) = weak.upgrade() {
                        this.set_submission_pending(false);
                    }
                });
                dialog.present();
            }
            SwkbdTextCheckResult::Confirm => {
                let dialog = gtk::MessageDialog::builder()
                    .transient_for(&self.parent)
                    .modal(true)
                    .message_type(gtk::MessageType::Question)
                    .buttons(gtk::ButtonsType::OkCancel)
                    .text(message)
                    .build();
                let weak = Rc::downgrade(self);
                dialog.connect_response(move |dialog, response| {
                    dialog.close();
                    if let Some(this) = weak.upgrade() {
                        if response == ResponseType::Ok {
                            this.submit_confirmed();
                        } else {
                            this.set_submission_pending(false);
                        }
                    }
                });
                dialog.present();
            }
            // `Silent` and `Success` need no interaction.
            _ => {}
        }
    }

    /// Upstream emits `SubmitNormalText(SwkbdResult::Ok, text, true)` once the
    /// player accepts a `Confirm` dialog (qt_software_keyboard.cpp:425).
    fn submit_confirmed(&self) {
        let Some(text) = self
            .active
            .borrow()
            .as_ref()
            .map(|active| active.entry.text().to_string())
        else {
            return;
        };
        let callback = self.state().submit_normal.clone();
        if let Some(callback) = callback {
            callback(SwkbdResult::Ok, text, true);
        }
    }

    fn set_submission_pending(&self, pending: bool) {
        if let Some(active) = self.active.borrow().as_ref() {
            active.submission_pending.set(pending);
        }
    }

    /// Answer the guest while retaining the dialog until `ExitKeyboard`.
    ///
    /// Upstream submits the current text with either `SwkbdResult::Ok` or
    /// `SwkbdResult::Cancel` (qt_software_keyboard.cpp:1145 and 1309), and the
    /// inline keyboard replies `DecidedEnter` / `DecidedCancel` (1207, 1303).
    fn finish(&self, accepted: bool) {
        let Some((text, cursor)) = self.active.borrow().as_ref().and_then(|active| {
            if active.submission_pending.replace(true) {
                None
            } else {
                let text = active.entry.text().to_string();
                let cursor = utf16_cursor(&text, active.entry.position());
                Some((text, cursor))
            }
        }) else {
            return;
        };

        let (is_inline, submit_inline, submit_normal) = {
            let mut state = self.state();
            state.current_text = text.clone();
            (
                state.is_inline,
                state.submit_inline.clone(),
                state.submit_normal.clone(),
            )
        };
        if is_inline {
            if let Some(callback) = submit_inline {
                let reply = if accepted {
                    SwkbdReplyType::DecidedEnter
                } else {
                    SwkbdReplyType::DecidedCancel
                };
                callback(reply, text, cursor);
            }
            return;
        }

        if let Some(callback) = submit_normal {
            let result = if accepted {
                SwkbdResult::Ok
            } else {
                SwkbdResult::Cancel
            };
            callback(result, text, false);
        }
    }

    /// Close a dialog the guest withdrew, without answering it.
    fn dismiss_without_submitting(&self) {
        let active = self.active.borrow_mut().take();
        if let Some(active) = active {
            active.dialog.close();
        }
    }
}

fn utf16_len(text: &str) -> u32 {
    text.encode_utf16().count() as u32
}

fn utf16_cursor(text: &str, gtk_cursor: i32) -> i32 {
    text.chars()
        .take(gtk_cursor.max(0) as usize)
        .map(char::len_utf16)
        .sum::<usize>() as i32
}

fn gtk_cursor_from_utf16(text: &str, utf16_cursor: i32) -> i32 {
    let target = utf16_cursor.max(0) as usize;
    let mut units = 0;
    let mut characters = 0;
    for character in text.chars() {
        if units + character.len_utf16() > target {
            break;
        }
        units += character.len_utf16();
        characters += 1;
    }
    characters
}

/// Port of `QtSoftwareKeyboardDialog::ValidateInputText`
/// (qt_software_keyboard.cpp:999). The bit positions come from upstream
/// `SwkbdKeyDisableFlags` (applet_software_keyboard_types.h:110).
fn validate_input_text(input_text: &str, parameters: &KeyboardInitializeParameters) -> bool {
    let flags = parameters.key_disable_flags.raw;
    let has = |flag: u32| flags & flag != 0;
    let length = utf16_len(input_text);

    if length < parameters.min_text_length || length > parameters.max_text_length {
        return false;
    }
    if has(KEY_DISABLE_SPACE) && input_text.contains(' ') {
        return false;
    }
    if (has(KEY_DISABLE_AT) || has(KEY_DISABLE_USERNAME)) && input_text.contains('@') {
        return false;
    }
    if (has(KEY_DISABLE_PERCENT) || has(KEY_DISABLE_USERNAME)) && input_text.contains('%') {
        return false;
    }
    if has(KEY_DISABLE_SLASH) && input_text.contains('/') {
        return false;
    }
    if (has(KEY_DISABLE_BACKSLASH) || has(KEY_DISABLE_USERNAME)) && input_text.contains('\\') {
        return false;
    }
    if has(KEY_DISABLE_NUMBERS) && input_text.chars().any(char::is_numeric) {
        return false;
    }
    if matches!(parameters.swkbd_type, SwkbdType::NumberPad)
        && input_text.chars().any(|character| {
            !character.is_numeric()
                && character as u32 != parameters.left_optional_symbol_key as u32
                && character as u32 != parameters.right_optional_symbol_key as u32
        })
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parameters(min: u32, max: u32, flags: u32) -> KeyboardInitializeParameters {
        KeyboardInitializeParameters {
            min_text_length: min,
            max_text_length: max,
            key_disable_flags:
                ruzu_core::frontend::applets::software_keyboard::SwkbdKeyDisableFlags { raw: flags },
            ..Default::default()
        }
    }

    #[test]
    fn length_bounds_match_upstream() {
        let params = parameters(1, 3, 0);
        assert!(!validate_input_text("", &params));
        assert!(validate_input_text("AB", &params));
        assert!(validate_input_text("ABC", &params));
        assert!(!validate_input_text("ABCD", &params));
    }

    #[test]
    fn lengths_and_cursor_positions_use_utf16_code_units() {
        let params = parameters(2, 2, 0);
        assert!(validate_input_text("😀", &params));
        assert_eq!(utf16_cursor("A😀B", 2), 3);
        assert_eq!(gtk_cursor_from_utf16("A😀B", 3), 2);
    }

    #[test]
    fn disabled_keys_reject_their_characters() {
        assert!(!validate_input_text("a b", &parameters(0, 8, 1 << 1)));
        assert!(!validate_input_text("a@b", &parameters(0, 8, 1 << 2)));
        assert!(!validate_input_text("a1b", &parameters(0, 8, 1 << 6)));
        // `username` disables @, % and backslash together, as upstream does.
        let username = parameters(0, 8, 1 << 8);
        assert!(!validate_input_text("a@b", &username));
        assert!(!validate_input_text("a%b", &username));
        assert!(!validate_input_text("a\\b", &username));
        assert!(validate_input_text("ab", &username));
    }

    #[test]
    fn number_pad_only_accepts_digits_and_configured_symbols() {
        let mut params = parameters(0, 8, 0);
        params.swkbd_type = SwkbdType::NumberPad;
        params.left_optional_symbol_key = '-' as u16;
        params.right_optional_symbol_key = '.' as u16;

        assert!(validate_input_text("12-3.4", &params));
        assert!(!validate_input_text("12A", &params));
    }

    #[test]
    fn inline_appear_parameters_replace_all_upstream_fields() {
        use ruzu_core::frontend::applets::software_keyboard::SwkbdKeyDisableFlags;

        let (keyboard, _receiver) = GtkSoftwareKeyboard::new();
        keyboard.initialize_keyboard(
            false,
            KeyboardInitializeParameters::default(),
            Box::new(|_, _, _| {}),
            Box::new(|_, _, _| {}),
        );
        keyboard.show_inline_keyboard(InlineAppearParameters {
            max_text_length: 42,
            min_text_length: 3,
            swkbd_type: SwkbdType::NumberPad,
            key_disable_flags: SwkbdKeyDisableFlags { raw: 0x156 },
            enable_backspace_button: true,
            enable_return_button: true,
            disable_cancel_button: true,
            ..Default::default()
        });

        let state = keyboard.state();
        assert!(state.is_inline);
        assert_eq!(state.parameters.max_text_length, 42);
        assert_eq!(state.parameters.min_text_length, 3);
        assert_eq!(state.parameters.swkbd_type, SwkbdType::NumberPad);
        assert_eq!(state.parameters.key_disable_flags.raw, 0x156);
        assert!(state.parameters.enable_backspace_button);
        assert!(state.parameters.enable_return_button);
        assert!(state.parameters.disable_cancel_button);
    }

    #[test]
    fn exit_allows_the_persistent_frontend_to_initialize_for_the_next_session() {
        let (keyboard, receiver) = GtkSoftwareKeyboard::new();
        keyboard.initialize_keyboard(
            true,
            KeyboardInitializeParameters {
                header_text: "first".to_owned(),
                ..Default::default()
            },
            Box::new(|_, _, _| {}),
            Box::new(|_, _, _| {}),
        );

        keyboard.exit_keyboard();
        assert!(matches!(receiver.recv().unwrap(), SoftwareKeyboardRequest::Close));

        keyboard.initialize_keyboard(
            true,
            KeyboardInitializeParameters {
                header_text: "second".to_owned(),
                ..Default::default()
            },
            Box::new(|_, _, _| {}),
            Box::new(|_, _, _| {}),
        );

        let state = keyboard.state();
        assert!(state.initialized);
        assert_eq!(state.parameters.header_text, "second");
    }
}
