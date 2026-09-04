// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/configuration/configure_input_player.cpp`
// (`ConfigureInputPlayer`), whose widget tree lives in
// `configure_input_player.ui`.
//
// Layout, top to bottom:
//   * header: "Connect Controller" + controller-type combo | "Input Device" |
//     "Profile" (combo + Save / New / Delete);
//   * body: a grid whose columns are
//       Left Stick + D-Pad │ L/ZL │ Minus/Plus, Capture/Home, controller art │
//       R/ZR │ Face Buttons + Right Stick + Mouse panning;
//   * footer: Console Mode radios, Vibration / Motion toggles + Configure,
//     Motion 1 binding, the Connected-controllers checkbox strip, and
//     Defaults / Clear.
//
// Each binding button shows the currently-mapped host input; clicking one
// starts the same polling and timeout lifecycle as upstream's `HandleClick`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gtk::glib;
use gtk::prelude::*;

use hid_core::hid_core::EmulatedControllerHandle;

use common::settings_input::{native_analog, native_button, ControllerType, PlayerInput};
use input_common::main_common::generate_keyboard_param;

use super::configure_dialog::Page;
use super::input_profiles::InputProfiles;
use super::qt_config::{DEFAULT_ANALOGS, DEFAULT_BUTTONS, DEFAULT_MOTIONS, DEFAULT_STICK_MOD};
use super::shared_widget as w;

pub(crate) struct InputProfileContext {
    profiles: RefCell<InputProfiles>,
    dropdowns: RefCell<Vec<glib::WeakRef<gtk::DropDown>>>,
}

impl InputProfileContext {
    pub(crate) fn new(profiles: InputProfiles) -> Self {
        Self {
            profiles: RefCell::new(profiles),
            dropdowns: RefCell::new(Vec::new()),
        }
    }

    fn register(&self, dropdown: &gtk::DropDown) {
        self.dropdowns.borrow_mut().push(dropdown.downgrade());
    }

    fn refresh_dropdowns(&self) {
        let names = self.profiles.borrow_mut().get_input_profile_names();
        self.dropdowns.borrow_mut().retain(|dropdown| {
            let Some(dropdown) = dropdown.upgrade() else {
                return false;
            };
            let selected = combo_text(&dropdown);
            set_profile_model(&dropdown, &names, selected.as_deref());
            true
        });
    }
}

/// Controller types offered by the header combo — upstream
/// `ConfigureInputPlayer::UpdateControllerAvailableButtons`, in `.ui` order.
const CONTROLLER_TYPES: &[(ControllerType, &str)] = &[
    (ControllerType::ProController, "Pro Controller"),
    (ControllerType::DualJoyconDetached, "Dual Joycons"),
    (ControllerType::LeftJoycon, "Left Joycon"),
    (ControllerType::RightJoycon, "Right Joycon"),
    (ControllerType::Handheld, "Handheld"),
    (ControllerType::GameCube, "GameCube Controller"),
];

/// The label upstream shows for an unmapped binding.
const NOT_SET: &str = "[not set]";

/// What a binding button reads while it waits for an input.
const WAITING: &str = "[waiting]";
/// The same, for a motion binding — upstream asks the user to shake the pad.
const SHAKE: &str = "Shake!";

/// Upstream's `timeout_timer->start(4000)` and `poll_timer->start(25)`.
const CAPTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);
const CAPTURE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

/// Width of a binding button, so the columns line up like the Qt grid.
const BINDING_WIDTH: i32 = 84;

/// Upstream's `layout_show` array: every widget hidden by *some* controller
/// type, re-shown before the per-type hide list is applied.
///
/// The four `spacer_*` entries are upstream's
/// `horizontalSpacerShoulderButtonsWidget` .. `Widget4`. They are expanding
/// widgets sitting between the shoulder groups, and they are hidden and shown
/// alongside them — that is what keeps each remaining group in its own column
/// instead of letting them all bunch up together.
const ALWAYS_SHOWN_GROUPS: &[&str] = &[
    "slsr_left",
    "slsr_right",
    "spacer_1",
    "spacer_2",
    "spacer_3",
    "spacer_4",
    "shoulder_left",
    "minus_screenshot",
    "bottom_left",
    "shoulder_right",
    "plus_home",
    "bottom_right",
    "minus",
    "screenshot",
];

/// Upstream's `shoulderButtons` row, left to right.
///
/// The four `spacer_*` entries are `horizontalSpacerShoulderButtonsWidget` ..
/// `Widget4`; upstream's numbering is not in row order, and it is kept because
/// the show/hide lists name them by number. Minus/Capture and Plus/Home sit
/// side by side with no spacer between them, which is what pairs them in the
/// middle of the drawing.
const SHOULDER_ROW: &[&str] = &[
    "slsr_left",
    "spacer_4",
    "shoulder_left",
    "spacer_1",
    "minus_screenshot",
    "plus_home",
    "spacer_3",
    "shoulder_right",
    "spacer_2",
    "slsr_right",
];

/// Upstream's `layout_enable` array.
const ALWAYS_ENABLED_GROUPS: &[&str] = &["lstick_pressed", "rstick_pressed", "button_l", "home"];

/// Upstream `UpdateControllerAvailableButtons`' `layout_hidden` switch.
fn hidden_groups(layout: ControllerType) -> &'static [&'static str] {
    use ControllerType as C;
    match layout {
        C::ProController | C::Handheld => &["slsr_left", "slsr_right", "spacer_2", "spacer_4"],
        C::LeftJoycon => &[
            "slsr_right",
            "spacer_2",
            "spacer_3",
            "shoulder_right",
            "plus_home",
            "bottom_right",
        ],
        C::RightJoycon => &[
            "slsr_left",
            "spacer_1",
            "spacer_4",
            "shoulder_left",
            "minus_screenshot",
            "bottom_left",
        ],
        C::GameCube => &[
            "slsr_left",
            "slsr_right",
            "spacer_2",
            "spacer_4",
            "minus",
            "screenshot",
        ],
        // Dual Joy-Cons show every group.
        _ => &[],
    }
}

/// Upstream `UpdateControllerEnabledButtons`' `layout_disable` switch.
///
/// A GameCube pad has no home button and no clickable sticks, and its L is an
/// analog trigger rather than a digital button.
fn disabled_groups(layout: ControllerType) -> &'static [&'static str] {
    match layout {
        ControllerType::GameCube => &["home", "lstick_pressed", "rstick_pressed", "button_l"],
        _ => &[],
    }
}

/// Upstream `UpdateMotionButtons`, as `(motion_1_visible, motion_2_visible)`.
fn motion_visibility(layout: ControllerType) -> (bool, bool) {
    use ControllerType as C;
    match layout {
        C::ProController | C::LeftJoycon | C::Handheld => (true, false),
        C::RightJoycon => (false, true),
        C::GameCube => (false, false),
        // Dual Joy-Cons carry a motion sensor in each half.
        _ => (true, true),
    }
}

/// Upstream `UpdateControllerButtonNames`: the GameCube pad relabels half the
/// groups, because its shoulder layout does not line up with the Switch one.
fn group_titles(layout: ControllerType) -> &'static [(&'static str, &'static str)] {
    match layout {
        ControllerType::GameCube => &[
            ("plus", "Start / Pause"),
            ("zl", "L"),
            ("zr", "R"),
            ("r", "Z"),
            ("lstick", "Control Stick"),
            ("rstick", "C-Stick"),
        ],
        _ => &[
            ("plus", "Plus"),
            ("zl", "ZL"),
            ("zr", "ZR"),
            ("r", "R"),
            ("lstick", "Left Stick"),
            ("rstick", "Right Stick"),
        ],
    }
}

/// Build one "Player N" tab — upstream `ConfigureInputPlayer` for index `index`.
/// Everything the page needs to redraw itself after the controller type or the
/// input device changes.
///
/// Upstream keeps the equivalent as members of `ConfigureInputPlayer`
/// (`button_map`, `analog_map_buttons`, `motion_map`, and the `ui->` widget
/// pointers); the page is built by a free function here, so the handles are
/// collected in one struct instead.
struct PlayerPage {
    /// The working copy of the player's configuration. Upstream mutates the
    /// `EmulatedController` directly and only writes it back in `ApplyConfiguration`.
    state: Rc<RefCell<PlayerInput>>,

    /// Binding buttons, by `Settings::NativeButton` index.
    button_widgets: RefCell<Vec<(usize, gtk::Button)>>,
    /// Binding buttons, by `Settings::NativeAnalog` index and direction.
    analog_widgets: RefCell<Vec<(usize, Direction, gtk::Button)>>,
    /// Binding buttons, by `Settings::NativeMotion` index.
    motion_widgets: RefCell<Vec<(usize, gtk::Button)>>,

    /// Group boxes that upstream shows or hides per controller type.
    groups: RefCell<HashMap<&'static str, gtk::Widget>>,
    /// Group titles that upstream renames per controller type.
    titles: RefCell<HashMap<&'static str, gtk::Label>>,

    /// The player's emulated controller, the preview's source of live values.
    controller: RefCell<Option<EmulatedControllerHandle>>,

    /// Controllers put into configuration mode for this page. Player 1 owns
    /// both the Player1 and Handheld controllers upstream.
    configuration_controllers: RefCell<Vec<EmulatedControllerHandle>>,

    /// The input subsystem, for the mapping session a click starts.
    input_subsystem: RefCell<Option<Rc<RefCell<input_common::InputSubsystem>>>>,

    /// The capture in progress, upstream's `input_setter`. `None` when the page
    /// is idle; upstream refuses a second click while its timeout timer runs.
    capture: RefCell<Option<Capture>>,

    /// The per-stick analog controls, whose visibility upstream flips between
    /// deadzone/range and modifier depending on the bound engine.
    stick_controls: RefCell<Vec<StickControls>>,

    /// ZL/ZR threshold sliders, indexed by their native button.
    trigger_controls: RefCell<Vec<TriggerControls>>,

    /// The rows of the Input Device combo, and the one selected, so a captured
    /// input can be filtered the way `IsInputAcceptable` does.
    input_devices: RefCell<Vec<common::param_package::ParamPackage>>,
    selected_device: Cell<usize>,
}

/// The analog controls of one stick.
struct StickControls {
    analog: native_analog::Values,
    deadzone_label: gtk::Label,
    deadzone: gtk::Scale,
    range_block: gtk::Box,
    range: gtk::SpinButton,
    modifier_block: gtk::Box,
    modifier_button: gtk::Button,
    modifier_label: gtk::Label,
    modifier: gtk::Scale,
}

struct TriggerControls {
    button: native_button::Values,
    range: gtk::Scale,
}

/// What a click on a binding button is waiting for — upstream's `input_setter`
/// plus the button it has to relabel.
struct Capture {
    target: CaptureTarget,
    button: gtk::Button,
    /// Deadline, upstream's 4 s `timeout_timer`.
    deadline: std::time::Instant,
}

/// Which binding the captured input will be written to.
#[derive(Clone, Copy)]
enum CaptureTarget {
    Button(usize),
    Analog(usize, Direction),
    AnalogModifier(usize),
    Motion(usize),
}

impl PlayerPage {
    fn new(state: Rc<RefCell<PlayerInput>>) -> Rc<Self> {
        Rc::new(Self {
            state,
            button_widgets: RefCell::new(Vec::new()),
            analog_widgets: RefCell::new(Vec::new()),
            motion_widgets: RefCell::new(Vec::new()),
            groups: RefCell::new(HashMap::new()),
            titles: RefCell::new(HashMap::new()),
            controller: RefCell::new(None),
            configuration_controllers: RefCell::new(Vec::new()),
            input_subsystem: RefCell::new(None),
            capture: RefCell::new(None),
            stick_controls: RefCell::new(Vec::new()),
            trigger_controls: RefCell::new(Vec::new()),
            input_devices: RefCell::new(Vec::new()),
            selected_device: Cell::new(0),
        })
    }

    fn register_group(&self, name: &'static str, widget: &impl IsA<gtk::Widget>) {
        self.groups
            .borrow_mut()
            .insert(name, widget.clone().upcast());
    }

    /// Upstream `ConfigureInputPlayer::UpdateUI`: re-label every binding button
    /// from the current configuration.
    /// Push the working copy into the emulated controller so the preview shows
    /// what the page currently has bound.
    ///
    /// Upstream has no equivalent because its dialog edits the controller
    /// itself; see `EmulatedController::reload_from_player`.
    fn refresh_devices(&self) {
        if let Some(controller) = self.controller.borrow().as_ref() {
            controller.lock().reload_from_player(&self.state.borrow());
        }
    }

    /// Port of the Player 1 branch in upstream's controller-type handler.
    ///
    /// Player 1 owns two HIDCore controllers. Selecting Handheld transfers the
    /// temporary connection to Handheld; selecting any other style transfers it
    /// back to Player1. Both objects retain the selected temporary style.
    fn set_controller_type(&self, controller_type: ControllerType) {
        let npad_type =
            hid_core::frontend::emulated_controller::EmulatedController::map_settings_type_to_npad(
                controller_type,
            );
        let controllers = self.configuration_controllers.borrow();
        if controllers.len() != 2 {
            if let Some(controller) = self.controller.borrow().as_ref() {
                controller.lock().set_npad_style_index(npad_type);
            }
            return;
        }

        let controller_for = |npad_id| {
            controllers
                .iter()
                .find(|controller| controller.lock().get_npad_id_type() == npad_id)
                .cloned()
        };
        let Some(player_one) = controller_for(hid_core::hid_types::NpadIdType::Player1) else {
            return;
        };
        let Some(handheld) = controller_for(hid_core::hid_types::NpadIdType::Handheld) else {
            return;
        };

        let current = self.controller.borrow().as_ref().cloned();
        let is_connected = current
            .as_ref()
            .is_some_and(|controller| controller.lock().is_connected(true));

        player_one.lock().set_npad_style_index(npad_type);
        handheld.lock().set_npad_style_index(npad_type);

        let selected = if is_connected {
            if npad_type == hid_core::hid_types::NpadStyleIndex::Handheld {
                player_one.lock().disconnect();
                handheld.lock().connect(true);
                handheld
            } else {
                handheld.lock().disconnect();
                player_one.lock().connect(true);
                player_one
            }
        } else {
            current.unwrap_or(player_one)
        };
        selected.lock().set_npad_style_index(npad_type);
        *self.controller.borrow_mut() = Some(selected);
    }

    fn update_ui(&self) {
        self.refresh_devices();
        // GTK emits value-changed synchronously from set_value(). Keep no
        // RefCell borrow live while updating widgets, because the matching
        // callbacks write these values back to the working copy.
        let state = self.state.borrow().clone();
        for (index, button) in self.button_widgets.borrow().iter() {
            let text = state
                .buttons
                .get(*index)
                .map(|param| button_to_text(param))
                .unwrap_or_else(|| NOT_SET.to_string());
            button.set_label(&text);
        }
        for (index, direction, button) in self.analog_widgets.borrow().iter() {
            let text = state
                .analogs
                .get(*index)
                .map(|param| analog_to_text(param, *direction))
                .unwrap_or_else(|| NOT_SET.to_string());
            button.set_label(&text);
        }
        for (index, button) in self.motion_widgets.borrow().iter() {
            let text = state
                .motions
                .get(*index)
                .map(|param| button_to_text(param))
                .unwrap_or_else(|| NOT_SET.to_string());
            button.set_label(&text);
        }

        for controls in self.trigger_controls.borrow().iter() {
            let param = state
                .buttons
                .get(controls.button as usize)
                .map(|param| common::param_package::ParamPackage::from_serialized(param))
                .unwrap_or_default();
            if param.has("threshold") {
                controls
                    .range
                    .set_value((param.get_float("threshold", 0.5) * 100.0) as f64);
            }
        }

        // Upstream's tail of `UpdateUI`: a stick bound to a real controller
        // shows its deadzone and range, one made of buttons shows its modifier
        // range instead, and each set is loaded from the binding.
        for controls in self.stick_controls.borrow().iter() {
            let param = state
                .analogs
                .get(controls.analog as usize)
                .map(|param| common::param_package::ParamPackage::from_serialized(param))
                .unwrap_or_default();
            let is_controller = self
                .input_subsystem
                .borrow()
                .as_ref()
                .map(|subsystem| subsystem.borrow().is_controller(&param))
                .unwrap_or(false);

            controls
                .modifier_button
                .set_label(&button_to_text(&param.get_str("modifier", "")));

            if is_controller {
                let deadzone = (param.get_float("deadzone", 0.15) * 100.0) as i32;
                controls
                    .deadzone_label
                    .set_text(&format!("Deadzone: {deadzone}%"));
                controls.deadzone.set_value(deadzone as f64);
                controls
                    .range
                    .set_value((param.get_float("range", 0.95) * 100.0) as f64);
            } else {
                let modifier = (param.get_float("modifier_scale", 0.5) * 100.0) as i32;
                controls
                    .modifier_label
                    .set_text(&format!("Modifier Range: {modifier}%"));
                controls.modifier.set_value(modifier as f64);
            }

            controls.deadzone_label.set_visible(is_controller);
            controls.deadzone.set_visible(is_controller);
            controls.range_block.set_visible(is_controller);
            controls.modifier_block.set_visible(!is_controller);
            controls.modifier_label.set_visible(!is_controller);
            controls.modifier.set_visible(!is_controller);
        }
    }

    /// Upstream `ConfigureInputPlayer::HandleClick`.
    ///
    /// Puts the button into its waiting state, opens a mapping session on every
    /// engine, and polls for the first acceptable input. Upstream refuses a
    /// second click while its timeout timer is running; the same guard here is
    /// `capture` already being set.
    fn handle_click(self: &Rc<Self>, target: CaptureTarget, button: &gtk::Button) {
        if self.capture.borrow().is_some() {
            return;
        }
        let Some(subsystem) = self.input_subsystem.borrow().clone() else {
            return;
        };

        // Upstream shows "Shake!" for a motion binding, "[waiting]" otherwise.
        button.set_label(match target {
            CaptureTarget::Motion(_) => SHAKE,
            _ => WAITING,
        });
        button.grab_focus();

        let input_type = match target {
            CaptureTarget::Button(_) | CaptureTarget::AnalogModifier(_) => {
                input_common::polling::InputType::Button
            }
            CaptureTarget::Analog(..) => input_common::polling::InputType::Stick,
            CaptureTarget::Motion(_) => input_common::polling::InputType::Motion,
        };
        subsystem.borrow_mut().begin_mapping(input_type);

        *self.capture.borrow_mut() = Some(Capture {
            target,
            button: button.clone(),
            deadline: std::time::Instant::now() + CAPTURE_TIMEOUT,
        });

        // Upstream polls every 25 ms and gives up after 4 s.
        let page = Rc::downgrade(self);
        glib::timeout_add_local(CAPTURE_POLL_INTERVAL, move || {
            let Some(page) = page.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let Some(deadline) = page.capture.borrow().as_ref().map(|c| c.deadline) else {
                return glib::ControlFlow::Break;
            };
            if std::time::Instant::now() >= deadline {
                page.finish_capture(None);
                return glib::ControlFlow::Break;
            }
            let params = subsystem.borrow_mut().get_next_input();
            if !params.has("engine") || !page.is_input_acceptable(&params) {
                return glib::ControlFlow::Continue;
            }
            page.finish_capture(Some(params));
            glib::ControlFlow::Break
        });
    }

    /// Upstream `ConfigureInputPlayer::SetPollingResult`. `None` is its `abort`.
    fn finish_capture(&self, params: Option<common::param_package::ParamPackage>) {
        let Some(capture) = self.capture.borrow_mut().take() else {
            return;
        };
        if let Some(subsystem) = self.input_subsystem.borrow().as_ref() {
            subsystem.borrow_mut().stop_mapping();
        }

        if let Some(params) = params {
            let mut state = self.state.borrow_mut();
            match capture.target {
                CaptureTarget::Button(index) => {
                    if let Some(slot) = state.buttons.get_mut(index) {
                        *slot = params.serialize();
                    }
                }
                CaptureTarget::Analog(index, direction) => {
                    if let Some(slot) = state.analogs.get_mut(index) {
                        let mut analog = common::param_package::ParamPackage::from_serialized(slot);
                        set_analog_param(&params, &mut analog, direction);

                        let is_inverted = self
                            .input_subsystem
                            .borrow()
                            .as_ref()
                            .map(|subsystem| subsystem.borrow().is_stick_inverted(&analog))
                            .unwrap_or(false);
                        correct_inverted_stick(&mut analog, index, is_inverted);
                        *slot = analog.serialize();
                    }
                }
                CaptureTarget::AnalogModifier(index) => {
                    if let Some(slot) = state.analogs.get_mut(index) {
                        let mut analog = common::param_package::ParamPackage::from_serialized(slot);
                        analog.set_str("modifier", params.serialize());
                        *slot = analog.serialize();
                    }
                }
                CaptureTarget::Motion(index) => {
                    if let Some(slot) = state.motions.get_mut(index) {
                        *slot = params.serialize();
                    }
                }
            }
        }

        // `update_ui` relabels every button, including the one that was
        // waiting, so an aborted capture restores its previous text.
        let _ = capture.button;
        self.update_ui();
    }

    /// Upstream `ConfigureInputPlayer::IsInputAcceptable`: with a device
    /// selected, only that device's inputs are taken.
    fn is_input_acceptable(&self, params: &common::param_package::ParamPackage) -> bool {
        let selected = self.selected_device.get();
        if selected == 0 || params.has("motion") {
            return true;
        }
        let engine = params.get_str("engine", "");
        // Rows 1 and 2 are "Keyboard Only" and "Keyboard/Mouse".
        if selected == 1 || selected == 2 {
            return engine == "keyboard" || engine == "mouse";
        }
        let devices = self.input_devices.borrow();
        let Some(device) = devices.get(selected) else {
            return true;
        };
        let guid = params.get_str("guid", "");
        engine == device.get_str("engine", "")
            && (guid == device.get_str("guid", "") || guid == device.get_str("guid2", ""))
            && params.get_str("port", "0") == device.get_str("port", "0")
    }

    /// Upstream's right-click "Clear" action on a binding button.
    fn clear_binding(&self, target: CaptureTarget) {
        {
            let mut state = self.state.borrow_mut();
            match target {
                CaptureTarget::Button(index) => {
                    if let Some(slot) = state.buttons.get_mut(index) {
                        *slot = common::param_package::ParamPackage::default().serialize();
                    }
                }
                CaptureTarget::Analog(index, direction) => {
                    if let Some(slot) = state.analogs.get_mut(index) {
                        let mut analog = common::param_package::ParamPackage::from_serialized(slot);
                        if analog.get_str("engine", "") == "analog_from_button" {
                            analog.erase(direction.parameter_key());
                        } else {
                            analog.clear();
                        }
                        *slot = analog.serialize();
                    }
                }
                CaptureTarget::AnalogModifier(index) => {
                    if let Some(slot) = state.analogs.get_mut(index) {
                        let mut analog = common::param_package::ParamPackage::from_serialized(slot);
                        analog.set_str("modifier", String::new());
                        *slot = analog.serialize();
                    }
                }
                CaptureTarget::Motion(index) => {
                    if let Some(slot) = state.motions.get_mut(index) {
                        *slot = common::param_package::ParamPackage::default().serialize();
                    }
                }
            }
        }
        self.update_ui();
    }

    /// Upstream `UpdateControllerAvailableButtons`, `UpdateControllerEnabledButtons`,
    /// `UpdateMotionButtons` and `UpdateControllerButtonNames`, which upstream
    /// always calls together from the controller-type handler.
    ///
    /// The decisions themselves live in the free functions below so they can be
    /// checked without a display.
    fn update_controller_layout(&self, layout: ControllerType) {
        let groups = self.groups.borrow();

        // `layout_show`: upstream un-hides everything, then applies the
        // per-type hide list.
        for name in ALWAYS_SHOWN_GROUPS {
            if let Some(widget) = groups.get(name) {
                widget.set_visible(true);
            }
        }
        for name in hidden_groups(layout) {
            if let Some(widget) = groups.get(name) {
                widget.set_visible(false);
            }
        }

        // `layout_enable` / `layout_disable`.
        for name in ALWAYS_ENABLED_GROUPS {
            if let Some(widget) = groups.get(name) {
                widget.set_sensitive(true);
            }
        }
        for name in disabled_groups(layout) {
            if let Some(widget) = groups.get(name) {
                widget.set_sensitive(false);
            }
        }

        // `UpdateMotionButtons`.
        let (motion_1, motion_2) = motion_visibility(layout);
        if let Some(widget) = groups.get("motion_1") {
            widget.set_visible(motion_1);
        }
        if let Some(widget) = groups.get("motion_2") {
            widget.set_visible(motion_2);
        }

        // `UpdateControllerButtonNames`.
        let titles = self.titles.borrow();
        for (key, text) in group_titles(layout) {
            if let Some(label) = titles.get(key) {
                label.set_text(text);
            }
        }
    }
}

impl Drop for PlayerPage {
    fn drop(&mut self) {
        if self.capture.get_mut().take().is_some() {
            if let Some(subsystem) = self.input_subsystem.get_mut().as_ref() {
                subsystem.borrow_mut().stop_mapping();
            }
        }
        for controller in self.configuration_controllers.get_mut() {
            controller.lock().disable_configuration();
        }
    }
}

/// Build one "Player N" tab — upstream `ConfigureInputPlayer` for index `index`.
pub fn page(
    index: usize,
    input_subsystem: Rc<RefCell<input_common::InputSubsystem>>,
    hid_core: Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>,
    profile_context: Rc<InputProfileContext>,
) -> Page {
    let (controller, mut configuration_controllers) = {
        let hid_core = hid_core.lock();
        if index == 0 {
            let player_one =
                hid_core.get_emulated_controller(hid_core::hid_types::NpadIdType::Player1);
            let handheld =
                hid_core.get_emulated_controller(hid_core::hid_types::NpadIdType::Handheld);
            (player_one, vec![handheld])
        } else {
            (hid_core.get_emulated_controller_by_index(index), Vec::new())
        }
    };

    let (controller, mut configuration_controllers) = if index == 0 {
        let player_one = controller;
        let handheld = configuration_controllers
            .pop()
            .expect("Player 1 configuration must own the Handheld controller");

        player_one.lock().save_current_config();
        player_one.lock().enable_configuration();
        handheld.lock().save_current_config();
        handheld.lock().enable_configuration();

        let selected = if handheld.lock().is_connected(true) {
            player_one.lock().disconnect();
            Arc::clone(&handheld)
        } else {
            Arc::clone(&player_one)
        };
        (selected, vec![player_one, handheld])
    } else {
        controller.lock().save_current_config();
        controller.lock().enable_configuration();
        (Arc::clone(&controller), vec![controller])
    };

    let controller_settings_index =
        hid_core::hid_util::npad_id_type_to_index(controller.lock().get_npad_id_type());
    let state = Rc::new(RefCell::new(player_input(controller_settings_index)));
    let page = PlayerPage::new(Rc::clone(&state));

    // Upstream's `ConfigureInputPlayer` holds the player's
    // `Core::HID::EmulatedController` and hands it to the preview with
    // `ui->controllerFrame->SetController(emulated_controller)`. Keep the
    // stable controller owned by HIDCore; no frontend-local adapter exists.
    controller.lock().reload_from_player(&state.borrow());
    *page.controller.borrow_mut() = Some(Arc::clone(&controller));
    *page.configuration_controllers.borrow_mut() = std::mem::take(&mut configuration_controllers);
    *page.input_subsystem.borrow_mut() = Some(Rc::clone(&input_subsystem));
    let initial_type = state.borrow().controller_type;

    install_group_style();

    let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
    column.set_margin_top(10);
    column.set_margin_bottom(10);
    column.set_margin_start(10);
    column.set_margin_end(10);

    // --- Header -----------------------------------------------------------
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);

    let connect_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let connected = gtk::CheckButton::with_label("Connect Controller");
    connected.set_active(state.borrow().connected);
    let type_labels: Vec<&str> = CONTROLLER_TYPES.iter().map(|(_, l)| *l).collect();
    let controller_type = w::combo(
        &type_labels,
        CONTROLLER_TYPES
            .iter()
            .position(|(t, _)| *t == initial_type)
            .unwrap_or(0) as u32,
    );
    connect_box.append(&connected);
    connect_box.append(&controller_type);
    header.append(&connect_box);

    let device_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    device_box.set_hexpand(true);
    let device_label = gtk::Label::new(Some("Input Device"));
    device_label.set_xalign(0.0);
    device_label.set_valign(gtk::Align::Center);

    // Upstream `UpdateInputDevices`: the combo is filled from
    // `InputSubsystem::GetInputDevices()`, whose first entry is always "Any",
    // followed by "Keyboard Only", "Keyboard/Mouse" and one row per detected
    // pad ("Xbox One Controller 0"). The `ParamPackage`s are kept alongside so
    // a selection can be turned back into a device.
    let input_devices: Vec<common::param_package::ParamPackage> =
        input_subsystem.borrow().get_input_devices();
    let device_names: Vec<String> = input_devices
        .iter()
        .map(|device| device.get_str("display", "Unknown"))
        .collect();
    let device_refs: Vec<&str> = device_names.iter().map(String::as_str).collect();

    // Upstream `UpdateInputDeviceCombobox`: the row is chosen from the device
    // the stored bindings name. With nothing stored yet, select the pad that is
    // plugged in — there is no point starting on "Any" when there is exactly
    // one thing to configure — and fall back to "Any" when none is.
    let stored_index = device_index_for(&state.borrow(), &input_devices);
    let (initial_device, adopt_pad_defaults) = if stored_index != 0 {
        (stored_index, false)
    } else {
        match first_connected_pad(&input_devices) {
            Some(pad) => (pad, true),
            None => (0, false),
        }
    };
    let input_device = w::combo(&device_refs, initial_device as u32);
    // `IsInputAcceptable` filters a captured input against the selected row.
    *page.input_devices.borrow_mut() = input_devices.clone();
    page.selected_device.set(initial_device);
    device_box.append(&device_label);
    device_box.append(&input_device);
    header.append(&device_box);

    // Adopting the pad without its mapping would leave every binding on
    // `[not set]`, so install the same defaults the combo's own handler would
    // have applied had the user picked that row.
    if adopt_pad_defaults {
        if let Some(device) = input_devices.get(initial_device) {
            apply_device_defaults(&page, &input_subsystem.borrow(), device);
        }
    }

    let profile_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let profile_label = gtk::Label::new(Some("Profile"));
    profile_label.set_xalign(0.0);
    profile_label.set_valign(gtk::Align::Center);
    let profile_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let profile_names = profile_context
        .profiles
        .borrow_mut()
        .get_input_profile_names();
    let profile_refs: Vec<&str> = profile_names.iter().map(String::as_str).collect();
    let selected_profile = profile_names
        .iter()
        .position(|name| *name == state.borrow().profile_name);
    let profile = w::combo(&profile_refs, selected_profile.unwrap_or(0) as u32);
    if selected_profile.is_none() {
        profile.set_selected(gtk::INVALID_LIST_POSITION);
    }
    profile_context.register(&profile);
    profile.set_width_request(90);
    let save_profile = gtk::Button::with_label("Save");
    let new_profile = gtk::Button::with_label("New");
    let delete_profile = gtk::Button::with_label("Delete");
    profile_row.append(&profile);
    profile_row.append(&save_profile);
    profile_row.append(&new_profile);
    profile_row.append(&delete_profile);
    profile_box.append(&profile_label);
    profile_box.append(&profile_row);
    header.append(&profile_box);

    // The three header columns each stack a caption over a control. The
    // "Connect Controller" check box is taller than a plain label, which pushed
    // its combo below the other two; a vertical size group makes the caption
    // row one height so the controls beneath line up.
    let header_captions = gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical);
    header_captions.add_widget(&connected);
    header_captions.add_widget(&device_label);
    header_captions.add_widget(&profile_label);

    column.append(&header);

    // --- Body -------------------------------------------------------------
    //
    // Upstream's `horizontalLayout_2`: `bottomLeft`, `bottomMiddle` and
    // `bottomRight` side by side, with only the middle one expanding.
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    body.set_vexpand(true);

    // `bottomLeft`: Left Stick, then D-Pad.
    let bottom_left = gtk::Box::new(gtk::Orientation::Vertical, 6);
    bottom_left.set_valign(gtk::Align::Start);
    bottom_left.append(&stick_group(&page, "lstick", "Left Stick", Stick::Left));
    bottom_left.append(&dpad_group(&page));
    page.register_group("bottom_left", &bottom_left);
    body.append(&bottom_left);

    // `bottomMiddle`: the shoulder row, the controller art, the motion row.
    let centre = gtk::Box::new(gtk::Orientation::Vertical, 6);
    centre.set_hexpand(true);

    // `shoulderButtons`. Every member registers itself as a group and the row
    // is then assembled from `SHOULDER_ROW`, so the order on screen and the
    // order the show/hide lists reason about cannot drift apart.
    let shoulder_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    shoulder_row.set_valign(gtk::Align::Start);

    for name in ["spacer_1", "spacer_2", "spacer_3", "spacer_4"] {
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        page.register_group(name, &spacer);
    }

    let slsr_left = gtk::Box::new(gtk::Orientation::Vertical, 6);
    slsr_left.set_valign(gtk::Align::Start);
    slsr_left.append(&binding_block(&page, "SL", native_button::Values::SLLeft));
    slsr_left.append(&binding_block(&page, "SR", native_button::Values::SRLeft));
    page.register_group("slsr_left", &slsr_left);

    let shoulder_left = gtk::Box::new(gtk::Orientation::Vertical, 6);
    shoulder_left.set_valign(gtk::Align::Start);
    let (l_block, l_title) = titled_binding_block(&page, "L", native_button::Values::L);
    page.register_group("button_l", &l_block);
    let _ = l_title;
    shoulder_left.append(&l_block);
    let (zl_block, zl_title) = trigger_block(&page, "ZL", native_button::Values::ZL);
    page.titles.borrow_mut().insert("zl", zl_title);
    shoulder_left.append(&zl_block);
    page.register_group("shoulder_left", &shoulder_left);

    let minus_screenshot = gtk::Box::new(gtk::Orientation::Vertical, 6);
    minus_screenshot.set_valign(gtk::Align::Start);
    let (minus_block, _) = titled_binding_block(&page, "Minus", native_button::Values::Minus);
    page.register_group("minus", &minus_block);
    let (screenshot_block, _) =
        titled_binding_block(&page, "Capture", native_button::Values::Screenshot);
    page.register_group("screenshot", &screenshot_block);
    minus_screenshot.append(&minus_block);
    minus_screenshot.append(&screenshot_block);
    page.register_group("minus_screenshot", &minus_screenshot);

    let plus_home = gtk::Box::new(gtk::Orientation::Vertical, 6);
    plus_home.set_valign(gtk::Align::Start);
    let (plus_block, plus_title) = titled_binding_block(&page, "Plus", native_button::Values::Plus);
    page.titles.borrow_mut().insert("plus", plus_title);
    let (home_block, _) = titled_binding_block(&page, "Home", native_button::Values::Home);
    page.register_group("home", &home_block);
    plus_home.append(&plus_block);
    plus_home.append(&home_block);
    page.register_group("plus_home", &plus_home);

    let shoulder_right = gtk::Box::new(gtk::Orientation::Vertical, 6);
    shoulder_right.set_valign(gtk::Align::Start);
    let (r_block, r_title) = titled_binding_block(&page, "R", native_button::Values::R);
    page.titles.borrow_mut().insert("r", r_title);
    shoulder_right.append(&r_block);
    let (zr_block, zr_title) = trigger_block(&page, "ZR", native_button::Values::ZR);
    page.titles.borrow_mut().insert("zr", zr_title);
    shoulder_right.append(&zr_block);
    page.register_group("shoulder_right", &shoulder_right);

    let slsr_right = gtk::Box::new(gtk::Orientation::Vertical, 6);
    slsr_right.set_valign(gtk::Align::Start);
    slsr_right.append(&binding_block(&page, "SL", native_button::Values::SLRight));
    slsr_right.append(&binding_block(&page, "SR", native_button::Values::SRRight));
    page.register_group("slsr_right", &slsr_right);

    for name in SHOULDER_ROW {
        let widget =
            page.groups.borrow().get(name).cloned().unwrap_or_else(|| {
                panic!("{name} is part of the shoulder row but was never built")
            });
        shoulder_row.append(&widget);
    }

    centre.append(&shoulder_row);

    // Upstream's `PlayerControlPreview`, rebuilt when the controller type
    // changes (upstream instead tells the one widget which type to draw).
    let preview_holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    preview_holder.set_hexpand(true);
    preview_holder.append(&super::controller_preview::build(
        initial_type,
        Some(Arc::clone(&controller)),
    ));
    centre.append(&preview_holder);

    // Motion 1 / Motion 2 sit under the art, as in the .ui.
    let motion_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    motion_row.set_halign(gtk::Align::Center);
    let motion_1 = motion_block(&page, "Motion 1", 0);
    let motion_2 = motion_block(&page, "Motion 2", 1);
    page.register_group("motion_1", &motion_1);
    page.register_group("motion_2", &motion_2);
    motion_row.append(&motion_1);
    motion_row.append(&motion_2);
    centre.append(&motion_row);

    body.append(&centre);

    // `bottomRight`: Face Buttons, Right Stick, Mouse panning.
    let bottom_right = gtk::Box::new(gtk::Orientation::Vertical, 6);
    bottom_right.set_valign(gtk::Align::Start);
    bottom_right.append(&face_buttons_group(&page));
    bottom_right.append(&stick_group(&page, "rstick", "Right Stick", Stick::Right));
    let panning = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let panning_label = gtk::Label::new(Some("Mouse panning"));
    let configure_panning = gtk::Button::with_label("Configure");
    panning.append(&panning_label);
    panning.append(&configure_panning);
    panning.set_visible(index == 0);
    bottom_right.append(&panning);
    page.register_group("bottom_right", &bottom_right);
    body.append(&bottom_right);

    column.append(&body);

    // --- Footer -----------------------------------------------------------
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    footer.set_valign(gtk::Align::End);

    let console_mode = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let console_label = gtk::Label::new(Some("Console Mode"));
    console_label.set_xalign(0.0);
    let modes = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let docked = gtk::CheckButton::with_label("Docked");
    let handheld = gtk::CheckButton::with_label("Handheld");
    handheld.set_group(Some(&docked));
    docked.set_active(
        *common::settings::values().use_docked_mode.get_value()
            == common::settings_enums::ConsoleMode::Docked,
    );
    modes.append(&docked);
    modes.append(&handheld);
    console_mode.append(&console_label);
    console_mode.append(&modes);
    footer.append(&console_mode);

    let vibration_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let vibration = gtk::CheckButton::with_label("Vibration");
    vibration.set_active(*common::settings::values().vibration_enabled.get_value());
    let configure_vibration = gtk::Button::with_label("Configure");
    vibration_box.append(&vibration);
    vibration_box.append(&configure_vibration);
    footer.append(&vibration_box);

    let motion_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let motion = gtk::CheckButton::with_label("Motion");
    motion.set_active(*common::settings::values().motion_enabled.get_value());
    let configure_motion = gtk::Button::with_label("Configure");
    motion_box.append(&motion);
    motion_box.append(&configure_motion);
    footer.append(&motion_box);

    // "Connected  1 2 3 4 5 6 7 8" over a row of checkboxes.
    let connected_strip = gtk::Grid::new();
    connected_strip.set_column_spacing(4);
    connected_strip.set_hexpand(true);
    connected_strip.set_halign(gtk::Align::Center);
    let connected_label = gtk::Label::new(Some("Connected"));
    connected_label.set_xalign(0.0);
    connected_strip.attach(&connected_label, 0, 0, 1, 1);
    let controllers_label = gtk::Label::new(Some("Controllers"));
    controllers_label.set_xalign(0.0);
    connected_strip.attach(&controllers_label, 0, 1, 1, 1);
    for slot in 0..super::configure_input::NUM_PLAYERS {
        let number = gtk::Label::new(Some(&(slot + 1).to_string()));
        connected_strip.attach(&number, slot as i32 + 1, 0, 1, 1);
        let check = gtk::CheckButton::new();
        check.set_active(player_input(slot).connected);
        // Upstream drives these from the other players' pages; the current
        // player's own box mirrors "Connect Controller" above.
        check.set_sensitive(false);
        connected_strip.attach(&check, slot as i32 + 1, 1, 1, 1);
    }
    footer.append(&connected_strip);

    let actions = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let defaults = gtk::Button::with_label("Defaults");
    let clear = gtk::Button::with_label("Clear");
    actions.append(&defaults);
    actions.append(&clear);
    footer.append(&actions);

    column.append(&footer);

    // --- Behaviour --------------------------------------------------------

    {
        let page = Rc::downgrade(&page);
        connected.connect_toggled(move |check| {
            let Some(page) = page.upgrade() else {
                return;
            };
            let is_connected = check.is_active();
            page.state.borrow_mut().connected = is_connected;
            let controller = page.controller.borrow().as_ref().cloned();
            if let Some(controller) = controller {
                if is_connected {
                    controller.lock().connect(true);
                } else {
                    controller.lock().disconnect();
                }
            }
        });
    }

    // Upstream `UpdateMappingWithDefaults`: selecting a real device wipes the
    // current mapping and refills it from that device's defaults. Row 0 ("Any")
    // is left alone, exactly as upstream's early return does.
    {
        let page = Rc::downgrade(&page);
        let devices = input_devices.clone();
        let subsystem = Rc::clone(&input_subsystem);
        input_device.connect_selected_notify(move |combo| {
            let Some(page) = page.upgrade() else {
                return;
            };
            let selected = combo.selected() as usize;
            page.selected_device.set(selected);
            if selected == 0 {
                return;
            }
            let Some(device) = devices.get(selected) else {
                return;
            };
            apply_device_defaults(&page, &subsystem.borrow(), device);
            page.update_ui();
        });
    }

    // "Clear" empties every binding; "Defaults" re-applies the selected
    // device's mapping, matching upstream's `ClearAll` / `RestoreDefaults`.
    {
        let page = Rc::downgrade(&page);
        clear.connect_clicked(move |_| {
            let Some(page) = page.upgrade() else {
                return;
            };
            {
                let mut state = page.state.borrow_mut();
                state.buttons.iter_mut().for_each(|b| b.clear());
                state.analogs.iter_mut().for_each(|a| a.clear());
                state.motions.iter_mut().for_each(|m| m.clear());
            }
            page.update_ui();
        });
    }
    {
        let page = Rc::downgrade(&page);
        let devices = input_devices.clone();
        let subsystem = Rc::clone(&input_subsystem);
        let input_device = input_device.clone();
        defaults.connect_clicked(move |_| {
            let Some(page) = page.upgrade() else {
                return;
            };
            let selected = input_device.selected() as usize;
            if let Some(device) = devices.get(selected).filter(|_| selected != 0) {
                apply_device_defaults(&page, &subsystem.borrow(), device);
                page.update_ui();
            }
        });
    }

    {
        let page = Rc::downgrade(&page);
        let preview_holder = preview_holder.clone();
        controller_type.connect_selected_notify(move |combo| {
            let Some(page) = page.upgrade() else {
                return;
            };
            let selected = CONTROLLER_TYPES
                .get(combo.selected() as usize)
                .map(|(kind, _)| *kind)
                .unwrap_or(ControllerType::ProController);
            page.state.borrow_mut().controller_type = selected;
            page.set_controller_type(selected);
            while let Some(child) = preview_holder.first_child() {
                preview_holder.remove(&child);
            }
            preview_holder.append(&super::controller_preview::build(
                selected,
                page.controller.borrow().clone(),
            ));
            page.update_controller_layout(selected);
        });
    }

    {
        let page = Rc::downgrade(&page);
        let profile_context = Rc::clone(&profile_context);
        let controller_type = controller_type.clone();
        profile.connect_selected_notify(move |dropdown| {
            let Some(page) = page.upgrade() else {
                return;
            };
            let Some(profile_name) = combo_text(dropdown) else {
                return;
            };
            if !profile_context
                .profiles
                .borrow_mut()
                .load_profile(&profile_name, &mut page.state.borrow_mut())
            {
                show_error(
                    dropdown,
                    "Load Input Profile",
                    &format!("Failed to load the input profile \"{profile_name}\""),
                );
                profile_context.refresh_dropdowns();
                return;
            }

            page.state.borrow_mut().profile_name = profile_name;
            let selected_type = page.state.borrow().controller_type;
            let selected = CONTROLLER_TYPES
                .iter()
                .position(|(controller, _)| *controller == selected_type)
                .unwrap_or(0);
            controller_type.set_selected(selected as u32);
            page.update_ui();
        });
    }

    {
        let page = Rc::downgrade(&page);
        let profile_context = Rc::clone(&profile_context);
        let profile = profile.clone();
        new_profile.connect_clicked(move |button| {
            let page = page.clone();
            let profile_context = Rc::clone(&profile_context);
            let profile = profile.clone();
            let button = button.clone();
            let error_parent = button.clone();
            request_profile_name(&button, move |profile_name| {
                let Some(page) = page.upgrade() else {
                    return;
                };
                if profile_name.is_empty() || !InputProfiles::is_profile_name_valid(&profile_name) {
                    show_error(
                        &error_parent,
                        "Create Input Profile",
                        "The given profile name is not valid.",
                    );
                    return;
                }
                if !profile_context
                    .profiles
                    .borrow_mut()
                    .create_profile(&profile_name, &page.state.borrow())
                {
                    show_error(
                        &error_parent,
                        "Create Input Profile",
                        &format!("Failed to create the input profile \"{profile_name}\""),
                    );
                    profile_context.refresh_dropdowns();
                    return;
                }
                page.state.borrow_mut().profile_name = profile_name.clone();
                profile_context.refresh_dropdowns();
                select_profile(&profile, &profile_name);
            });
        });
    }

    {
        let page = Rc::downgrade(&page);
        let profile_context = Rc::clone(&profile_context);
        let profile = profile.clone();
        save_profile.connect_clicked(move |button| {
            let Some(page) = page.upgrade() else {
                return;
            };
            let Some(profile_name) = combo_text(&profile) else {
                return;
            };
            if !profile_context
                .profiles
                .borrow()
                .save_profile(&profile_name, &page.state.borrow())
            {
                show_error(
                    button,
                    "Save Input Profile",
                    &format!("Failed to save the input profile \"{profile_name}\""),
                );
            }
        });
    }

    {
        let page = Rc::downgrade(&page);
        let profile_context = Rc::clone(&profile_context);
        let profile = profile.clone();
        delete_profile.connect_clicked(move |button| {
            let Some(profile_name) = combo_text(&profile) else {
                return;
            };
            if !profile_context
                .profiles
                .borrow_mut()
                .delete_profile(&profile_name)
            {
                show_error(
                    button,
                    "Delete Input Profile",
                    &format!("Failed to delete the input profile \"{profile_name}\""),
                );
                return;
            }
            if let Some(page) = page.upgrade() {
                let is_current_profile = page.state.borrow().profile_name == profile_name;
                if is_current_profile {
                    page.state.borrow_mut().profile_name.clear();
                }
            }
            profile_context.refresh_dropdowns();
        });
    }

    {
        let page = Rc::downgrade(&page);
        let input_subsystem = Rc::clone(&input_subsystem);
        configure_panning.connect_clicked(move |button| {
            let Some(page) = page.upgrade() else {
                return;
            };
            let right_stick =
                page.state.borrow().analogs[native_analog::Values::RStick as usize].clone();
            let right_stick = common::param_package::ParamPackage::from_serialized(&right_stick);
            let deadzone = right_stick.get_float("deadzone", 0.0);
            let range = right_stick.get_float("range", 1.0);
            super::configure_mouse_panning::present(
                button,
                Rc::clone(&input_subsystem),
                deadzone,
                range,
            );
        });
    }

    {
        let hid_core = Arc::clone(&hid_core);
        configure_vibration.connect_clicked(move |button| {
            super::configure_vibration::present(button, Arc::clone(&hid_core));
        });
    }
    {
        let input_subsystem = Rc::clone(&input_subsystem);
        configure_motion.connect_clicked(move |button| {
            super::configure_motion_touch::present(button, Rc::clone(&input_subsystem));
        });
    }

    // Paint the initial state: labels first, then the per-type layout.
    page.update_ui();
    page.update_controller_layout(initial_type);

    // The grid is dense enough that a narrow dialog would otherwise force the
    // window taller than the screen; scrolling keeps the button row reachable.
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .hexpand(true)
        .vexpand(true)
        .propagate_natural_width(false)
        .propagate_natural_height(false)
        .child(&column)
        .build();

    // Upstream owns ConfigureInputPlayer for as long as its QWidget exists.
    // `Page` owns this apply closure for the same lifetime, so keep the Rust
    // controller here while widget callbacks retain only Weak references.
    let page_owner = Rc::clone(&page);
    Page::new(&format!("Player {}", index + 1), scroller, move || {
        // Widgets hold only a weak reference to their size group, so it has to
        // stay owned for the page's lifetime.
        let _keep_alive = &header_captions;

        let is_connected = connected.is_active();
        let selected_controller_type = CONTROLLER_TYPES
            .get(controller_type.selected() as usize)
            .map(|(t, _)| *t)
            .unwrap_or(ControllerType::ProController);
        let vibrates = vibration.is_active();
        let uses_motion = motion.is_active();
        let is_docked = docked.is_active();

        page_owner.refresh_devices();
        if let Some(controller) = page_owner.controller.borrow().as_ref() {
            let mut controller = controller.lock();
            controller.set_npad_style_index(
                hid_core::frontend::emulated_controller::EmulatedController::map_settings_type_to_npad(
                    selected_controller_type,
                ),
            );
            if is_connected {
                controller.connect(true);
            } else {
                controller.disconnect();
            }
        }
        for controller in page_owner.configuration_controllers.borrow().iter() {
            let mut controller = controller.lock();
            controller.disable_configuration();
            controller.save_current_config();
            controller.enable_configuration();
        }

        {
            let mut values = common::settings::values_mut();
            let players = values.players.get_value_mut();
            if let Some(slot) = players.get_mut(controller_settings_index) {
                let edited = page_owner.state.borrow();
                slot.profile_name = edited.profile_name.clone();
            }
            values.vibration_enabled.set_value(vibrates);
            values.motion_enabled.set_value(uses_motion);
            values.use_docked_mode.set_value(if is_docked {
                common::settings_enums::ConsoleMode::Docked
            } else {
                common::settings_enums::ConsoleMode::Handheld
            });
        }
    })
}

/// Upstream `ConfigureInputPlayer::UpdateMappingWithDefaults`.
///
/// Clears the current mapping, then writes the device's own defaults —
/// `GetButtonMappingForDevice` walks SDL's game-controller bindings, so an
/// Xbox pad comes back with `Button 0`, `Axis 1+` and so on rather than
/// `[not set]`.
/// One entry of upstream `EmulatedController::GetMappedDevices`: the device a
/// stored binding names, reduced to what identifies it.
#[derive(Clone, PartialEq, Eq, Debug)]
struct MappedDevice {
    engine: String,
    guid: String,
    port: String,
    pad: String,
}

impl MappedDevice {
    /// Upstream compares a mapped device against a row of the combo on these
    /// four keys.
    fn matches(&self, device: &common::param_package::ParamPackage) -> bool {
        self.engine == device.get_str("engine", "")
            && self.guid == device.get_str("guid", "")
            && self.port == device.get_str("port", "0")
            && self.pad == device.get_str("pad", "0")
    }
}

/// Upstream `EmulatedController::GetMappedDevices`: the distinct devices the
/// player's bindings refer to, buttons first, then sticks.
///
/// Upstream skips `analog_from_button` sticks, because those are made of button
/// bindings that were already counted.
fn mapped_devices(player: &PlayerInput) -> Vec<MappedDevice> {
    let mut devices: Vec<MappedDevice> = Vec::new();
    let params = player
        .buttons
        .iter()
        .map(|param| (param, false))
        .chain(player.analogs.iter().map(|param| (param, true)));

    for (param, is_stick) in params {
        let param = common::param_package::ParamPackage::from_serialized(param);
        let engine = param.get_str("engine", "");
        if engine.is_empty() {
            continue;
        }
        if is_stick && engine == "analog_from_button" {
            continue;
        }
        let device = MappedDevice {
            engine,
            guid: param.get_str("guid", ""),
            port: param.get_str("port", "0"),
            pad: param.get_str("pad", "0"),
        };
        if !devices.contains(&device) {
            devices.push(device);
        }
    }
    devices
}

/// Upstream `ConfigureInputPlayer::UpdateInputDeviceCombobox`: which row of the
/// Input Device combo the player's stored bindings imply.
///
/// Returns 0 ("Any") when the bindings span more than two devices, or when the
/// device they name is not plugged in — upstream's fallbacks.
fn device_index_for(
    player: &PlayerInput,
    devices: &[common::param_package::ParamPackage],
) -> usize {
    let mapped = mapped_devices(player);
    let index_of = |wanted: &MappedDevice| {
        devices
            .iter()
            .position(|device| wanted.matches(device))
            .unwrap_or(0)
    };

    match mapped.len() {
        0 => 0,
        1 => index_of(&mapped[0]),
        2 => {
            let (first, second) = (&mapped[0], &mapped[1]);
            let is_keyboard_mouse = |engine: &str| engine == "keyboard" || engine == "mouse";
            if is_keyboard_mouse(&first.engine) && is_keyboard_mouse(&second.engine) {
                // Upstream's row 2, "Keyboard/Mouse".
                return 2;
            }
            if first.engine != second.engine || first.port != second.port {
                return 0;
            }
            // A pair of Joy-Cons on one port: the row carries both guids, in
            // either order.
            devices
                .iter()
                .position(|device| {
                    let guid = device.get_str("guid", "");
                    let guid2 = device.get_str("guid2", "");
                    let paired = (guid == first.guid && guid2 == second.guid)
                        || (guid == second.guid && guid2 == first.guid);
                    device.get_str("engine", "") == first.engine
                        && paired
                        && device.get_str("port", "0") == first.port
                })
                .unwrap_or(0)
        }
        _ => 0,
    }
}

/// The first row of the combo that is a real pad rather than "Any", "Keyboard
/// Only" or "Keyboard/Mouse".
///
/// Upstream leaves an unmapped player on "Any" and waits for the user to pick a
/// device. Picking the pad that is actually plugged in saves that step, and
/// falls back to "Any" when there is none.
fn first_connected_pad(devices: &[common::param_package::ParamPackage]) -> Option<usize> {
    devices.iter().position(|device| {
        let engine = device.get_str("engine", "");
        engine != "any" && engine != "keyboard" && engine != "mouse"
    })
}

fn apply_device_defaults(
    page: &PlayerPage,
    subsystem: &input_common::InputSubsystem,
    device: &common::param_package::ParamPackage,
) {
    let mut state = page.state.borrow_mut();
    state.buttons.iter_mut().for_each(|b| b.clear());
    state.analogs.iter_mut().for_each(|a| a.clear());
    state.motions.iter_mut().for_each(|m| m.clear());

    let engine = device.get_str("engine", "");
    if engine == "keyboard" || engine == "mouse" {
        apply_keyboard_defaults(&mut state);
        // Upstream's "Keyboard Only" row returns here. "Keyboard/Mouse"
        // continues so Mouse::GetAnalogMappingForDevice can replace RStick.
        if engine == "keyboard" {
            return;
        }
    }

    let buttons = subsystem.get_button_mapping_for_device(device);
    let analogs = subsystem.get_analog_mapping_for_device(device);
    let motions = subsystem.get_motion_mapping_for_device(device);

    for (index, param) in buttons {
        if let Some(slot) = state.buttons.get_mut(index as usize) {
            *slot = param.serialize();
        }
    }
    for (index, param) in analogs {
        if let Some(slot) = state.analogs.get_mut(index as usize) {
            *slot = param.serialize();
        }
    }
    for (index, param) in motions {
        if let Some(slot) = state.motions.get_mut(index as usize) {
            *slot = param.serialize();
        }
    }
}

/// The keyboard half of upstream
/// `ConfigureInputPlayer::UpdateMappingWithDefaults`.
fn apply_keyboard_defaults(player: &mut PlayerInput) {
    for (slot, key) in DEFAULT_BUTTONS.iter().copied().enumerate() {
        player.buttons[slot] = generate_keyboard_param(key);
    }
    for (slot, keys) in DEFAULT_ANALOGS.iter().copied().enumerate() {
        let mut analog = common::param_package::ParamPackage::default();
        for (key, direction) in keys.into_iter().zip([
            Direction::Up,
            Direction::Down,
            Direction::Left,
            Direction::Right,
        ]) {
            let input =
                common::param_package::ParamPackage::from_serialized(&generate_keyboard_param(key));
            set_analog_param(&input, &mut analog, direction);
        }
        analog.set_str("modifier", generate_keyboard_param(DEFAULT_STICK_MOD[slot]));
        player.analogs[slot] = analog.serialize();
    }
    for (slot, key) in DEFAULT_MOTIONS.iter().copied().enumerate() {
        player.motions[slot] = generate_keyboard_param(key);
    }
}

/// Give the group frames the light fill Qt's Fusion style paints behind a
/// `QGroupBox`, so the binding clusters read as boxes rather than bare lines.
fn install_group_style() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let provider = gtk::CssProvider::new();
        provider.load_from_data(
            "frame.input-group > border { \
                 background-color: alpha(currentColor, 0.06); \
                 border-radius: 4px; \
             }",
        );
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    });
}

/// Which analog stick a [`stick_group`] renders.
#[derive(Clone, Copy)]
enum Stick {
    Left,
    Right,
}

/// Read player `index`'s stored input configuration.
fn player_input(index: usize) -> PlayerInput {
    common::settings::values()
        .players
        .get_value()
        .get(index)
        .cloned()
        .unwrap_or_default()
}

fn combo_text(dropdown: &gtk::DropDown) -> Option<String> {
    dropdown
        .model()
        .and_downcast::<gtk::StringList>()
        .and_then(|list| list.string(dropdown.selected()))
        .map(|text| text.to_string())
}

fn set_profile_model(dropdown: &gtk::DropDown, names: &[String], selected: Option<&str>) {
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    dropdown.set_model(Some(&gtk::StringList::new(&refs)));
    let selected = selected
        .and_then(|selected| names.iter().position(|name| name == selected))
        .map(|index| index as u32)
        .unwrap_or(gtk::INVALID_LIST_POSITION);
    dropdown.set_selected(selected);
}

fn select_profile(dropdown: &gtk::DropDown, profile_name: &str) {
    let Some(model) = dropdown.model().and_downcast::<gtk::StringList>() else {
        return;
    };
    let selected = (0..model.n_items())
        .find(|index| model.string(*index).as_deref() == Some(profile_name))
        .unwrap_or(gtk::INVALID_LIST_POSITION);
    dropdown.set_selected(selected);
}

fn show_error(source: &impl IsA<gtk::Widget>, message: &str, detail: &str) {
    let parent = source.root().and_downcast::<gtk::Window>();
    crate::gtk_compat::show_message(parent.as_ref(), message, detail);
}

fn request_profile_name(source: &impl IsA<gtk::Widget>, on_accept: impl FnOnce(String) + 'static) {
    let window = gtk::Window::builder()
        .title("New Profile")
        .modal(true)
        .resizable(false)
        .default_width(360)
        .build();
    if let Some(parent) = source.root().and_downcast::<gtk::Window>() {
        window.set_transient_for(Some(&parent));
    }

    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_spacing(8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    let label = gtk::Label::new(Some("Enter a profile name:"));
    label.set_xalign(0.0);
    content.append(&label);

    let entry = gtk::Entry::new();
    entry.set_max_length(30);
    content.append(&entry);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let create = gtk::Button::with_label("Create");
    create.add_css_class("suggested-action");
    actions.append(&cancel);
    actions.append(&create);
    content.append(&actions);
    window.set_child(Some(&content));

    let callback = Rc::new(RefCell::new(Some(on_accept)));
    {
        let window = window.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }
    {
        let callback = Rc::clone(&callback);
        let entry = entry.clone();
        let window = window.downgrade();
        create.connect_clicked(move |_| {
            if let Some(callback) = callback.borrow_mut().take() {
                callback(entry.text().to_string());
            }
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }
    {
        let callback = Rc::clone(&callback);
        let window = window.downgrade();
        entry.connect_activate(move |entry| {
            if let Some(callback) = callback.borrow_mut().take() {
                callback(entry.text().to_string());
            }
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }

    window.present();
    entry.grab_focus();
}

/// Render an engine mapping string the way upstream's `ButtonToText` does.
///
/// The stored form is a comma-separated `key:value` list; the displayed form is
/// `"Button N"` / `"Axis N±"` / `"Hat N Direction"` depending on which keys are
/// present. Anything unrecognised falls back to `[not set]`, matching upstream's
/// `if (!param.Has("engine")) return tr("[not set]")`.
pub fn button_to_text(param: &str) -> String {
    let fields: std::collections::HashMap<&str, &str> = param
        .split(',')
        .filter_map(|pair| pair.split_once(':'))
        .collect();

    if !fields.contains_key("engine") {
        return NOT_SET.to_string();
    }

    // Upstream handles keyboard bindings before asking the input driver for a
    // common button name. Qt's QKeySequence renders the stored Qt::Key value;
    // the GTK frontend keeps those same values for config compatibility.
    if fields.get("engine") == Some(&"keyboard") {
        let enabled = |name: &str| fields.get(name).is_some_and(|value| *value == "true");
        let turbo = if enabled("turbo") { "$" } else { "" };
        let toggle = if enabled("toggle") { "~" } else { "" };
        let inverted = if enabled("inverted") { "!" } else { "" };
        let code = fields
            .get("code")
            .and_then(|code| code.parse::<i32>().ok())
            .unwrap_or(0);
        return format!("{turbo}{toggle}{inverted}{}", qt_key_name(code));
    }

    if let Some(button) = fields.get("button") {
        return format!("Button {button}");
    }
    if let Some(axis) = fields.get("axis") {
        // `direction` is "+" or "-" upstream; a missing one means the whole axis.
        let direction = fields.get("direction").copied().unwrap_or("");
        return format!("Axis {axis}{direction}");
    }
    if let Some(hat) = fields.get("hat") {
        let direction = fields.get("direction").copied().unwrap_or("");
        return format!("Hat {hat} {direction}");
    }
    NOT_SET.to_string()
}

/// GTK counterpart of upstream's file-local `GetKeyName`.
fn qt_key_name(key_code: i32) -> String {
    const QT_KEY_ESCAPE: i32 = 0x0100_0000;
    const QT_KEY_TAB: i32 = 0x0100_0001;
    const QT_KEY_BACKTAB: i32 = 0x0100_0002;
    const QT_KEY_BACKSPACE: i32 = 0x0100_0003;
    const QT_KEY_RETURN: i32 = 0x0100_0004;
    const QT_KEY_ENTER: i32 = 0x0100_0005;
    const QT_KEY_INSERT: i32 = 0x0100_0006;
    const QT_KEY_DELETE: i32 = 0x0100_0007;
    const QT_KEY_PAUSE: i32 = 0x0100_0008;
    const QT_KEY_PRINT: i32 = 0x0100_0009;
    const QT_KEY_HOME: i32 = 0x0100_0010;
    const QT_KEY_END: i32 = 0x0100_0011;
    const QT_KEY_LEFT: i32 = 0x0100_0012;
    const QT_KEY_UP: i32 = 0x0100_0013;
    const QT_KEY_RIGHT: i32 = 0x0100_0014;
    const QT_KEY_DOWN: i32 = 0x0100_0015;
    const QT_KEY_PAGE_UP: i32 = 0x0100_0016;
    const QT_KEY_PAGE_DOWN: i32 = 0x0100_0017;
    const QT_KEY_SHIFT: i32 = 0x0100_0020;
    const QT_KEY_CONTROL: i32 = 0x0100_0021;
    const QT_KEY_META: i32 = 0x0100_0022;
    const QT_KEY_ALT: i32 = 0x0100_0023;
    const QT_KEY_CAPS_LOCK: i32 = 0x0100_0024;
    const QT_KEY_NUM_LOCK: i32 = 0x0100_0025;
    const QT_KEY_SCROLL_LOCK: i32 = 0x0100_0026;
    const QT_KEY_F1: i32 = 0x0100_0030;
    const QT_KEY_F35: i32 = 0x0100_0052;

    let name = match key_code {
        QT_KEY_ESCAPE => "Esc",
        QT_KEY_TAB => "Tab",
        QT_KEY_BACKTAB => "Backtab",
        QT_KEY_BACKSPACE => "Backspace",
        QT_KEY_RETURN => "Return",
        QT_KEY_ENTER => "Enter",
        QT_KEY_INSERT => "Ins",
        QT_KEY_DELETE => "Del",
        QT_KEY_PAUSE => "Pause",
        QT_KEY_PRINT => "Print",
        QT_KEY_HOME => "Home",
        QT_KEY_END => "End",
        QT_KEY_LEFT => "Left",
        QT_KEY_UP => "Up",
        QT_KEY_RIGHT => "Right",
        QT_KEY_DOWN => "Down",
        QT_KEY_PAGE_UP => "PgUp",
        QT_KEY_PAGE_DOWN => "PgDown",
        QT_KEY_SHIFT => "Shift",
        QT_KEY_CONTROL => "Ctrl",
        QT_KEY_META => "",
        QT_KEY_ALT => "Alt",
        QT_KEY_CAPS_LOCK => "CapsLock",
        QT_KEY_NUM_LOCK => "NumLock",
        QT_KEY_SCROLL_LOCK => "ScrollLock",
        _ => {
            if (QT_KEY_F1..=QT_KEY_F35).contains(&key_code) {
                return format!("F{}", key_code - QT_KEY_F1 + 1);
            }
            return char::from_u32(key_code as u32)
                .filter(|key| !key.is_control())
                .map(|key| key.to_string())
                .unwrap_or_default();
        }
    };
    name.to_string()
}

/// A [`shared_widget::group`] with its padding trimmed.
///
/// The binding grid stacks two groups per column plus a header and a footer;
/// at the default group padding that overflows the dialog height that upstream's
/// `adjustSize()` settles on, so the whole page would scroll. Qt's grid is
/// tighter than GTK's defaults, and this recovers the difference.
fn compact_group(title: &str) -> (gtk::Box, gtk::Box, gtk::Label) {
    let (outer, content) = w::group(title);
    outer.set_margin_bottom(2);
    content.set_spacing(2);
    content.set_margin_top(4);
    content.set_margin_bottom(4);

    // `w::group` puts the caption first and the frame second; both handles are
    // needed here — the caption to rename per controller type, the frame to
    // carry the Fusion-style fill.
    let caption = outer
        .first_child()
        .and_then(|child| child.downcast::<gtk::Label>().ok())
        .unwrap_or_else(|| gtk::Label::new(Some(title)));
    if let Some(frame) = content.parent() {
        frame.add_css_class("input-group");
    }

    (outer, content, caption)
}

/// One of the four directions of an analog stick.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl Direction {
    fn parameter_key(self) -> &'static str {
        match self {
            Direction::Up => "up",
            Direction::Down => "down",
            Direction::Left => "left",
            Direction::Right => "right",
        }
    }

    /// The axis (`x` / `y`) this direction moves along, and its sign.
    fn axis_and_sign(self) -> (&'static str, &'static str) {
        match self {
            Direction::Up => ("y", "+"),
            Direction::Down => ("y", "-"),
            Direction::Left => ("x", "-"),
            Direction::Right => ("x", "+"),
        }
    }
}

/// Render one direction of a stick mapping — upstream's `AnalogToText`.
///
/// A stick param binds both axes at once
/// (`"engine:sdl,axis_x:0,axis_y:1,..."`), so each of the four direction
/// buttons displays its own axis with the sign that direction moves in:
/// left stick "Up" shows `Axis 1+`, "Left" shows `Axis 0-`.
///
/// A stick can also be bound button-per-direction, in which case the param
/// carries `up`/`down`/`left`/`right` sub-params; upstream then recurses into
/// `ButtonToText`. Anything else renders `[not set]`.
pub fn analog_to_text(param: &str, direction: Direction) -> String {
    let param = common::param_package::ParamPackage::from_serialized(param);
    if !param.has("engine") {
        return NOT_SET.to_string();
    }

    if param.get_str("engine", "") == "analog_from_button" {
        return button_to_text(&param.get_str(direction.parameter_key(), ""));
    }

    if !param.has("axis_x") || !param.has("axis_y") {
        return "[unknown]".to_string();
    }

    let (axis, sign) = direction.axis_and_sign();
    let inverted = param.get_str(&format!("invert_{axis}"), "+") == "-";
    let sign = if inverted {
        if sign == "+" {
            "-"
        } else {
            "+"
        }
    } else {
        sign
    };
    format!("Axis {}{sign}", param.get_str(&format!("axis_{axis}"), ""))
}

/// Upstream's file-local `SetAnalogParam`.
fn set_analog_param(
    input_param: &common::param_package::ParamPackage,
    analog_param: &mut common::param_package::ParamPackage,
    direction: Direction,
) {
    if input_param.has("axis_x") && input_param.has("axis_y") {
        *analog_param = input_param.clone();
        return;
    }

    if !analog_param.has("engine") || analog_param.has("axis_x") || analog_param.has("axis_y") {
        *analog_param =
            common::param_package::ParamPackage::from_pairs([("engine", "analog_from_button")]);
    }
    analog_param.set_str(direction.parameter_key(), input_param.serialize());
}

/// Upstream's post-`SetAnalogParam` correction for drivers that report their
/// stick axes in the opposite order.
fn correct_inverted_stick(
    analog_param: &mut common::param_package::ParamPackage,
    analog_index: usize,
    is_inverted: bool,
) {
    if !is_inverted {
        return;
    }

    let key = match analog_index {
        index if index == native_analog::Values::LStick as usize => "invert_x",
        index if index == native_analog::Values::RStick as usize => "invert_y",
        _ => return,
    };
    let value = if analog_param.get_str(key, "+") == "-" {
        "+"
    } else {
        "-"
    };
    analog_param.set_str(key, value.to_string());
}

/// A `label` over a binding button — the unit the whole grid is built from.
///
/// The button is registered with `page` so `update_ui` can re-label it when the
/// mapping changes, the way upstream keeps every binding button in `button_map`.
fn binding_block(page: &Rc<PlayerPage>, label: &str, button: native_button::Values) -> gtk::Box {
    let (block, _) = titled_binding_block(page, label, button);
    block
}

/// Make a binding button capture an input on click and clear it on right-click.
///
/// Upstream connects `QPushButton::clicked` to `HandleClick` and gives the
/// button a custom context menu whose first entry is "Clear".
fn attach_capture(page: &Rc<PlayerPage>, widget: &gtk::Button, target: CaptureTarget) {
    {
        let page = Rc::downgrade(page);
        widget.connect_clicked(move |button| {
            if let Some(page) = page.upgrade() {
                page.handle_click(target, button);
            }
        });
    }

    let gesture = gtk::GestureClick::new();
    gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
    {
        let page = Rc::downgrade(page);
        gesture.connect_pressed(move |_, _, _, _| {
            if let Some(page) = page.upgrade() {
                page.clear_binding(target);
            }
        });
    }
    widget.add_controller(gesture);
}

/// [`binding_block`], also handing back the caption so callers that rename it
/// per controller type (Plus → "Start / Pause", R → "Z") can keep the handle.
fn titled_binding_block(
    page: &Rc<PlayerPage>,
    label: &str,
    button: native_button::Values,
) -> (gtk::Box, gtk::Label) {
    let block = gtk::Box::new(gtk::Orientation::Vertical, 2);
    block.set_halign(gtk::Align::Center);

    let caption = gtk::Label::new(Some(label));
    let widget = gtk::Button::with_label(NOT_SET);
    widget.set_width_request(BINDING_WIDTH);
    page.button_widgets
        .borrow_mut()
        .push((button as usize, widget.clone()));
    attach_capture(page, &widget, CaptureTarget::Button(button as usize));

    block.append(&caption);
    block.append(&widget);
    (block, caption)
}

/// One direction of a stick, bound to `analog` rather than a button.
fn analog_binding_block(
    page: &Rc<PlayerPage>,
    label: &str,
    analog: native_analog::Values,
    direction: Direction,
) -> gtk::Box {
    let block = gtk::Box::new(gtk::Orientation::Vertical, 2);
    block.set_halign(gtk::Align::Center);

    let caption = gtk::Label::new(Some(label));
    let widget = gtk::Button::with_label(NOT_SET);
    widget.set_width_request(BINDING_WIDTH);
    page.analog_widgets
        .borrow_mut()
        .push((analog as usize, direction, widget.clone()));
    attach_capture(
        page,
        &widget,
        CaptureTarget::Analog(analog as usize, direction),
    );

    block.append(&caption);
    block.append(&widget);
    block
}

/// A "Motion N" block, bound to `Settings::NativeMotion` index `motion`.
fn motion_block(page: &Rc<PlayerPage>, label: &str, motion: usize) -> gtk::Box {
    let block = gtk::Box::new(gtk::Orientation::Vertical, 2);
    block.set_halign(gtk::Align::Center);

    let caption = gtk::Label::new(Some(label));
    let widget = gtk::Button::with_label(NOT_SET);
    widget.set_width_request(BINDING_WIDTH);
    page.motion_widgets
        .borrow_mut()
        .push((motion, widget.clone()));
    attach_capture(page, &widget, CaptureTarget::Motion(motion));

    block.append(&caption);
    block.append(&widget);
    block
}

/// A trigger block: binding button plus the analog-range slider beneath it,
/// as ZL / ZR carry in `configure_input_player.ui`.
fn trigger_block(
    page: &Rc<PlayerPage>,
    label: &str,
    button: native_button::Values,
) -> (gtk::Box, gtk::Label) {
    let (block, caption) = titled_binding_block(page, label, button);
    let range = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    range.set_draw_value(false);
    let initial_threshold = page
        .state
        .borrow()
        .buttons
        .get(button as usize)
        .map(|param| common::param_package::ParamPackage::from_serialized(param))
        .filter(|param| param.has("threshold"))
        .map(|param| param.get_float("threshold", 0.5) * 100.0)
        .unwrap_or(50.0);
    range.set_value(initial_threshold as f64);
    range.set_width_request(BINDING_WIDTH);
    {
        let page = Rc::downgrade(page);
        range.connect_value_changed(move |scale| {
            let Some(page) = page.upgrade() else {
                return;
            };
            let mut state = page.state.borrow_mut();
            let Some(slot) = state.buttons.get_mut(button as usize) else {
                return;
            };
            let mut param = common::param_package::ParamPackage::from_serialized(slot);
            // Upstream only changes a threshold supplied by an axis mapping.
            if !param.has("threshold") {
                return;
            }
            param.set_float("threshold", scale.value() as f32 / 100.0);
            *slot = param.serialize();
            drop(state);
            page.refresh_devices();
        });
    }
    page.trigger_controls.borrow_mut().push(TriggerControls {
        button,
        range: range.clone(),
    });
    block.append(&range);
    (block, caption)
}

/// The Left/Right Stick group: four directions, the press binding, a modifier
/// range spin box, and the deadzone slider.
fn stick_group(page: &Rc<PlayerPage>, key: &'static str, title: &str, stick: Stick) -> gtk::Box {
    let (outer, content, caption) = compact_group(title);
    page.titles.borrow_mut().insert(key, caption);

    let (analog, pressed, pressed_key) = match stick {
        Stick::Left => (
            native_analog::Values::LStick,
            native_button::Values::LStick,
            "lstick_pressed",
        ),
        Stick::Right => (
            native_analog::Values::RStick,
            native_button::Values::RStick,
            "rstick_pressed",
        ),
    };

    content.append(&analog_binding_block(page, "Up", analog, Direction::Up));

    let middle = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    middle.set_halign(gtk::Align::Center);
    middle.append(&analog_binding_block(page, "Left", analog, Direction::Left));
    middle.append(&analog_binding_block(
        page,
        "Right",
        analog,
        Direction::Right,
    ));
    content.append(&middle);

    content.append(&analog_binding_block(page, "Down", analog, Direction::Down));

    let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bottom.set_halign(gtk::Align::Center);
    let (pressed_block, _) = titled_binding_block(page, "Pressed", pressed);
    page.register_group(pressed_key, &pressed_block);
    bottom.append(&pressed_block);

    let modifier_block = gtk::Box::new(gtk::Orientation::Vertical, 2);
    modifier_block.set_halign(gtk::Align::Center);
    modifier_block.append(&gtk::Label::new(Some("Modifier")));
    let modifier_button = gtk::Button::with_label(NOT_SET);
    modifier_button.set_width_request(BINDING_WIDTH);
    attach_capture(
        page,
        &modifier_button,
        CaptureTarget::AnalogModifier(analog as usize),
    );
    modifier_block.append(&modifier_button);
    bottom.append(&modifier_block);

    let range_block = gtk::Box::new(gtk::Orientation::Vertical, 2);
    range_block.set_halign(gtk::Align::Center);
    range_block.append(&gtk::Label::new(Some("Range")));
    let range = gtk::SpinButton::with_range(50.0, 150.0, 1.0);
    range.set_value(stick_property(page, analog, "range", 0.95) * 100.0);
    {
        // Upstream: `param.Set("range", spinbox_value / 100.0f)`.
        let page = Rc::downgrade(page);
        range.connect_value_changed(move |spin| {
            if let Some(page) = page.upgrade() {
                set_stick_property(&page, analog, "range", spin.value() as f32 / 100.0);
            }
        });
    }
    range_block.append(&range);
    bottom.append(&range_block);
    content.append(&bottom);

    let deadzone_percent = stick_property(page, analog, "deadzone", 0.15) * 100.0;
    let deadzone_label = gtk::Label::new(Some(&format!("Deadzone: {}%", deadzone_percent as i32)));
    content.append(&deadzone_label);
    let deadzone = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    deadzone.set_draw_value(false);
    deadzone.set_value(deadzone_percent);
    {
        // Upstream: relabel through `UpdateSliderText`, then
        // `param.Set("deadzone", slider_value / 100.0f)`.
        let page = Rc::downgrade(page);
        let caption = deadzone_label.clone();
        deadzone.connect_value_changed(move |scale| {
            let value = scale.value() as i32;
            caption.set_text(&format!("Deadzone: {value}%"));
            if let Some(page) = page.upgrade() {
                set_stick_property(&page, analog, "deadzone", value as f32 / 100.0);
            }
        });
    }
    content.append(&deadzone);

    let modifier_percent = stick_property(page, analog, "modifier_scale", 0.5) * 100.0;
    let modifier_label = gtk::Label::new(Some(&format!(
        "Modifier Range: {}%",
        modifier_percent as i32
    )));
    content.append(&modifier_label);
    let modifier = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    modifier.set_draw_value(false);
    modifier.set_value(modifier_percent);
    {
        // Upstream: `param.Set("modifier_scale", slider_value / 100.0f)`.
        let page = Rc::downgrade(page);
        let caption = modifier_label.clone();
        modifier.connect_value_changed(move |scale| {
            let value = scale.value() as i32;
            caption.set_text(&format!("Modifier Range: {value}%"));
            if let Some(page) = page.upgrade() {
                set_stick_property(&page, analog, "modifier_scale", value as f32 / 100.0);
            }
        });
    }
    content.append(&modifier);

    page.stick_controls.borrow_mut().push(StickControls {
        analog,
        deadzone_label,
        deadzone,
        range_block,
        range,
        modifier_block,
        modifier_button,
        modifier_label,
        modifier,
    });

    outer
}

/// One analog property of a stick binding, or `default` when unset.
///
/// Upstream reads it straight off `GetStickParam(analog_id)`.
fn stick_property(
    page: &Rc<PlayerPage>,
    analog: native_analog::Values,
    key: &str,
    default: f32,
) -> f64 {
    let state = page.state.borrow();
    let Some(param) = state.analogs.get(analog as usize) else {
        return default as f64;
    };
    common::param_package::ParamPackage::from_serialized(param).get_float(key, default) as f64
}

/// Write one analog property back into the stick's binding.
///
/// Upstream's sliders each do `param.Set(key, value); SetStickParam(id, param)`.
/// Setting it on an unbound stick would invent a parameter package with no
/// engine, which `ButtonToText` renders as `[not set]` anyway, so an empty
/// binding is left alone.
fn set_stick_property(page: &Rc<PlayerPage>, analog: native_analog::Values, key: &str, value: f32) {
    {
        let mut state = page.state.borrow_mut();
        let Some(slot) = state.analogs.get_mut(analog as usize) else {
            return;
        };
        if slot.is_empty() {
            return;
        }
        let mut param = common::param_package::ParamPackage::from_serialized(slot);
        param.set_float(key, value);
        *slot = param.serialize();
    }
    // The preview reads the deadzone and range through the controller.
    page.refresh_devices();
}

/// The D-Pad group: Up / Left-Right / Down.
fn dpad_group(page: &Rc<PlayerPage>) -> gtk::Box {
    let (outer, content, _) = compact_group("D-Pad");

    content.append(&binding_block(page, "Up", native_button::Values::DUp));

    let middle = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    middle.set_halign(gtk::Align::Center);
    middle.append(&binding_block(page, "Left", native_button::Values::DLeft));
    middle.append(&binding_block(page, "Right", native_button::Values::DRight));
    content.append(&middle);

    content.append(&binding_block(page, "Down", native_button::Values::DDown));

    outer
}

/// The Face Buttons group: X on top, Y and A on the sides, B below.
fn face_buttons_group(page: &Rc<PlayerPage>) -> gtk::Box {
    let (outer, content, _) = compact_group("Face Buttons");

    content.append(&binding_block(page, "X", native_button::Values::X));

    let middle = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    middle.set_halign(gtk::Align::Center);
    middle.append(&binding_block(page, "Y", native_button::Values::Y));
    middle.append(&binding_block(page, "A", native_button::Values::A));
    content.append(&middle);

    content.append(&binding_block(page, "B", native_button::Values::B));

    outer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_to_text_renders_sdl_buttons() {
        assert_eq!(
            button_to_text("engine:sdl,button:9,guid:0,port:0"),
            "Button 9"
        );
    }

    #[test]
    fn button_to_text_renders_upstream_qt_keyboard_names() {
        assert_eq!(button_to_text("engine:keyboard,code:67"), "C");
        assert_eq!(button_to_text("engine:keyboard,code:16777235"), "Up");
        assert_eq!(button_to_text("engine:keyboard,code:16777248"), "Shift");
        assert_eq!(
            button_to_text("engine:keyboard,code:88,turbo:true,toggle:true,inverted:true"),
            "$~!X"
        );
    }

    #[test]
    fn button_to_text_renders_axes_with_direction() {
        assert_eq!(
            button_to_text("engine:sdl,axis:1,direction:+,guid:0,port:0"),
            "Axis 1+"
        );
        assert_eq!(
            button_to_text("engine:sdl,axis:0,direction:-,guid:0,port:0"),
            "Axis 0-"
        );
    }

    #[test]
    fn button_to_text_reports_unmapped_inputs() {
        // Upstream returns "[not set]" whenever the param has no engine — an
        // empty or malformed mapping must not render as "Button " with no id.
        assert_eq!(button_to_text(""), NOT_SET);
        assert_eq!(button_to_text("button:3"), NOT_SET);
        assert_eq!(button_to_text("engine:sdl,port:0"), NOT_SET);
    }

    #[test]
    fn analog_to_text_splits_a_stick_param_into_four_directions() {
        // A single stick param binds both axes; each direction button shows
        // its own axis with the sign it moves in.
        let param = "engine:sdl,axis_x:0,axis_y:1,guid:0,port:0";
        assert_eq!(analog_to_text(param, Direction::Up), "Axis 1+");
        assert_eq!(analog_to_text(param, Direction::Down), "Axis 1-");
        assert_eq!(analog_to_text(param, Direction::Left), "Axis 0-");
        assert_eq!(analog_to_text(param, Direction::Right), "Axis 0+");
    }

    #[test]
    fn analog_to_text_follows_per_direction_button_bindings() {
        // A stick can also be bound one button per direction, in which case
        // upstream recurses into ButtonToText.
        let input =
            common::param_package::ParamPackage::from_pairs([("engine", "sdl"), ("button", "11")]);
        let mut analog = common::param_package::ParamPackage::default();
        set_analog_param(&input, &mut analog, Direction::Up);

        assert_eq!(
            analog_to_text(&analog.serialize(), Direction::Up),
            "Button 11"
        );
        assert_eq!(
            analog_to_text(&analog.serialize(), Direction::Down),
            NOT_SET
        );
    }

    #[test]
    fn analog_to_text_reports_unmapped_sticks() {
        assert_eq!(analog_to_text("", Direction::Up), NOT_SET);
        // Engine present but no axes bound: neither axis can be named.
        assert_eq!(
            analog_to_text("engine:sdl,port:0", Direction::Left),
            "[unknown]"
        );
    }

    #[test]
    fn set_analog_param_replaces_button_directions_with_a_complete_stick() {
        let button = common::param_package::ParamPackage::from_pairs([
            ("engine", "keyboard"),
            ("code", "119"),
        ]);
        let mut analog = common::param_package::ParamPackage::default();
        set_analog_param(&button, &mut analog, Direction::Up);
        assert_eq!(analog.get_str("engine", ""), "analog_from_button");
        assert!(analog.has("up"));

        let stick = common::param_package::ParamPackage::from_pairs([
            ("engine", "sdl"),
            ("axis_x", "0"),
            ("axis_y", "1"),
        ]);
        set_analog_param(&stick, &mut analog, Direction::Left);
        assert_eq!(analog.get_str("engine", ""), "sdl");
        assert_eq!(analog.get_str("axis_x", ""), "0");
        assert!(!analog.has("up"));
    }

    #[test]
    fn set_analog_param_preserves_other_button_directions() {
        let up = common::param_package::ParamPackage::from_pairs([
            ("engine", "keyboard"),
            ("code", "119"),
        ]);
        let left = common::param_package::ParamPackage::from_pairs([
            ("engine", "keyboard"),
            ("code", "97"),
        ]);
        let mut analog = common::param_package::ParamPackage::default();

        set_analog_param(&up, &mut analog, Direction::Up);
        set_analog_param(&left, &mut analog, Direction::Left);

        assert_eq!(analog.get_str("engine", ""), "analog_from_button");
        assert_eq!(
            common::param_package::ParamPackage::from_serialized(&analog.get_str("up", ""))
                .get_str("code", ""),
            "119"
        );
        assert_eq!(
            common::param_package::ParamPackage::from_serialized(&analog.get_str("left", ""))
                .get_str("code", ""),
            "97"
        );
    }

    #[test]
    fn inverted_stick_correction_matches_upstream_axis_choice() {
        let mut left = common::param_package::ParamPackage::from_pairs([
            ("engine", "sdl"),
            ("axis_x", "0"),
            ("axis_y", "1"),
        ]);
        correct_inverted_stick(&mut left, native_analog::Values::LStick as usize, true);
        assert_eq!(left.get_str("invert_x", ""), "-");
        assert_eq!(left.get_str("invert_y", "+"), "+");

        let mut right = left.clone();
        correct_inverted_stick(&mut right, native_analog::Values::RStick as usize, true);
        assert_eq!(right.get_str("invert_y", ""), "-");
    }

    #[test]
    fn stick_directions_do_not_share_an_axis() {
        // Up/Down must ride the y axis and Left/Right the x axis; swapping
        // them would silently invert a user's stick.
        assert_eq!(Direction::Up.axis_and_sign().0, "y");
        assert_eq!(Direction::Down.axis_and_sign().0, "y");
        assert_eq!(Direction::Left.axis_and_sign().0, "x");
        assert_eq!(Direction::Right.axis_and_sign().0, "x");
        assert_ne!(
            Direction::Up.axis_and_sign().1,
            Direction::Down.axis_and_sign().1
        );
        assert_ne!(
            Direction::Left.axis_and_sign().1,
            Direction::Right.axis_and_sign().1
        );
    }

    #[test]
    fn a_single_joycon_hides_the_other_half() {
        // Upstream's Left Joycon layout hides the right shoulder, Plus/Home and
        // the whole right column; the Right Joycon hides their mirrors. Getting
        // this wrong shows a player controls their pad does not have.
        let left = hidden_groups(ControllerType::LeftJoycon);
        assert!(left.contains(&"bottom_right"));
        assert!(left.contains(&"shoulder_right"));
        assert!(left.contains(&"plus_home"));
        assert!(!left.contains(&"bottom_left"));

        let right = hidden_groups(ControllerType::RightJoycon);
        assert!(right.contains(&"bottom_left"));
        assert!(right.contains(&"shoulder_left"));
        assert!(right.contains(&"minus_screenshot"));
        assert!(!right.contains(&"bottom_right"));
    }

    #[test]
    fn only_detached_joycons_expose_sl_and_sr() {
        // SL/SR exist on the rail of a detached Joy-Con. Every other layout
        // hides both; a single Joy-Con keeps only its own side.
        for layout in [
            ControllerType::ProController,
            ControllerType::Handheld,
            ControllerType::GameCube,
        ] {
            let hidden = hidden_groups(layout);
            assert!(
                hidden.contains(&"slsr_left"),
                "{layout:?} should hide SL/SR left"
            );
            assert!(
                hidden.contains(&"slsr_right"),
                "{layout:?} should hide SL/SR right"
            );
        }
        assert!(hidden_groups(ControllerType::DualJoyconDetached).is_empty());
        assert!(hidden_groups(ControllerType::LeftJoycon).contains(&"slsr_right"));
        assert!(!hidden_groups(ControllerType::LeftJoycon).contains(&"slsr_left"));
    }

    /// What the shoulder row looks like once the per-type hide list has run.
    fn visible_shoulder_row(layout: ControllerType) -> Vec<&'static str> {
        let hidden = hidden_groups(layout);
        SHOULDER_ROW
            .iter()
            .copied()
            .filter(|name| !hidden.contains(name))
            .collect()
    }

    /// The spacers are hidden together with the groups they separate, so a
    /// layout that drops a group also drops the gap it left behind. Hiding a
    /// group without its spacer bunches the rest of the row together in the
    /// middle instead of leaving one group per column.
    #[test]
    fn hiding_a_shoulder_group_takes_its_spacer_with_it() {
        // Pro, Handheld and GameCube: L/ZL at the left edge, Minus/Plus in the
        // middle, R/ZR at the right edge.
        let both_halves = vec![
            "shoulder_left",
            "spacer_1",
            "minus_screenshot",
            "plus_home",
            "spacer_3",
            "shoulder_right",
        ];
        for layout in [
            ControllerType::ProController,
            ControllerType::Handheld,
            ControllerType::GameCube,
        ] {
            assert_eq!(visible_shoulder_row(layout), both_halves, "{layout:?}");
        }

        // A detached Joy-Con pair shows the whole row.
        assert_eq!(
            visible_shoulder_row(ControllerType::DualJoyconDetached),
            SHOULDER_ROW.to_vec()
        );

        // A lone left Joy-Con ends on Minus/Capture, so it lands at the right
        // edge rather than floating in the middle.
        assert_eq!(
            visible_shoulder_row(ControllerType::LeftJoycon),
            vec![
                "slsr_left",
                "spacer_4",
                "shoulder_left",
                "spacer_1",
                "minus_screenshot"
            ]
        );

        // A lone right Joy-Con starts on Plus/Home, at the left edge.
        assert_eq!(
            visible_shoulder_row(ControllerType::RightJoycon),
            vec![
                "plus_home",
                "spacer_3",
                "shoulder_right",
                "spacer_2",
                "slsr_right"
            ]
        );
    }

    /// A spacer that never ends up next to a visible group is dead weight, and
    /// two visible groups with no spacer between them share a column. The only
    /// pair upstream deliberately leaves adjacent is Minus/Capture + Plus/Home.
    #[test]
    fn the_shoulder_row_never_leaves_a_spacer_at_an_edge() {
        for layout in [
            ControllerType::ProController,
            ControllerType::Handheld,
            ControllerType::GameCube,
            ControllerType::DualJoyconDetached,
            ControllerType::LeftJoycon,
            ControllerType::RightJoycon,
        ] {
            let row = visible_shoulder_row(layout);
            let is_spacer = |name: &str| name.starts_with("spacer_");
            assert!(
                !is_spacer(row.first().expect("a non-empty row")),
                "{layout:?} starts with a spacer"
            );
            assert!(
                !is_spacer(row.last().expect("a non-empty row")),
                "{layout:?} ends with a spacer"
            );
            for pair in row.windows(2) {
                assert!(
                    !(is_spacer(pair[0]) && is_spacer(pair[1])),
                    "{layout:?} has two spacers in a row: {pair:?}"
                );
            }
        }
    }

    /// The rows `InputSubsystem::get_input_devices` produces, in its order.
    fn device_rows() -> Vec<common::param_package::ParamPackage> {
        let row = |pairs: &[(&str, &str)]| {
            common::param_package::ParamPackage::from_pairs(
                pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())),
            )
        };
        vec![
            row(&[("engine", "any"), ("display", "Any")]),
            row(&[("engine", "keyboard"), ("display", "Keyboard Only")]),
            row(&[("engine", "mouse"), ("display", "Keyboard/Mouse")]),
            row(&[
                ("engine", "sdl"),
                ("display", "Xbox One Elite 2 Controller 0"),
                ("guid", "030000005e040000000b000015050000"),
                ("port", "0"),
            ]),
        ]
    }

    fn player_bound_to(engine: &str, guid: &str, port: &str) -> PlayerInput {
        let mut player = PlayerInput::default();
        player.buttons[0] = format!("engine:{engine},guid:{guid},port:{port},button:1");
        player
    }

    /// Upstream `UpdateInputDeviceCombobox` reads the device back out of the
    /// stored bindings, so reopening the dialog lands on the pad the player is
    /// already mapped to rather than resetting to "Any".
    #[test]
    fn the_device_combo_reopens_on_the_mapped_pad() {
        let devices = device_rows();
        let player = player_bound_to("sdl", "030000005e040000000b000015050000", "0");
        assert_eq!(device_index_for(&player, &devices), 3);
    }

    /// A binding naming a pad that is no longer plugged in falls back to "Any",
    /// as does a player mapped across three or more devices.
    #[test]
    fn an_unknown_or_scattered_mapping_falls_back_to_any() {
        let devices = device_rows();

        let unplugged = player_bound_to("sdl", "ffffffffffffffffffffffffffffffff", "0");
        assert_eq!(device_index_for(&unplugged, &devices), 0);

        let mut scattered = PlayerInput::default();
        for (slot, guid) in ["aa", "bb", "cc"].iter().enumerate() {
            scattered.buttons[slot] = format!("engine:sdl,guid:{guid},port:0,button:1");
        }
        assert_eq!(device_index_for(&scattered, &devices), 0);
    }

    /// Keyboard and mouse bindings together are upstream's "Keyboard/Mouse" row.
    #[test]
    fn keyboard_and_mouse_together_select_their_shared_row() {
        let devices = device_rows();
        let mut player = PlayerInput::default();
        player.buttons[0] = "engine:keyboard,code:65".to_string();
        player.analogs[0] = "engine:mouse,axis_x:0,axis_y:1".to_string();
        assert_eq!(device_index_for(&player, &devices), 2);
    }

    /// Upstream installs `QtConfig::default_*` when either keyboard row is
    /// selected; the keyboard driver itself does not provide these mappings.
    #[test]
    fn keyboard_device_installs_qt_default_bindings() {
        let mut player = PlayerInput::default();
        apply_keyboard_defaults(&mut player);

        let button = |button: native_button::Values| {
            common::param_package::ParamPackage::from_serialized(&player.buttons[button as usize])
        };
        assert_eq!(
            button(native_button::Values::A).get_int("code", 0),
            b'C' as i32
        );
        assert_eq!(
            button(native_button::Values::Plus).get_int("code", 0),
            b'M' as i32
        );
        assert_eq!(
            button(native_button::Values::Minus).get_int("code", 0),
            b'N' as i32
        );
        assert_eq!(
            button(native_button::Values::DUp).get_int("code", 0),
            0x0100_0013
        );

        let left_stick = common::param_package::ParamPackage::from_serialized(
            &player.analogs[native_analog::Values::LStick as usize],
        );
        assert_eq!(left_stick.get_str("engine", ""), "analog_from_button");
        let up =
            common::param_package::ParamPackage::from_serialized(&left_stick.get_str("up", ""));
        assert_eq!(up.get_int("code", 0), b'W' as i32);

        let motion = common::param_package::ParamPackage::from_serialized(&player.motions[0]);
        assert_eq!(motion.get_int("code", 0), b'7' as i32);
    }

    /// With nothing mapped yet the page adopts the pad that is plugged in; with
    /// no pad at all it stays on "Any".
    #[test]
    fn an_unmapped_player_adopts_the_pad_that_is_plugged_in() {
        let devices = device_rows();
        assert_eq!(device_index_for(&PlayerInput::default(), &devices), 0);
        assert_eq!(first_connected_pad(&devices), Some(3));

        // "Any", keyboard and mouse are not pads.
        assert_eq!(first_connected_pad(&devices[..3]), None);
    }

    /// A stick built out of button bindings names no device of its own —
    /// upstream skips it so it cannot outvote the real pad.
    #[test]
    fn a_stick_made_of_buttons_is_not_counted_as_a_device() {
        let mut player = player_bound_to("sdl", "030000005e040000000b000015050000", "0");
        player.analogs[0] = "engine:analog_from_button,up:engine$0keyboard".to_string();
        assert_eq!(mapped_devices(&player).len(), 1);
        assert_eq!(device_index_for(&player, &device_rows()), 3);
    }

    #[test]
    fn a_gamecube_pad_disables_the_controls_it_lacks() {
        // No home button, no clickable sticks, and L is analog rather than a
        // digital button — upstream disables rather than hides these.
        let disabled = disabled_groups(ControllerType::GameCube);
        for name in ["home", "lstick_pressed", "rstick_pressed", "button_l"] {
            assert!(disabled.contains(&name), "{name} should be disabled");
        }
        assert!(disabled_groups(ControllerType::ProController).is_empty());
    }

    #[test]
    fn motion_groups_follow_the_halves_the_controller_has() {
        assert_eq!(
            motion_visibility(ControllerType::ProController),
            (true, false)
        );
        assert_eq!(motion_visibility(ControllerType::LeftJoycon), (true, false));
        // The right Joy-Con is motion 2, not motion 1 — reusing motion 1 would
        // write the binding into the wrong slot.
        assert_eq!(
            motion_visibility(ControllerType::RightJoycon),
            (false, true)
        );
        assert_eq!(
            motion_visibility(ControllerType::DualJoyconDetached),
            (true, true)
        );
        assert_eq!(motion_visibility(ControllerType::GameCube), (false, false));
    }

    #[test]
    fn a_gamecube_pad_renames_its_shoulder_and_stick_groups() {
        let titles = group_titles(ControllerType::GameCube);
        let of = |key: &str| titles.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        assert_eq!(of("plus"), Some("Start / Pause"));
        assert_eq!(of("lstick"), Some("Control Stick"));
        assert_eq!(of("rstick"), Some("C-Stick"));
        // The GameCube shoulders shift by one: its ZL slot is labelled L, and
        // the Switch R slot becomes Z.
        assert_eq!(of("zl"), Some("L"));
        assert_eq!(of("zr"), Some("R"));
        assert_eq!(of("r"), Some("Z"));
    }

    #[test]
    fn every_switch_layout_uses_the_switch_names() {
        for layout in [
            ControllerType::ProController,
            ControllerType::DualJoyconDetached,
            ControllerType::LeftJoycon,
            ControllerType::RightJoycon,
            ControllerType::Handheld,
        ] {
            let titles = group_titles(layout);
            assert!(titles.contains(&("lstick", "Left Stick")), "{layout:?}");
            assert!(titles.contains(&("plus", "Plus")), "{layout:?}");
        }
    }

    #[test]
    fn every_hideable_group_is_restored_before_the_hide_list_runs() {
        // Upstream re-shows `layout_show` first; any name that some layout
        // hides but that list omits would stay hidden forever once a user
        // switched away from that controller type.
        for layout in [
            ControllerType::ProController,
            ControllerType::DualJoyconDetached,
            ControllerType::LeftJoycon,
            ControllerType::RightJoycon,
            ControllerType::Handheld,
            ControllerType::GameCube,
        ] {
            for name in hidden_groups(layout) {
                assert!(
                    ALWAYS_SHOWN_GROUPS.contains(name),
                    "{name} is hidden by {layout:?} but never re-shown"
                );
            }
            for name in disabled_groups(layout) {
                assert!(
                    ALWAYS_ENABLED_GROUPS.contains(name),
                    "{name} is disabled by {layout:?} but never re-enabled"
                );
            }
        }
    }

    #[test]
    fn controller_type_rows_start_with_pro_controller() {
        // The default `PlayerInput::controller_type` is `ProController`; if it
        // were not row 0, a fresh profile would display the wrong type.
        assert_eq!(CONTROLLER_TYPES[0].0, ControllerType::ProController);
        assert_eq!(
            PlayerInput::default().controller_type,
            CONTROLLER_TYPES[0].0
        );
    }
}
