// SPDX-FileCopyrightText: Copyright 2020 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! GTK counterpart of `yuzu/applets/qt_controller.{h,cpp}`.
//!
//! The HLE applet runs on the emulation thread. Requests cross a channel and
//! the selector itself remains owned by GTK's main thread, matching upstream's
//! queued Qt signal connection.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use common::settings_enums::ConsoleMode;
use common::settings_input::ControllerType;
use gtk::prelude::*;
use gtk::{glib, ResponseType};
use hid_core::hid_core::{EmulatedControllerHandle, HIDCore, AVAILABLE_CONTROLLERS};
use hid_core::hid_types::{NpadIdType, NpadStyleIndex, NpadStyleSet, NpadStyleTag};
use parking_lot::Mutex;
use ruzu_core::frontend::applets::applet::Applet;
use ruzu_core::frontend::applets::controller::{
    ControllerApplet, ControllerParameters, ReconfigureCallback,
};

use crate::util::controller_navigation::{ControllerNavigation, NavigationKey};

const NUM_PLAYERS: usize = AVAILABLE_CONTROLLERS - 2;

pub(crate) enum ControllerAppletRequest {
    Reconfigure {
        callback: ReconfigureCallback,
        parameters: ControllerParameters,
    },
    Close,
}

/// Frontend object installed into `FrontendAppletHolder` for GUI boots.
pub(crate) struct GtkControllerSelector {
    sender: Sender<ControllerAppletRequest>,
}

impl GtkControllerSelector {
    pub(crate) fn new() -> (Arc<Self>, Receiver<ControllerAppletRequest>) {
        let (sender, receiver) = mpsc::channel();
        (Arc::new(Self { sender }), receiver)
    }
}

impl Applet for GtkControllerSelector {
    fn close(&self) {
        let _ = self.sender.send(ControllerAppletRequest::Close);
    }
}

impl ControllerApplet for GtkControllerSelector {
    fn reconfigure_controllers(
        &self,
        callback: ReconfigureCallback,
        parameters: &ControllerParameters,
    ) {
        log::info!(
            "Controller selector request: players={}..{} single={} keep={}",
            parameters.min_players,
            parameters.max_players,
            parameters.enable_single_mode,
            parameters.keep_controllers_connected
        );
        if self
            .sender
            .send(ControllerAppletRequest::Reconfigure {
                callback,
                parameters: parameters.clone(),
            })
            .is_err()
        {
            log::error!("Controller selector request receiver is no longer available");
        }
    }
}

struct ActiveDialog {
    dialog: gtk::Dialog,
    callback: Rc<RefCell<Option<ReconfigureCallback>>>,
    state: Rc<ControllerSelectorDialog>,
}

/// GTK-main-thread owner corresponding to the signal connections between
/// upstream `QtControllerSelector` and `GMainWindow`.
pub(crate) struct ControllerAppletFrontend {
    parent: gtk::ApplicationWindow,
    hid_core: Arc<Mutex<HIDCore>>,
    input_subsystem: Rc<RefCell<input_common::InputSubsystem>>,
    receiver: Receiver<ControllerAppletRequest>,
    active: RefCell<Option<ActiveDialog>>,
}

impl ControllerAppletFrontend {
    pub(crate) fn new(
        parent: &gtk::ApplicationWindow,
        hid_core: Arc<Mutex<HIDCore>>,
        input_subsystem: Rc<RefCell<input_common::InputSubsystem>>,
        receiver: Receiver<ControllerAppletRequest>,
    ) -> Rc<Self> {
        Rc::new(Self {
            parent: parent.clone(),
            hid_core,
            input_subsystem,
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

    fn handle_request(self: &Rc<Self>, request: ControllerAppletRequest) {
        match request {
            ControllerAppletRequest::Close => self.finish_active(false, false),
            ControllerAppletRequest::Reconfigure {
                callback,
                parameters,
            } => {
                log::info!("Opening GTK controller selector dialog");
                self.finish_active(false, false);
                self.open_dialog(callback, parameters);
            }
        }
    }

    fn open_dialog(
        self: &Rc<Self>,
        callback: ReconfigureCallback,
        parameters: ControllerParameters,
    ) {
        let callback = Rc::new(RefCell::new(Some(callback)));
        let (dialog, state, automatically_accepted) = ControllerSelectorDialog::build(
            &self.parent,
            Arc::clone(&self.hid_core),
            Rc::clone(&self.input_subsystem),
            parameters,
        );

        let weak = Rc::downgrade(self);
        dialog.connect_response(move |_, response| {
            if let Some(this) = weak.upgrade() {
                this.finish_active(response == ResponseType::Accept, true);
            }
        });

        let weak = Rc::downgrade(self);
        dialog.connect_close_request(move |_| {
            if let Some(this) = weak.upgrade() {
                this.finish_active(false, true);
            }
            glib::Propagation::Proceed
        });

        *self.active.borrow_mut() = Some(ActiveDialog {
            dialog: dialog.clone(),
            callback,
            state,
        });

        if automatically_accepted {
            let weak = Rc::downgrade(self);
            glib::idle_add_local_once(move || {
                if let Some(this) = weak.upgrade() {
                    this.finish_active(true, true);
                }
            });
        } else {
            dialog.present();
        }
    }

    fn finish_active(&self, accepted: bool, invoke_callback: bool) {
        let Some(active) = self.active.borrow_mut().take() else {
            return;
        };

        if accepted {
            active.state.apply_configuration();
        }
        self.hid_core.lock().disable_all_controller_configuration();
        active.dialog.close();

        if invoke_callback {
            if let Some(callback) = active.callback.borrow_mut().take() {
                callback(accepted);
            }
        } else {
            active.callback.borrow_mut().take();
        }
    }
}

struct PlayerRow {
    connected: gtk::CheckButton,
    connected_strip: gtk::CheckButton,
    card: gtk::Frame,
    icon: gtk::DrawingArea,
    player_label: gtk::Label,
    leds: [gtk::CheckButton; 4],
    controller: gtk::DropDown,
    controller_types: Vec<NpadStyleIndex>,
}

struct ControllerSelectorDialog {
    parameters: ControllerParameters,
    hid_core: Arc<Mutex<HIDCore>>,
    docked: gtk::CheckButton,
    handheld: gtk::CheckButton,
    vibration: gtk::CheckButton,
    motion: gtk::CheckButton,
    rows: Vec<PlayerRow>,
    controller_navigation: RefCell<Option<ControllerNavigation>>,
    error: gtk::Label,
    ok_button: gtk::Widget,
    updating: Cell<bool>,
}

impl ControllerSelectorDialog {
    fn build(
        parent: &gtk::ApplicationWindow,
        hid_core: Arc<Mutex<HIDCore>>,
        input_subsystem: Rc<RefCell<input_common::InputSubsystem>>,
        parameters: ControllerParameters,
    ) -> (gtk::Dialog, Rc<Self>, bool) {
        install_controller_applet_style(&parameters);
        let dialog = gtk::Dialog::builder()
            .title(&crate::i18n::tr("Controller Applet"))
            .transient_for(parent)
            .modal(true)
            .resizable(true)
            .default_width(839)
            .build();
        // Upstream keeps `closeButtons` (the error label plus the button box) inside
        // `bottomControllerApplet`, not in a separate action row, so the buttons are
        // built here and appended to the footer further down.
        let cancel_button = gtk::Button::with_label(&crate::i18n::tr("Cancel"));
        let ok_button = gtk::Button::with_label(&crate::i18n::tr("OK"));
        cancel_button.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| dialog.response(ResponseType::Cancel)
        ));
        ok_button.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| dialog.response(ResponseType::Accept)
        ));
        // Upstream marks the button box's OK button as the dialog default, so Return
        // confirms. `set_default_response` cannot be used here: it resolves the action
        // widget registered for a response id, and these buttons live in the footer
        // rather than in the dialog's action area. The default widget is set directly
        // instead, once the tree is built.
        ok_button.set_receives_default(true);

        // Upstream `verticalLayout` has zero margins and no spacing: each of the three
        // bands (`topControllerApplet`, `middleControllerApplet`, `bottomControllerApplet`)
        // carries its own padding and they are flush against each other.
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let player_range = player_range(&parameters);

        // Upstream `topControllerApplet`: label, five fixed controller images,
        // then the accepted player count.
        let supported_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        // Upstream `horizontalLayout` has zero left/right margins and centres its
        // content between `controllerAppletHorizontalSpacer2` and `...Spacer3`, so the
        // band itself spans the full dialog width.
        let leading_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        leading_spacer.set_hexpand(true);
        supported_row.append(&leading_spacer);
        let supported_label =
            gtk::Label::new(Some(&crate::i18n::tr("Supported Controller Types:")));
        supported_label.set_width_chars(11);
        supported_label.set_wrap(true);
        supported_label.set_xalign(1.0);
        supported_label.add_css_class("heading");
        supported_row.append(&supported_label);

        for (style, supported) in [
            (
                NpadStyleIndex::Handheld,
                parameters.enable_single_mode && parameters.allow_handheld,
            ),
            (NpadStyleIndex::JoyconDual, parameters.allow_dual_joycons),
            (NpadStyleIndex::JoyconLeft, parameters.allow_left_joycon),
            (NpadStyleIndex::JoyconRight, parameters.allow_right_joycon),
            (
                NpadStyleIndex::Fullkey,
                parameters.allow_pro_controller || parameters.allow_gamecube_controller,
            ),
        ] {
            supported_row.append(&controller_icon(style, 70, 70, supported));
        }

        let players_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        players_box.set_size_request(80, 70);
        players_box.set_valign(gtk::Align::Center);
        let players_label = gtk::Label::new(Some(&crate::i18n::tr("Players:")));
        players_label.add_css_class("heading");
        let players_value = if player_range.0 == player_range.1 {
            player_range.0.to_string()
        } else {
            format!("{} - {}", player_range.0, player_range.1)
        };
        let players_value = gtk::Label::new(Some(&players_value));
        players_value.add_css_class("title-3");
        players_box.append(&players_label);
        players_box.append(&players_value);
        supported_row.append(&players_box);
        let trailing_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        trailing_spacer.set_hexpand(true);
        supported_row.append(&trailing_spacer);
        supported_row.add_css_class("controller-applet-band");
        content.append(&supported_row);
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let style_tag = hid_core.lock().get_supported_style_tag();
        let players_grid = gtk::Grid::new();
        players_grid.set_row_spacing(16);
        players_grid.set_column_spacing(12);
        players_grid.set_column_homogeneous(true);
        players_grid.set_halign(gtk::Align::Center);
        let mut rows = Vec::new();
        for index in 0..player_range.1 {
            let controller_types = controller_types(style_tag, index);
            let names = controller_types
                .iter()
                .map(|style| crate::i18n::tr(controller_name(*style)))
                .collect::<Vec<_>>();
            let refs = names.iter().map(String::as_str).collect::<Vec<_>>();

            let player = gtk::Box::new(gtk::Orientation::Vertical, 5);
            player.set_width_request(150);

            let card = gtk::Frame::new(None);
            card.add_css_class("controller-applet-card");
            card.add_css_class(&format!("controller-applet-player-{}", index + 1));
            let card_content = gtk::Box::new(gtk::Orientation::Vertical, 3);
            card_content.set_margin_top(4);
            card_content.set_margin_bottom(4);
            card_content.set_margin_start(8);
            card_content.set_margin_end(8);

            // Upstream `groupPlayerNConnected` is a checkable group box with an empty
            // title: the player number is shown only by the centred `labelPlayerN`.
            let connected = gtk::CheckButton::new();
            connected.set_halign(gtk::Align::Start);
            card_content.append(&connected);

            let overlay = gtk::Overlay::new();
            overlay.set_size_request(96, 56);
            let icon = controller_icon(NpadStyleIndex::Fullkey, 96, 56, true);
            // Upstream `labelPlayerN` is a plain QLabel with no font override.
            let player_label = gtk::Label::new(Some(&format!("P{}", index + 1)));
            player_label.set_halign(gtk::Align::Center);
            player_label.set_valign(gtk::Align::Center);
            overlay.set_child(Some(&icon));
            overlay.add_overlay(&player_label);
            card_content.append(&overlay);

            let led_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            led_row.set_halign(gtk::Align::Center);
            let leds = std::array::from_fn(|_| {
                let led = gtk::CheckButton::new();
                led.set_sensitive(false);
                led.add_css_class("controller-applet-led");
                led_row.append(&led);
                led
            });
            card_content.append(&led_row);
            card.set_child(Some(&card_content));
            player.append(&card);

            if parameters.enable_explain_text {
                let explain = gtk::Label::new(Some(&explain_text(&parameters, index)));
                explain.set_wrap(true);
                explain.set_justify(gtk::Justification::Center);
                explain.set_max_width_chars(22);
                player.append(&explain);
            }

            let controller =
                gtk::DropDown::new(Some(gtk::StringList::new(&refs)), gtk::Expression::NONE);
            controller.set_hexpand(true);
            player.append(&controller);

            let profile = gtk::DropDown::from_strings(&[&crate::i18n::tr("Use Current Config")]);
            player.append(&profile);

            players_grid.attach(&player, (index % 4) as i32, (index / 4) as i32, 1, 1);

            rows.push(PlayerRow {
                connected,
                connected_strip: gtk::CheckButton::new(),
                card,
                icon,
                player_label,
                leds,
                controller,
                controller_types,
            });
        }
        // Upstream `verticalLayout_2` uses stretch="0,3,0": only the middle band grows,
        // which keeps the footer pinned to the bottom edge of the dialog.
        players_grid.set_vexpand(true);
        players_grid.set_valign(gtk::Align::Center);
        players_grid.set_margin_top(10);
        players_grid.set_margin_bottom(10);
        players_grid.set_margin_start(20);
        players_grid.set_margin_end(20);
        content.append(&players_grid);
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        footer.add_css_class("controller-applet-band");

        let console = gtk::Frame::new(Some(&crate::i18n::tr("Console Mode")));
        let console_modes = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        console_modes.set_margin_top(6);
        console_modes.set_margin_bottom(6);
        console_modes.set_margin_start(8);
        console_modes.set_margin_end(8);
        let docked = gtk::CheckButton::with_label(&crate::i18n::tr("Docked"));
        let handheld = gtk::CheckButton::with_label(&crate::i18n::tr("Handheld"));
        handheld.set_group(Some(&docked));
        console_modes.append(&docked);
        console_modes.append(&handheld);
        console.set_child(Some(&console_modes));
        footer.append(&console);

        let (vibration_box, vibration, vibration_button) =
            toggle_action(&crate::i18n::tr("Vibration"), &crate::i18n::tr("Configure"));
        footer.append(&vibration_box);
        let (motion_box, motion, motion_button) =
            toggle_action(&crate::i18n::tr("Motion"), &crate::i18n::tr("Configure"));
        footer.append(&motion_box);

        // Upstream `inputConfigGroup`: a non-checkable group box whose single button
        // opens `ConfigureInputProfileDialog`.
        let profiles_group = gtk::Frame::new(Some(&crate::i18n::tr("Profiles")));
        let profiles_content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        profiles_content.set_margin_top(5);
        profiles_content.set_margin_bottom(5);
        profiles_content.set_margin_start(6);
        profiles_content.set_margin_end(6);
        profiles_content.set_valign(gtk::Align::Center);
        let profiles_button = gtk::Button::with_label(&crate::i18n::tr("Create"));
        profiles_content.append(&profiles_button);
        profiles_group.set_child(Some(&profiles_content));
        footer.append(&profiles_group);

        let connected_grid = gtk::Grid::new();
        connected_grid.set_column_spacing(4);
        connected_grid.attach(
            &gtk::Label::new(Some(&crate::i18n::tr("Connected"))),
            0,
            0,
            1,
            1,
        );
        connected_grid.attach(
            &gtk::Label::new(Some(&crate::i18n::tr("Controllers"))),
            0,
            1,
            1,
            1,
        );
        for (index, row) in rows.iter().enumerate() {
            connected_grid.attach(
                &gtk::Label::new(Some(&(index + 1).to_string())),
                index as i32 + 1,
                0,
                1,
                1,
            );
            connected_grid.attach(&row.connected_strip, index as i32 + 1, 1, 1, 1);
        }
        connected_grid.set_valign(gtk::Align::Center);
        connected_grid.set_hexpand(true);
        connected_grid.set_halign(gtk::Align::End);
        footer.append(&connected_grid);

        // Upstream `closeButtons`: `labelError` stacked directly above the button box,
        // both anchored to the right end of the bottom band.
        let close_buttons = gtk::Box::new(gtk::Orientation::Vertical, 4);
        close_buttons.set_valign(gtk::Align::Center);
        let error = gtk::Label::new(Some(&crate::i18n::tr("Not enough controllers")));
        error.add_css_class("error");
        error.set_xalign(1.0);
        error.set_visible(false);
        close_buttons.append(&error);
        let button_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        button_box.set_halign(gtk::Align::End);
        button_box.append(&cancel_button);
        button_box.append(&ok_button);
        close_buttons.append(&button_box);
        footer.append(&close_buttons);

        content.append(&footer);
        content.set_vexpand(true);
        dialog.content_area().append(&content);
        // Now that OK is inside the dialog's widget tree, make it the default so
        // Return activates it, matching upstream's default `QDialogButtonBox` button.
        dialog.set_default_widget(Some(&ok_button));

        let state = Rc::new(Self {
            parameters,
            hid_core,
            docked,
            handheld,
            vibration,
            motion,
            rows,
            controller_navigation: RefCell::new(None),
            error,
            ok_button: ok_button.upcast(),
            updating: Cell::new(false),
        });

        state.load_configuration();
        *state.controller_navigation.borrow_mut() =
            Some(ControllerNavigation::new(&state.hid_core));
        state.connect_signals();

        {
            let weak = Rc::downgrade(&state);
            let dialog = dialog.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
                let Some(state) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                let keys = state
                    .controller_navigation
                    .borrow()
                    .as_ref()
                    .map(ControllerNavigation::take_pending_keys)
                    .unwrap_or_default();
                for key in keys {
                    state.handle_navigation(&dialog, key);
                }
                glib::ControlFlow::Continue
            });
        }

        {
            let hid_core = Arc::clone(&state.hid_core);
            vibration_button.connect_clicked(move |button| {
                crate::configuration::configure_vibration::present(button, Arc::clone(&hid_core));
            });
        }
        // Upstream `CallConfigureInputProfileDialog`. `QtControllerSelectorDialog` owns
        // one `InputProfiles` member (`qt_controller.cpp:68`) shared by every profile
        // dialog it opens, so the context is built once here rather than per click.
        {
            let hid_core = Arc::clone(&state.hid_core);
            let input_subsystem = Rc::clone(&input_subsystem);
            let profile_context = Rc::new(
                crate::configuration::configure_input_player::InputProfileContext::new(
                    crate::configuration::input_profiles::InputProfiles::new(),
                ),
            );
            profiles_button.connect_clicked(move |button| {
                crate::configuration::configure_input_profile_dialog::present(
                    button,
                    Rc::clone(&input_subsystem),
                    Arc::clone(&hid_core),
                    Rc::clone(&profile_context),
                );
            });
        }

        motion_button.connect_clicked(move |button| {
            crate::configuration::configure_motion_touch::present(
                button,
                Rc::clone(&input_subsystem),
            );
        });

        let parameters_met = state.check_if_parameters_met();
        let automatically_accepted = parameters_met && state.parameters.enable_single_mode;

        if !automatically_accepted && !state.parameters.keep_controllers_connected {
            state.set_connected_players(0);
        }

        (dialog, state, automatically_accepted)
    }

    fn load_configuration(&self) {
        self.hid_core.lock().enable_all_controller_configuration();

        let (handheld, controllers) = controller_handles(&self.hid_core);
        let handheld_connected = handheld.lock().is_connected(true);
        for (index, row) in self.rows.iter().enumerate() {
            let controller = controllers[index].lock();
            let connected = controller.is_connected(true) || (index == 0 && handheld_connected);
            row.connected.set_active(connected);
            row.connected_strip.set_active(connected);
            let style = controller.get_npad_style_index(true);
            let selected = row
                .controller_types
                .iter()
                .position(|candidate| *candidate == style)
                .unwrap_or(0);
            row.controller.set_selected(selected as u32);
            drop(controller);
            self.update_player_visuals(index);
        }

        let is_docked =
            *common::settings::values().use_docked_mode.get_value() == ConsoleMode::Docked;
        self.docked.set_active(is_docked && !handheld_connected);
        self.handheld.set_active(!is_docked || handheld_connected);
        self.docked.set_sensitive(!handheld_connected);
        self.handheld.set_sensitive(!handheld_connected);
        self.vibration
            .set_active(*common::settings::values().vibration_enabled.get_value());
        self.motion
            .set_active(*common::settings::values().motion_enabled.get_value());

        let max_players = player_range(&self.parameters).1;
        for index in max_players..NUM_PLAYERS {
            update_controller(&controllers[index], None, false);
        }
    }

    fn connect_signals(self: &Rc<Self>) {
        for (index, row) in self.rows.iter().enumerate() {
            let weak: Weak<Self> = Rc::downgrade(self);
            row.connected.connect_toggled(move |button| {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                state.propagate_connection(index, button.is_active());
            });

            let weak: Weak<Self> = Rc::downgrade(self);
            row.connected_strip.connect_toggled(move |button| {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                state.propagate_connection(index, button.is_active());
            });

            let weak: Weak<Self> = Rc::downgrade(self);
            row.controller.connect_selected_notify(move |_| {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                if state.updating.get() {
                    return;
                }
                state.update_player_visuals(index);
                state.update_controller_state(index);
                if index == 0 {
                    state.update_docked_state();
                }
                state.check_if_parameters_met();
            });
        }
    }

    fn handle_navigation(&self, dialog: &gtk::Dialog, key: NavigationKey) {
        let connected = self
            .rows
            .iter()
            .filter(|row| row.connected.is_active())
            .count();
        let (minimum, maximum) = player_range(&self.parameters);
        match key {
            NavigationKey::Enter => {
                if self.check_if_parameters_met() {
                    dialog.response(ResponseType::Accept);
                } else {
                    self.error.set_visible(true);
                }
            }
            NavigationKey::Escape => dialog.response(ResponseType::Cancel),
            NavigationKey::Left if connected > minimum => self.set_connected_players(connected - 1),
            NavigationKey::Right if connected < maximum => {
                self.set_connected_players(connected + 1)
            }
            NavigationKey::Up => {
                dialog.child_focus(gtk::DirectionType::Up);
            }
            NavigationKey::Down => {
                dialog.child_focus(gtk::DirectionType::Down);
            }
            NavigationKey::Left | NavigationKey::Right => {}
        }
    }

    fn propagate_connection(&self, index: usize, connected: bool) {
        if self.updating.get() {
            return;
        }

        let current = self
            .rows
            .iter()
            .map(|row| row.connected.is_active())
            .collect::<Vec<_>>();
        let propagated = propagated_connection_states(&current, index, connected);

        self.updating.set(true);
        for (index, connected) in propagated.into_iter().enumerate() {
            let row = &self.rows[index];
            row.connected.set_active(connected);
            row.connected_strip.set_active(connected);
            self.update_controller_state(index);
            self.update_player_visuals(index);
        }
        self.update_docked_state();
        self.updating.set(false);
        self.error.set_visible(false);
        self.check_if_parameters_met();
    }

    fn set_connected_players(&self, count: usize) {
        self.updating.set(true);
        for (index, row) in self.rows.iter().enumerate() {
            let connected = index < count;
            row.connected.set_active(connected);
            row.connected_strip.set_active(connected);
        }
        self.apply_all_controller_states();
        for index in 0..self.rows.len() {
            self.update_player_visuals(index);
        }
        self.update_docked_state();
        self.updating.set(false);
        self.check_if_parameters_met();
    }

    fn apply_all_controller_states(&self) {
        for index in 0..self.rows.len() {
            self.update_controller_state(index);
        }
    }

    fn update_controller_state(&self, index: usize) {
        let row = &self.rows[index];
        let selected = row.controller.selected() as usize;
        let controller_type = row
            .controller_types
            .get(selected)
            .copied()
            .unwrap_or(NpadStyleIndex::Fullkey);
        let connected = row.connected.is_active();
        let (handheld, controllers) = controller_handles(&self.hid_core);
        let player_connected = connected && controller_type != NpadStyleIndex::Handheld;

        {
            let controller = controllers[index].lock();
            if controller.get_npad_style_index(true) == controller_type
                && controller.is_connected(true) == player_connected
            {
                return;
            }
        }

        update_controller(&controllers[index], Some(controller_type), false);
        if index == 0 && controller_type == NpadStyleIndex::Handheld {
            update_controller(&handheld, Some(NpadStyleIndex::Handheld), connected);
        }
        update_controller(&controllers[index], Some(controller_type), player_connected);
    }

    fn update_player_visuals(&self, index: usize) {
        let row = &self.rows[index];
        let controller_type = row
            .controller_types
            .get(row.controller.selected() as usize)
            .copied()
            .unwrap_or(NpadStyleIndex::Fullkey);
        let connected = row.connected.is_active();

        configure_controller_icon(&row.icon, controller_type, true);
        row.icon.set_visible(connected);
        row.player_label.set_visible(!connected);
        if connected {
            row.card.add_css_class("connected");
        } else {
            row.card.remove_css_class("connected");
        }

        let led_pattern = if connected && controller_type != NpadStyleIndex::Handheld {
            let (_, controllers) = controller_handles(&self.hid_core);
            let pattern = controllers[index].lock().get_led_pattern().raw;
            pattern
        } else {
            0
        };
        for (led_index, led) in row.leds.iter().enumerate() {
            led.set_active(led_pattern & (1 << led_index) != 0);
        }
    }

    fn update_docked_state(&self) {
        let handheld_selected = self.rows.first().is_some_and(|row| {
            row.controller_types
                .get(row.controller.selected() as usize)
                .copied()
                == Some(NpadStyleIndex::Handheld)
        });
        self.docked.set_sensitive(!handheld_selected);
        self.handheld.set_sensitive(!handheld_selected);
        if handheld_selected {
            self.handheld.set_active(true);
        }
    }

    fn apply_configuration(&self) {
        let mut values = common::settings::values_mut();
        values
            .use_docked_mode
            .set_value(if self.docked.is_active() {
                ConsoleMode::Docked
            } else {
                ConsoleMode::Handheld
            });
        values
            .vibration_enabled
            .set_value(self.vibration.is_active());
        values.motion_enabled.set_value(self.motion.is_active());
    }

    fn check_if_parameters_met(&self) -> bool {
        let (minimum, maximum) = player_range(&self.parameters);
        let connected = self
            .rows
            .iter()
            .filter(|row| row.connected.is_active())
            .count();
        let count_valid = connected >= minimum && connected <= maximum;
        let types_valid = self
            .rows
            .iter()
            .filter(|row| row.connected.is_active())
            .all(|row| {
                row.controller_types
                    .get(row.controller.selected() as usize)
                    .copied()
                    .is_some_and(|style| is_controller_compatible(style, &self.parameters))
            });
        let valid = count_valid && types_valid;
        self.ok_button.set_sensitive(valid);
        valid
    }
}

fn controller_icon(
    controller_type: NpadStyleIndex,
    width: i32,
    height: i32,
    enabled: bool,
) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_content_width(width);
    area.set_content_height(height);
    area.set_size_request(width, height);
    configure_controller_icon(&area, controller_type, enabled);
    area
}

fn configure_controller_icon(
    area: &gtk::DrawingArea,
    controller_type: NpadStyleIndex,
    enabled: bool,
) {
    // Upstream's `*_disabled` artwork is the full-strength outline plus a diagonal
    // stroke, not a faded copy, so only the stroke below marks an unsupported type.
    area.set_opacity(1.0);
    area.set_draw_func(move |widget, cr, width, height| {
        let controller_type = settings_controller_type(controller_type);
        let (art_width, art_height) = match controller_type {
            ControllerType::ProController => (400.0, 300.0),
            ControllerType::DualJoyconDetached => (440.0, 300.0),
            ControllerType::LeftJoycon | ControllerType::RightJoycon => (240.0, 430.0),
            ControllerType::Handheld => (560.0, 320.0),
            ControllerType::GameCube => (400.0, 300.0),
            _ => (400.0, 300.0),
        };
        let scale = (width as f64 / art_width).min(height as f64 / art_height) * 0.9;
        let dark = widget.settings().is_gtk_application_prefer_dark_theme();
        let _ = cr.save();
        cr.translate(width as f64 / 2.0, height as f64 / 2.0);
        cr.scale(scale, scale);
        crate::configuration::controller_preview::draw(
            cr,
            (0.0, 0.0),
            controller_type,
            dark,
            &crate::configuration::controller_preview::Input::released(),
        );
        let _ = cr.restore();

        // Upstream swaps in the `*_disabled` artwork for unsupported types: the same
        // outline crossed out by a diagonal stroke.
        if !enabled {
            let inset = f64::from(width.min(height)) * 0.1;
            if dark {
                cr.set_source_rgba(0.87, 0.87, 0.87, 0.95);
            } else {
                cr.set_source_rgba(0.1, 0.1, 0.1, 0.95);
            }
            cr.set_line_width(3.0);
            cr.move_to(inset, f64::from(height) - inset);
            cr.line_to(f64::from(width) - inset, inset);
            let _ = cr.stroke();
        }
    });
    area.queue_draw();
}

fn settings_controller_type(controller_type: NpadStyleIndex) -> ControllerType {
    match controller_type {
        NpadStyleIndex::Fullkey => ControllerType::ProController,
        NpadStyleIndex::JoyconDual => ControllerType::DualJoyconDetached,
        NpadStyleIndex::JoyconLeft => ControllerType::LeftJoycon,
        NpadStyleIndex::JoyconRight => ControllerType::RightJoycon,
        NpadStyleIndex::Handheld => ControllerType::Handheld,
        NpadStyleIndex::GameCube => ControllerType::GameCube,
        _ => ControllerType::ProController,
    }
}

fn toggle_action(title: &str, action: &str) -> (gtk::Frame, gtk::CheckButton, gtk::Button) {
    let frame = gtk::Frame::new(None);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    content.set_margin_top(5);
    content.set_margin_bottom(5);
    content.set_margin_start(6);
    content.set_margin_end(6);
    let toggle = gtk::CheckButton::with_label(title);
    let button = gtk::Button::with_label(action);
    content.append(&toggle);
    content.append(&button);
    frame.set_child(Some(&content));
    (frame, toggle, button)
}

fn explain_text(parameters: &ControllerParameters, index: usize) -> String {
    let Some(buffer) = parameters.explain_text.get(index) else {
        return String::new();
    };
    let length = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..length]).into_owned()
}

fn install_controller_applet_style(parameters: &ControllerParameters) {
    let mut css = String::from(
        // Upstream pins `groupPlayerNConnected` to a fixed 100x100 via matching
        // minimumSize/maximumSize.
        "frame.controller-applet-card > border { min-width: 100px; min-height: 100px; \
         border: 1px solid alpha(currentColor, 0.22); border-radius: 3px; } \
         frame.controller-applet-card.connected > border { \
         border-color: @accent_color; border-width: 2px; } \
         checkbutton.controller-applet-led { padding: 0; min-width: 10px; min-height: 10px; } \
         box.controller-applet-band { background-color: alpha(currentColor, 0.04); \
         padding: 10px 20px; }",
    );
    if parameters.enable_border_color {
        for (index, color) in parameters
            .border_colors
            .iter()
            .take(NUM_PLAYERS)
            .enumerate()
        {
            css.push_str(&format!(
                " frame.controller-applet-player-{}.connected > border {{ \
                 border-color: rgba({}, {}, {}, {}); }}",
                index + 1,
                color[0],
                color[1],
                color[2],
                color[3] as f64 / 255.0,
            ));
        }
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_data(&css);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn propagated_connection_states(current: &[bool], player_index: usize, checked: bool) -> Vec<bool> {
    let mut propagated = current.to_vec();
    let reconnect_current = !checked && current.get(player_index + 1).copied().unwrap_or(false);

    if checked {
        for connected in &mut propagated[..=player_index] {
            *connected = true;
        }
    } else {
        for connected in &mut propagated[player_index..] {
            *connected = false;
        }
        if reconnect_current {
            propagated[player_index] = true;
        }
    }

    propagated
}

fn controller_handles(
    hid_core: &Arc<Mutex<HIDCore>>,
) -> (EmulatedControllerHandle, Vec<EmulatedControllerHandle>) {
    let hid_core = hid_core.lock();
    let handheld = hid_core.get_emulated_controller(NpadIdType::Handheld);
    let controllers = (0..NUM_PLAYERS)
        .map(|index| hid_core.get_emulated_controller_by_index(index))
        .collect();
    (handheld, controllers)
}

fn update_controller(
    controller: &EmulatedControllerHandle,
    controller_type: Option<NpadStyleIndex>,
    connected: bool,
) {
    let mut controller = controller.lock();
    if controller.is_connected(true) {
        controller.disconnect();
    }
    if let Some(controller_type) = controller_type {
        controller.set_npad_style_index(controller_type);
    }
    if connected {
        controller.connect(true);
    }
}

fn player_range(parameters: &ControllerParameters) -> (usize, usize) {
    if parameters.enable_single_mode {
        return (1, 1);
    }
    let minimum = usize::try_from(parameters.min_players)
        .unwrap_or(0)
        .min(NUM_PLAYERS);
    let maximum = usize::try_from(parameters.max_players)
        .unwrap_or(0)
        .clamp(1, NUM_PLAYERS);
    (minimum.min(maximum), maximum)
}

fn controller_types(style_tag: NpadStyleTag, player_index: usize) -> Vec<NpadStyleIndex> {
    // QtControllerSelectorDialog::SetEmulatedControllers owns this list separately
    // from ConfigureInputPlayer; its missing-selection behavior also differs.
    let mut result = Vec::new();
    let mut add = |flag, style| {
        if style_tag.raw.contains(flag) {
            result.push(style);
        }
    };
    add(NpadStyleSet::FULLKEY, NpadStyleIndex::Fullkey);
    add(NpadStyleSet::JOY_DUAL, NpadStyleIndex::JoyconDual);
    add(NpadStyleSet::JOY_LEFT, NpadStyleIndex::JoyconLeft);
    add(NpadStyleSet::JOY_RIGHT, NpadStyleIndex::JoyconRight);
    if player_index == 0 {
        add(NpadStyleSet::HANDHELD, NpadStyleIndex::Handheld);
    }
    add(NpadStyleSet::GC, NpadStyleIndex::GameCube);
    if *common::settings::values().enable_all_controllers.get_value() {
        add(NpadStyleSet::PALMA, NpadStyleIndex::Pokeball);
        add(NpadStyleSet::LARK, NpadStyleIndex::NES);
        add(NpadStyleSet::LUCIA, NpadStyleIndex::SNES);
        add(NpadStyleSet::LAGOON, NpadStyleIndex::N64);
        add(NpadStyleSet::LAGER, NpadStyleIndex::SegaGenesis);
    }
    result
}

fn is_controller_compatible(
    controller_type: NpadStyleIndex,
    parameters: &ControllerParameters,
) -> bool {
    match controller_type {
        NpadStyleIndex::Fullkey => parameters.allow_pro_controller,
        NpadStyleIndex::JoyconDual => parameters.allow_dual_joycons,
        NpadStyleIndex::JoyconLeft => parameters.allow_left_joycon,
        NpadStyleIndex::JoyconRight => parameters.allow_right_joycon,
        NpadStyleIndex::Handheld => parameters.enable_single_mode && parameters.allow_handheld,
        NpadStyleIndex::GameCube => parameters.allow_gamecube_controller,
        _ => false,
    }
}

fn controller_name(controller_type: NpadStyleIndex) -> &'static str {
    match controller_type {
        NpadStyleIndex::Fullkey => "Pro Controller",
        NpadStyleIndex::JoyconDual => "Dual Joycons",
        NpadStyleIndex::JoyconLeft => "Left Joycon",
        NpadStyleIndex::JoyconRight => "Right Joycon",
        NpadStyleIndex::Handheld => "Handheld",
        NpadStyleIndex::GameCube => "GameCube Controller",
        NpadStyleIndex::Pokeball => "Poke Ball Plus",
        NpadStyleIndex::NES => "NES Controller",
        NpadStyleIndex::SNES => "SNES Controller",
        NpadStyleIndex::N64 => "N64 Controller",
        NpadStyleIndex::SegaGenesis => "Sega Genesis",
        _ => "Pro Controller",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn empty_supported_styles_do_not_add_an_unsupported_controller() {
        for player in 0..8 {
            assert!(super::controller_types(super::NpadStyleTag {
                raw: super::NpadStyleSet::empty(),
            }, player).is_empty());
        }
    }
    use super::*;

    #[test]
    fn single_mode_overrides_player_limits() {
        let parameters = ControllerParameters {
            min_players: 4,
            max_players: 8,
            enable_single_mode: true,
            ..Default::default()
        };
        assert_eq!(player_range(&parameters), (1, 1));
    }

    #[test]
    fn compatibility_matches_upstream_controller_applet_rules() {
        let parameters = ControllerParameters {
            allow_left_joycon: true,
            allow_right_joycon: true,
            ..Default::default()
        };
        assert!(is_controller_compatible(
            NpadStyleIndex::JoyconLeft,
            &parameters
        ));
        assert!(is_controller_compatible(
            NpadStyleIndex::JoyconRight,
            &parameters
        ));
        assert!(!is_controller_compatible(
            NpadStyleIndex::Fullkey,
            &parameters
        ));
    }

    #[test]
    fn applet_request_keeps_callback_for_gui_thread() {
        let (selector, receiver) = GtkControllerSelector::new();
        selector.reconfigure_controllers(Box::new(|_| {}), &ControllerParameters::default());
        assert!(matches!(
            receiver.recv().unwrap(),
            ControllerAppletRequest::Reconfigure { .. }
        ));
    }

    #[test]
    fn player_propagation_matches_upstream_sequential_connection_rules() {
        assert_eq!(
            propagated_connection_states(&[true, false, false], 2, true),
            [true, true, true]
        );
        assert_eq!(
            propagated_connection_states(&[true, true, true], 1, false),
            [true, true, false]
        );
        assert_eq!(
            propagated_connection_states(&[true, true, false], 1, false),
            [true, false, false]
        );
    }

    #[test]
    fn applet_styles_map_to_the_matching_preview_art() {
        assert_eq!(
            settings_controller_type(NpadStyleIndex::Fullkey),
            ControllerType::ProController
        );
        assert_eq!(
            settings_controller_type(NpadStyleIndex::JoyconDual),
            ControllerType::DualJoyconDetached
        );
        assert_eq!(
            settings_controller_type(NpadStyleIndex::JoyconLeft),
            ControllerType::LeftJoycon
        );
        assert_eq!(
            settings_controller_type(NpadStyleIndex::JoyconRight),
            ControllerType::RightJoycon
        );
        assert_eq!(
            settings_controller_type(NpadStyleIndex::Handheld),
            ControllerType::Handheld
        );
    }

    #[test]
    fn explain_text_stops_at_the_upstream_nul_terminator() {
        let mut parameters = ControllerParameters::default();
        let mut text = [0u8; 0x81];
        text[..8].copy_from_slice(b"Player 1");
        text[9] = b'X';
        parameters.explain_text.push(text);
        assert_eq!(explain_text(&parameters, 0), "Player 1");
        assert!(explain_text(&parameters, 1).is_empty());
    }
}
