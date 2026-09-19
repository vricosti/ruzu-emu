// SPDX-License-Identifier: GPL-3.0-or-later
//
// GTK counterpart of upstream `yuzu/util/controller_navigation.{h,cpp}`.

use std::collections::VecDeque;
use std::sync::Arc;

use common::input::{ButtonStatus, StickStatus};
use common::settings_input::{native_analog, native_button};
use gtk::prelude::*;
use hid_core::frontend::emulated_controller::{ControllerTriggerType, ControllerUpdateCallback};
use hid_core::hid_core::{EmulatedControllerHandle, HIDCore};
use hid_core::hid_types::{NpadIdType, NpadStyleIndex};
use parking_lot::Mutex;

/// GTK adaptation of the upstream keyboard-event delivery. Only one general
/// UI receiver drains HID input; applets with their own button semantics opt
/// out. Never inject platform keyboard events into another application.
pub(crate) fn install_interface_navigation(
    main: &gtk::Window,
    hid: &Arc<Mutex<HIDCore>>,
    input: &std::rc::Rc<std::cell::RefCell<input_common::InputSubsystem>>,
    launcher_active: impl Fn() -> bool + 'static,
    list_key: impl Fn(NavigationKey) -> bool + 'static,
) {
    let navigation = ControllerNavigation::for_interface(hid, input);
    let weak = main.downgrade();
    let previous_target = gtk::glib::WeakRef::<gtk::Window>::new();
    let mut repeat = NavigationRepeat::default();
    gtk::glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
        let Some(main) = weak.upgrade() else {
            return gtk::glib::ControlFlow::Break;
        };
        // Drain even when inactive, so releases update the edge detector and
        // presses received in another window never become deferred actions.
        let mut keys = navigation.take_pending_keys();
        let held = navigation.held_vertical_direction();
        let target = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.is_active() && w.is_visible() && belongs_to(w, &main))
            .filter(|w| {
                !w.has_css_class("ruzu-applet-navigation")
                    && !w.has_css_class("ruzu-controller-capture")
                    && interface_navigation_allowed(w)
                    && (w != &main || launcher_active())
            });
        let changed = previous_target.upgrade() != target;
        previous_target.set(target.as_ref());
        let Some(target) = target else {
            repeat.suppress(held);
            return gtk::glib::ControlFlow::Continue;
        };
        if changed {
            repeat.suppress(held);
            return gtk::glib::ControlFlow::Continue;
        }
        if let Some(key) = repeat.poll(held, std::time::Instant::now()) {
            if !keys.contains(&key) { keys.push(key); }
        }
        for key in keys {
            // A directional action can select Controls within the same window.
            // Do not deliver the rest of that batch to its mapping widgets.
            if !interface_navigation_allowed(&target) { break; }
            if target == main && list_key(key) {
                target.set_focus_visible(true);
            } else {
                navigate_window(&target, key);
            }
            // Activation may open/close a modal or start emulation. Discard
            // the remaining batch rather than delivering it across that boundary.
            if matches!(key, NavigationKey::Enter | NavigationKey::Escape)
                || !target.is_visible()
                || !target.is_active()
            {
                break;
            }
        }
        gtk::glib::ControlFlow::Continue
    });
}

pub(crate) const INPUT_CONFIGURATION_CSS_CLASS: &str = "ruzu-input-configuration";

fn interface_navigation_allowed(window: &gtk::Window) -> bool {
    let mut current = Some(window.clone());
    while let Some(window) = current {
        if window.has_css_class(INPUT_CONFIGURATION_CSS_CLASS) {
            return false;
        }
        current = window.transient_for();
    }
    true
}

fn belongs_to(window: &gtk::Window, main: &gtk::Window) -> bool {
    let mut current = Some(window.clone());
    while let Some(window) = current {
        if window == *main {
            return true;
        }
        current = window.transient_for();
    }
    false
}

/// Invoke GTK's widget actions, not emulated keyboard/controller bindings.
/// GTK owns focus ordering, scrolling and selection inside its file chooser.
/// Focus the menu bar without opening a menu or activating an action.
pub(crate) fn focus_menu_bar(window: &gtk::Window) -> bool {
    super::inline_menu::focus_bar(window)
        || find_widget::<gtk::PopoverMenuBar>(window.upcast_ref())
            .and_then(|menu| menu.first_child())
            .is_some_and(|item| item.grab_focus())
}

pub(crate) fn navigate_window(window: &gtk::Window, key: NavigationKey) {
    if super::inline_menu::navigate(window, key) {
        window.set_focus_visible(true);
        return;
    }
    if key == NavigationKey::Menu {
        if let Some(menu) = find_widget::<gtk::PopoverMenuBar>(window.upcast_ref()) {
            if let Some(item) = menu.first_child() {
                item.grab_focus();
                item.activate();
            }
        }
        window.set_focus_visible(true);
        return;
    }
    let direction = match key {
        NavigationKey::Up => Some(gtk::DirectionType::Up),
        NavigationKey::Down => Some(gtk::DirectionType::Down),
        NavigationKey::Left => Some(gtk::DirectionType::Left),
        NavigationKey::Right => Some(gtk::DirectionType::Right),
        NavigationKey::Previous => Some(gtk::DirectionType::TabBackward),
        NavigationKey::Next => Some(gtk::DirectionType::TabForward),
        _ => None,
    };
    let focus = gtk::prelude::GtkWindowExt::focus(window);
    if matches!(
        key,
        NavigationKey::Up | NavigationKey::Down | NavigationKey::Left | NavigationKey::Right
    ) && focus
        .as_ref()
        .is_some_and(|focus| activate_key_binding(focus, key))
    {
        window.set_focus_visible(true);
        return;
    }
    let mut ancestor = focus.clone();
    while let Some(widget) = ancestor {
        // FileChooserWidget uses TreeView on the supported GTK 4.6 baseline.
        // Its action signal preserves cursor selection, folder activation and
        // automatic scroll-to-cursor, unlike moving focus between widgets.
        if let Some(tree) = widget.downcast_ref::<gtk::TreeView>() {
            match key {
                NavigationKey::Up | NavigationKey::Down => {
                    let count: i32 = if key == NavigationKey::Up { -1 } else { 1 };
                    tree.emit_by_name::<bool>(
                        "move-cursor",
                        &[&gtk::MovementStep::DisplayLines, &count, &false, &false],
                    );
                    window.set_focus_visible(true);
                    return;
                }
                NavigationKey::Enter => {
                    tree.emit_by_name::<bool>("select-cursor-row", &[&false]);
                    window.set_focus_visible(true);
                    return;
                }
                _ => {}
            }
        }
        if let Some(range) = widget.downcast_ref::<gtk::Range>() {
            if matches!(key, NavigationKey::Left | NavigationKey::Right) {
                let delta = range.adjustment().step_increment()
                    * if key == NavigationKey::Left {
                        -1.0
                    } else {
                        1.0
                    };
                range.set_value(range.value() + delta);
                window.set_focus_visible(true);
                return;
            }
        }
        if let Some(popover) = widget.downcast_ref::<gtk::Popover>() {
            if key == NavigationKey::Escape {
                popover.popdown();
                window.set_focus_visible(true);
                return;
            }
            if let Some(direction) = direction {
                let moved = popover.child_focus(direction);
                // At a menu boundary GTK may focus its internal scroller.
                // Keep the last item selected: A must still activate an item,
                // not disappear into an otherwise inert container.
                let on_scroller = gtk::prelude::GtkWindowExt::focus(window)
                    .is_some_and(|w| w.is::<gtk::ScrolledWindow>());
                if on_scroller {
                    if let Some(focus) = focus.as_ref() {
                        focus.grab_focus();
                    }
                }
                if (!moved || on_scroller)
                    && matches!(key, NavigationKey::Left | NavigationKey::Right)
                {
                    if let Some(bar) = popover.ancestor(gtk::PopoverMenuBar::static_type()) {
                        let mut item: gtk::Widget = popover.clone().upcast();
                        while item.parent().is_some_and(|parent| parent != bar) {
                            item = item.parent().unwrap();
                        }
                        let next = if key == NavigationKey::Right {
                            item.next_sibling().or_else(|| bar.first_child())
                        } else {
                            item.prev_sibling().or_else(|| bar.last_child())
                        };
                        if let Some(next) = next.filter(|w| w.is_visible() && w.is_sensitive()) {
                            popover.popdown();
                            next.grab_focus();
                            next.activate();
                        }
                    }
                }
                window.set_focus_visible(true);
                return;
            }
        }
        if let Some(menu) = widget.downcast_ref::<gtk::PopoverMenuBar>() {
            if matches!(key, NavigationKey::Left | NavigationKey::Right) {
                menu.child_focus(direction.unwrap());
                window.set_focus_visible(true);
                return;
            }
        }
        ancestor = widget.parent();
    }
    match key {
        NavigationKey::Enter => {
            if let Some(widget) = focus {
                if widget.is_sensitive() {
                    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                        // GtkButton::activate waits for keyboard animation;
                        // a controller has no corresponding GTK key release.
                        button.emit_clicked();
                    } else if !activate_key_binding(&widget, key) {
                        widget.activate();
                    }
                }
            } else {
                window.child_focus(gtk::DirectionType::TabForward);
            }
        }
        NavigationKey::Escape => {
            // Never quit the launcher on B. A modal's normal close handler
            // owns cancellation and cleanup (including file chooser callbacks).
            if window.is_modal() {
                window.close();
            }
        }
        _ => {
            if let Some(direction) = direction {
                if !window.child_focus(direction)
                    && matches!(key, NavigationKey::Previous | NavigationKey::Next)
                {
                    gtk::prelude::GtkWindowExt::set_focus(window, gtk::Widget::NONE);
                    window.child_focus(direction);
                }
            }
        }
    }
    // GTK clears focus-visible when assigning focus. Set it afterwards.
    window.set_focus_visible(true);
}

/// GTK publishes its class keybindings as ShortcutControllers. Reuse their
/// actions and arguments, including ListView/GridView cursor movement, rather
/// than depending on the private widgets used by a particular GTK release.
fn activate_key_binding(focus: &gtk::Widget, key: NavigationKey) -> bool {
    let keyval = match key {
        NavigationKey::Enter => gtk::gdk::Key::Return,
        NavigationKey::Up => gtk::gdk::Key::Up,
        NavigationKey::Down => gtk::gdk::Key::Down,
        NavigationKey::Left => gtk::gdk::Key::Left,
        NavigationKey::Right => gtk::gdk::Key::Right,
        _ => return false,
    };
    // Run owner-defined focus transitions before GTK's built-in spatial search.
    // The same capture-phase shortcuts handle physical keyboard presses.
    let mut owner = Some(focus.clone());
    while let Some(widget) = owner {
        if widget.is::<gtk::Popover>() || widget.is::<gtk::Window>() { break; }
        for controller in widget.observe_controllers().iter::<gtk::glib::Object>().flatten()
            .filter_map(|object| object.downcast::<gtk::EventController>().ok()) {
            if controller.name().as_deref() != Some("ruzu-directional-navigation") { continue; }
            let Some(controller) = controller.downcast_ref::<gtk::ShortcutController>() else { continue; };
            for shortcut in controller.iter::<gtk::glib::Object>().flatten()
                .filter_map(|object| object.downcast::<gtk::Shortcut>().ok()) {
                let Some(trigger) = shortcut.trigger().and_downcast::<gtk::KeyvalTrigger>() else { continue; };
                if trigger.keyval() == keyval && trigger.modifiers().is_empty()
                    && shortcut.action().is_some_and(|action| action.activate(
                        gtk::ShortcutActionFlags::empty(), &widget, shortcut.arguments().as_ref()))
                {
                    return true;
                }
            }
        }
        owner = widget.parent();
    }
    let mut current = Some(focus.clone());
    while let Some(widget) = current {
        if widget.is::<gtk::Popover>() || widget.is::<gtk::Window>() {
            break;
        }
        // Do not invoke a container's arrow-to-scroll shortcut before moving
        // focus between its buttons (notably menu items inside a scroller).
        if !(widget.is::<gtk::ListView>()
            || widget.is::<gtk::GridView>()
            || widget.is::<gtk::ColumnView>()
            || widget.is::<gtk::TreeView>()
            || widget.is::<gtk::ListBox>()
            || widget.is::<gtk::Range>()
            || widget.is::<gtk::SpinButton>())
        {
            current = widget.parent();
            continue;
        }
        let controllers = widget.observe_controllers();
        for i in 0..controllers.n_items() {
            let Some(controller) = controllers
                .item(i)
                .and_downcast::<gtk::ShortcutController>()
            else {
                continue;
            };
            for i in 0..controller.n_items() {
                let Some(shortcut) = controller.item(i).and_downcast::<gtk::Shortcut>() else {
                    continue;
                };
                let Some(trigger) = shortcut.trigger().and_downcast::<gtk::KeyvalTrigger>() else {
                    continue;
                };
                if trigger.keyval() == keyval && trigger.modifiers().is_empty() {
                    if let Some(action) = shortcut.action() {
                        if action.activate(
                            gtk::ShortcutActionFlags::empty(),
                            &widget,
                            shortcut.arguments().as_ref(),
                        ) {
                            return true;
                        }
                    }
                }
            }
        }
        current = widget.parent();
    }
    false
}

fn find_widget<T: IsA<gtk::Widget> + gtk::glib::types::StaticType>(
    root: &gtk::Widget,
) -> Option<T> {
    if !root.is_visible() {
        return None;
    }
    if let Ok(widget) = root.clone().downcast::<T>() {
        return Some(widget);
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = find_widget::<T>(&widget) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

/// Keyboard-equivalent actions emitted by upstream `ControllerNavigation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationKey {
    Enter,
    Escape,
    Down,
    Left,
    Right,
    Up,
    /// GTK focus traversal, exposed on the shoulder buttons.
    Previous,
    Next,
    /// Plus opens the in-window menu bar, which GTK omits from its Tab chain.
    Menu,
}

struct NavigationState {
    button_values: Vec<ButtonStatus>,
    stick_values: Vec<StickStatus>,
    pending_triggers: VecDeque<ControllerTriggerType>,
    pending_keys: VecDeque<NavigationKey>,
}

// Frontend-only repeat: guest input and applet confirmation remain edge-triggered.
#[derive(Default)]
struct NavigationRepeat {
    held: Option<NavigationKey>,
    next: Option<std::time::Instant>,
    suppressed: bool,
}

impl NavigationRepeat {
    fn suppress(&mut self, held: Option<NavigationKey>) {
        self.held = held;
        self.next = None;
        self.suppressed = held.is_some();
    }

    fn poll(&mut self, held: Option<NavigationKey>, now: std::time::Instant) -> Option<NavigationKey> {
        let held = held.filter(|key| matches!(key, NavigationKey::Up | NavigationKey::Down));
        if held.is_none() { *self = Self::default(); return None; }
        if self.suppressed { return None; }
        if self.held != held {
            self.held = held;
            self.next = Some(now + std::time::Duration::from_millis(400));
            return None;
        }
        if self.next.is_some_and(|next| now >= next) {
            // Do not replay a backlog if the UI thread was busy.
            self.next = Some(now + std::time::Duration::from_millis(80));
            held
        } else { None }
    }
}

impl Default for NavigationState {
    fn default() -> Self {
        Self {
            button_values: vec![ButtonStatus::default(); native_button::NUM_BUTTONS],
            stick_values: vec![StickStatus::default(); native_analog::NUM_ANALOGS],
            pending_triggers: VecDeque::new(),
            pending_keys: VecDeque::new(),
        }
    }
}

/// Port of upstream `ControllerNavigation`.
///
/// GTK objects are main-thread-only, while HID callbacks are `Send + Sync`.
/// The callback therefore queues upstream's keyboard-equivalent action and the
/// owning widget drains it from the GTK main loop.
pub struct ControllerNavigation {
    state: Arc<Mutex<NavigationState>>,
    player_1_controller: EmulatedControllerHandle,
    handheld_controller: EmulatedControllerHandle,
    player_1_callback_key: i32,
    handheld_callback_key: i32,
    interface_pad: std::cell::RefCell<Option<InterfacePad>>,
}

impl ControllerNavigation {
    pub fn new(hid_core: &Arc<Mutex<HIDCore>>) -> Self {
        let (player_1_controller, handheld_controller) = {
            let hid_core = hid_core.lock();
            (
                hid_core.get_emulated_controller(NpadIdType::Player1),
                hid_core.get_emulated_controller(NpadIdType::Handheld),
            )
        };
        let state = Arc::new(Mutex::new(NavigationState::default()));

        let callback_state = Arc::clone(&state);
        let on_change: Arc<dyn Fn(ControllerTriggerType) + Send + Sync> =
            Arc::new(move |trigger_type| {
                callback_state
                    .lock()
                    .pending_triggers
                    .push_back(trigger_type);
            });

        let player_1_callback_key =
            player_1_controller
                .lock()
                .set_callback(ControllerUpdateCallback {
                    on_change: Arc::clone(&on_change),
                    is_npad_service: false,
                });
        let handheld_callback_key =
            handheld_controller
                .lock()
                .set_callback(ControllerUpdateCallback {
                    on_change,
                    is_npad_service: false,
                });

        Self {
            state,
            player_1_controller,
            handheld_controller,
            player_1_callback_key,
            handheld_callback_key,
            interface_pad: std::cell::RefCell::new(None),
        }
    }

    pub(crate) fn for_interface(
        hid: &Arc<Mutex<HIDCore>>,
        input: &std::rc::Rc<std::cell::RefCell<input_common::InputSubsystem>>,
    ) -> Self {
        let navigation = Self::new(hid);
        *navigation.interface_pad.borrow_mut() = Some(InterfacePad {
            input: std::rc::Rc::downgrade(input),
            devices: Vec::new(),
            pad_states: Vec::new(),
            identity: String::new(),
            state: Arc::new(Mutex::new(NavigationState::default())),
            refresh_at: std::time::Instant::now(),
        });
        navigation
    }

    /// Drain keyboard-equivalent actions on the GTK main thread.
    pub fn take_pending_keys(&self) -> Vec<NavigationKey> {
        let triggers: Vec<_> = self.state.lock().pending_triggers.drain(..).collect();
        for trigger_type in triggers {
            controller_update_event(
                &self.state,
                &self.player_1_controller,
                &self.handheld_controller,
                trigger_type,
            );
        }
        let mut keys: Vec<_> = self.state.lock().pending_keys.drain(..).collect();
        if let Some(pad) = self.interface_pad.borrow_mut().as_mut() {
            pad.refresh(&self.player_1_controller, &self.handheld_controller);
            let pending: Vec<_> = pad.state.lock().pending_keys.drain(..).collect();
            if *common::settings::values().controller_navigation.get_value() {
                keys.extend(pending);
            }
        }
        keys
    }

    fn held_vertical_direction(&self) -> Option<NavigationKey> {
        if !*common::settings::values().controller_navigation.get_value() { return None; }
        let controller = self.player_1_controller.lock();
        let style = controller.get_npad_style_index(false);
        drop(controller);
        let direction = vertical_direction(&self.state.lock(), style);
        let mut up = direction == Some(NavigationKey::Up);
        let mut down = direction == Some(NavigationKey::Down);
        if let Some(pad) = self.interface_pad.borrow().as_ref() {
            for state in &pad.pad_states {
                let direction = vertical_direction(&state.lock(), NpadStyleIndex::Fullkey);
                up |= direction == Some(NavigationKey::Up);
                down |= direction == Some(NavigationKey::Down);
            }
        }
        match (up, down) {
            (true, false) => Some(NavigationKey::Up),
            (false, true) => Some(NavigationKey::Down),
            _ => None,
        }
    }

    /// Discard events received while the list is hidden or inactive.
    pub fn discard_pending_keys(&self) {
        let mut state = self.state.lock();
        state.pending_triggers.clear();
        state.pending_keys.clear();
        drop(state);
        if let Some(pad) = self.interface_pad.borrow_mut().as_mut() {
            pad.refresh(&self.player_1_controller, &self.handheld_controller);
            pad.state.lock().pending_keys.clear();
        }
    }

    /// Upstream `ControllerNavigation::UnloadController`.
    pub fn unload_controller(&mut self) {
        if self.player_1_callback_key >= 0 {
            self.player_1_controller
                .lock()
                .delete_callback(self.player_1_callback_key);
            self.player_1_callback_key = -1;
        }
        if self.handheld_callback_key >= 0 {
            self.handheld_controller
                .lock()
                .delete_callback(self.handheld_callback_key);
            self.handheld_callback_key = -1;
        }
    }
}

/// First-run GTK adaptation: listen to SDL pads even before the user maps them
/// to emulated controllers. Mapped pads retain ControllerNavigation's normal
/// HID path. The input factories, mappings and callbacks remain owned by
/// InputSubsystem; this reader never changes player configuration.
struct InterfacePad {
    pad_states: Vec<Arc<Mutex<NavigationState>>>,
    input: std::rc::Weak<std::cell::RefCell<input_common::InputSubsystem>>,
    devices: Vec<Box<dyn common::input::InputDevice>>,
    identity: String,
    state: Arc<Mutex<NavigationState>>,
    refresh_at: std::time::Instant,
}

impl InterfacePad {
    fn refresh(&mut self, player: &EmulatedControllerHandle, handheld: &EmulatedControllerHandle) {
        let now = std::time::Instant::now();
        if now < self.refresh_at {
            return;
        }
        let Some(input) = self.input.upgrade() else {
            self.devices.clear();
            self.pad_states.clear();
            return;
        };
        // SDL's macOS HID discovery can reenter GTK while PumpEvents holds
        // the mutable input borrow. Retry next tick without expiring this refresh.
        let Ok(input) = input.try_borrow() else {
            return;
        };
        self.refresh_at = now + std::time::Duration::from_secs(1);
        let mut devices = input.get_input_devices();
        devices.sort_by_key(interface_device_identity);
        let candidates: Vec<_> = devices
            .into_iter()
            .filter(|device| {
                if device.get_str("engine", "") != "sdl" {
                    return false;
                }
                ![player, handheld].into_iter().any(|controller| {
                    let controller = controller.lock();
                    (0..native_button::NUM_BUTTONS).any(|i| {
                        let mapping = controller.get_button_param(i);
                        mapping.get_str("engine", "") == "sdl"
                            && (mapping.get_str("guid", "") == device.get_str("guid", "")
                                || (!mapping.get_str("guid2", "").is_empty()
                                    && mapping.get_str("guid2", "") == device.get_str("guid", "")))
                            && mapping.get_int("port", 0) == device.get_int("port", 0)
                    })
                })
            })
            .collect();
        let identity = candidates
            .iter()
            .map(interface_device_identity)
            .collect::<Vec<_>>()
            .join("|");
        if identity == self.identity {
            return;
        }
        self.identity = identity;
        self.devices.clear();
        self.pad_states.clear();
        // An already-dispatched callback from a removed device can finish on
        // another thread. Give new readers a new queue so that callback cannot
        // deliver stale input after reconnection or a mapping change.
        self.state = Arc::new(Mutex::new(NavigationState::default()));
        for candidate in candidates {
            let pad_state = Arc::new(Mutex::new(NavigationState::default()));
            self.pad_states.push(pad_state.clone());
            let mappings = input.get_button_mapping_for_device(&candidate);
            use native_button::Values as Button;
            for (button, key) in [
                (Button::A, NavigationKey::Enter),
                (Button::B, NavigationKey::Escape),
                (Button::DUp, NavigationKey::Up),
                (Button::DDown, NavigationKey::Down),
                (Button::DLeft, NavigationKey::Left),
                (Button::DRight, NavigationKey::Right),
                (Button::L, NavigationKey::Previous),
                (Button::R, NavigationKey::Next),
                (Button::Plus, NavigationKey::Menu),
            ] {
                let Some(mapping) = mappings.get(&(button as i32)) else {
                    continue;
                };
                let mut device = common::input::create_input_device(mapping);
                let state = pad_state.clone();
                let output = self.state.clone();
                device.set_callback(common::input::InputCallback {
                    on_change: Some(Arc::new(move |status| {
                        let value =
                            hid_core::frontend::input_converter::transform_to_button(status).value;
                        let mut state = state.lock();
                        let previous = &mut state.button_values[button as usize];
                        previous.locked = previous.value == value;
                        previous.value = value;
                        trigger_button(&mut state, button, key);
                        output
                            .lock()
                            .pending_keys
                            .extend(state.pending_keys.drain(..));
                    })),
                });
                device.force_update();
                self.devices.push(device);
            }
            if let Some(mapping) = input
                .get_analog_mapping_for_device(&candidate)
                .get(&(native_analog::Values::LStick as i32))
            {
                let mut device = common::input::create_input_device(mapping);
                let state = pad_state.clone();
                let output = self.state.clone();
                device.set_callback(common::input::InputCallback {
                    on_change: Some(Arc::new(move |status| {
                        let stick = hid_core::frontend::input_converter::transform_to_stick(status);
                        let mut state = state.lock();
                        let previous = state.stick_values[native_analog::Values::LStick as usize];
                        state.stick_values[native_analog::Values::LStick as usize] = stick;
                        if (previous.up, previous.down, previous.left, previous.right)
                            != (stick.up, stick.down, stick.left, stick.right)
                        {
                            if let Some(key) =
                                stick_navigation_key(NpadStyleIndex::Fullkey, &state.stick_values)
                            {
                                output.lock().pending_keys.push_back(key);
                            }
                        }
                    })),
                });
                device.force_update();
                self.devices.push(device);
            }
        }
        // Connecting a pad with a held button must not accept a dialog.
        self.state.lock().pending_keys.clear();
    }
}

fn interface_device_identity(device: &common::param_package::ParamPackage) -> String {
    // ParamPackage serialization is unordered; it is not a stable device key.
    format!(
        "{}:{}:{}",
        device.get_str("guid", ""),
        device.get_str("guid2", ""),
        device.get_int("port", 0)
    )
}

impl Drop for ControllerNavigation {
    fn drop(&mut self) {
        self.unload_controller();
    }
}

fn controller_update_event(
    state: &Mutex<NavigationState>,
    player_1_controller: &EmulatedControllerHandle,
    handheld_controller: &EmulatedControllerHandle,
    trigger_type: ControllerTriggerType,
) {
    let enabled = *common::settings::values().controller_navigation.get_value();
    if !enabled {
        return;
    }

    match trigger_type {
        ControllerTriggerType::Button => {
            controller_update_button(state, player_1_controller, handheld_controller)
        }
        ControllerTriggerType::Stick => {
            controller_update_stick(state, player_1_controller, handheld_controller)
        }
        _ => {}
    }
}

fn controller_update_button(
    state: &Mutex<NavigationState>,
    player_1_controller: &EmulatedControllerHandle,
    handheld_controller: &EmulatedControllerHandle,
) {
    let (controller_type, player_1_buttons) = {
        let controller = player_1_controller.lock();
        (
            controller.get_npad_style_index(false),
            controller.get_buttons_values(),
        )
    };
    let handheld_buttons = handheld_controller.lock().get_buttons_values();
    let mut state = state.lock();

    for index in 0..state.button_values.len() {
        let button = player_1_buttons[index].value || handheld_buttons[index].value;
        state.button_values[index].locked = button == state.button_values[index].value;
        state.button_values[index].value = button;
    }

    match controller_type {
        NpadStyleIndex::Fullkey
        | NpadStyleIndex::JoyconDual
        | NpadStyleIndex::Handheld
        | NpadStyleIndex::GameCube => {
            trigger_button(&mut state, native_button::Values::A, NavigationKey::Enter);
            trigger_button(&mut state, native_button::Values::B, NavigationKey::Escape);
            trigger_button(
                &mut state,
                native_button::Values::L,
                NavigationKey::Previous,
            );
            trigger_button(&mut state, native_button::Values::R, NavigationKey::Next);
            trigger_button(&mut state, native_button::Values::Plus, NavigationKey::Menu);
            trigger_button(
                &mut state,
                native_button::Values::DDown,
                NavigationKey::Down,
            );
            trigger_button(
                &mut state,
                native_button::Values::DLeft,
                NavigationKey::Left,
            );
            trigger_button(
                &mut state,
                native_button::Values::DRight,
                NavigationKey::Right,
            );
            trigger_button(&mut state, native_button::Values::DUp, NavigationKey::Up);
        }
        NpadStyleIndex::JoyconLeft => {
            trigger_button(
                &mut state,
                native_button::Values::DDown,
                NavigationKey::Enter,
            );
            trigger_button(
                &mut state,
                native_button::Values::DLeft,
                NavigationKey::Escape,
            );
        }
        NpadStyleIndex::JoyconRight => {
            trigger_button(&mut state, native_button::Values::X, NavigationKey::Enter);
            trigger_button(&mut state, native_button::Values::A, NavigationKey::Escape);
        }
        _ => {}
    }
}

fn trigger_button(
    state: &mut NavigationState,
    native_button: native_button::Values,
    key: NavigationKey,
) {
    let button = state.button_values[native_button as usize];
    if button.value && !button.locked {
        state.pending_keys.push_back(key);
    }
}

fn controller_update_stick(
    state: &Mutex<NavigationState>,
    player_1_controller: &EmulatedControllerHandle,
    _handheld_controller: &EmulatedControllerHandle,
) {
    let (controller_type, player_1_sticks) = {
        let controller = player_1_controller.lock();
        (
            controller.get_npad_style_index(false),
            controller.get_sticks_values(),
        )
    };

    // This deliberately follows upstream: its `handheld_sticks` reference is
    // currently obtained from `player1_controller`, not `handheld_controller`.
    let handheld_sticks = player_1_controller.lock().get_sticks_values();
    let mut state = state.lock();
    let mut update = false;

    for index in 0..state.stick_values.len() {
        let stick = StickStatus {
            left: player_1_sticks[index].left || handheld_sticks[index].left,
            right: player_1_sticks[index].right || handheld_sticks[index].right,
            up: player_1_sticks[index].up || handheld_sticks[index].up,
            down: player_1_sticks[index].down || handheld_sticks[index].down,
            ..StickStatus::default()
        };
        if stick.down != state.stick_values[index].down
            || stick.left != state.stick_values[index].left
            || stick.right != state.stick_values[index].right
            || stick.up != state.stick_values[index].up
        {
            update = true;
        }
        state.stick_values[index] = stick;
    }

    if !update {
        return;
    }
    if let Some(key) = stick_navigation_key(controller_type, &state.stick_values) {
        state.pending_keys.push_back(key);
    }
}

fn vertical_direction(state: &NavigationState, style: NpadStyleIndex) -> Option<NavigationKey> {
    let standard = matches!(style, NpadStyleIndex::Fullkey | NpadStyleIndex::JoyconDual
        | NpadStyleIndex::Handheld | NpadStyleIndex::GameCube);
    let stick = stick_navigation_key(style, &state.stick_values);
    let up = (standard && state.button_values[native_button::Values::DUp as usize].value)
        || stick == Some(NavigationKey::Up);
    let down = (standard && state.button_values[native_button::Values::DDown as usize].value)
        || stick == Some(NavigationKey::Down);
    match (up, down) {
        (true, false) => Some(NavigationKey::Up),
        (false, true) => Some(NavigationKey::Down),
        _ => None,
    }
}

fn stick_navigation_key(
    controller_type: NpadStyleIndex,
    stick_values: &[StickStatus],
) -> Option<NavigationKey> {
    match controller_type {
        NpadStyleIndex::Fullkey
        | NpadStyleIndex::JoyconDual
        | NpadStyleIndex::Handheld
        | NpadStyleIndex::GameCube => {
            let stick = stick_values[native_analog::Values::LStick as usize];
            if stick.down {
                Some(NavigationKey::Down)
            } else if stick.left {
                Some(NavigationKey::Left)
            } else if stick.right {
                Some(NavigationKey::Right)
            } else if stick.up {
                Some(NavigationKey::Up)
            } else {
                None
            }
        }
        NpadStyleIndex::JoyconLeft => {
            let stick = stick_values[native_analog::Values::LStick as usize];
            if stick.left {
                Some(NavigationKey::Down)
            } else if stick.up {
                Some(NavigationKey::Left)
            } else if stick.down {
                Some(NavigationKey::Right)
            } else if stick.right {
                Some(NavigationKey::Up)
            } else {
                None
            }
        }
        NpadStyleIndex::JoyconRight => {
            let stick = stick_values[native_analog::Values::RStick as usize];
            if stick.right {
                Some(NavigationKey::Down)
            } else if stick.down {
                Some(NavigationKey::Left)
            } else if stick.up {
                Some(NavigationKey::Right)
            } else if stick.left {
                Some(NavigationKey::Up)
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "controller_navigation_tests.rs"]
mod tests;
