// Port of `yuzu/multiplayer/direct_connect.cpp/.h/.ui` (`DirectConnectWindow`).
//
// Collects the four fields upstream asks for — nickname, address, port,
// password — validates them the way `Validation` does, persists the last used
// values, and hands them to `RoomMember::Join` on a worker thread so the GTK
// main loop is never blocked by the 5 s connection timeout.

use std::rc::Rc;
use std::sync::Arc;

use gtk::glib;
use gtk::prelude::*;

use network::room_member::{RoomMember, RoomMemberState};

use super::validation::{is_valid_address, is_valid_nickname, parse_port};
use crate::configuration::shared_widget as w;

/// Shows the modal dialog. `on_joined` runs once the member reaches a connected
/// state so the caller can open the room window, matching upstream's
/// `connect(room_member, &RoomMember::StateChanged, ...)` in `main.cpp`.
pub fn show(
    parent: &gtk::ApplicationWindow,
    room_member: Arc<RoomMember>,
    on_joined: impl Fn() + 'static,
) {
    let dialog = gtk::Window::builder()
        .title("Direct Connect to Room")
        .transient_for(parent)
        .modal(true)
        .default_width(420)
        .build();

    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.set_margin_top(16);
    column.set_margin_bottom(16);
    column.set_margin_start(16);
    column.set_margin_end(16);

    let nickname_value = crate::uisettings::with(|v| v.multiplayer_nickname.get_value().clone());
    let nickname_value = if nickname_value.is_empty() {
        let web_username = common::settings::values().eden_username.get_value().clone();
        if web_username.is_empty() {
            nickname_value
        } else {
            web_username
        }
    } else {
        nickname_value
    };
    let (nickname_row, nickname) = w::entry_row("Nickname:", &nickname_value);
    let (address_row, address) = w::entry_row(
        "IP Address:",
        &crate::uisettings::with(|v| v.multiplayer_ip.get_value().clone()),
    );
    let (port_row, port) = w::entry_row(
        "Port:",
        &crate::uisettings::with(|v| *v.multiplayer_port.get_value()).to_string(),
    );
    let (password_row, password) = w::entry_row("Password:", "");
    password.set_visibility(false);

    for row in [&nickname_row, &address_row, &port_row, &password_row] {
        column.append(row);
    }

    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    column.append(&status);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let connect = gtk::Button::with_label("Connect");
    connect.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&connect);
    column.append(&buttons);

    dialog.set_child(Some(&column));

    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }

    let on_joined = Rc::new(on_joined);
    connect.connect_clicked(glib::clone!(
        #[strong]
        dialog,
        #[strong]
        nickname,
        #[strong]
        address,
        #[strong]
        port,
        #[strong]
        password,
        #[strong]
        status,
        #[strong]
        connect,
        #[strong]
        room_member,
        #[strong]
        on_joined,
        move |_| {
            let current_state = room_member.get_state();
            if current_state == RoomMemberState::Joining {
                status.set_text("A room connection is already in progress.");
                return;
            }
            if room_member.is_connected() {
                status.set_text("Leave the current room before connecting to another one.");
                return;
            }

            if ruzu_core::internal_network::network_interface::get_selected_network_interface()
                .is_none()
            {
                super::message::ErrorManager::show_error(
                    Some(dialog.upcast_ref()),
                    &super::message::ErrorManager::NO_INTERFACE_SELECTED,
                );
                return;
            }

            let nickname_text = nickname.text().to_string();
            let address_text = address.text().to_string();
            let password_text = password.text().to_string();

            if !is_valid_nickname(&nickname_text) {
                super::message::ErrorManager::show_error(
                    Some(dialog.upcast_ref()),
                    &super::message::ErrorManager::USERNAME_NOT_VALID,
                );
                return;
            }
            if !is_valid_address(&address_text) {
                super::message::ErrorManager::show_error(
                    Some(dialog.upcast_ref()),
                    &super::message::ErrorManager::IP_ADDRESS_NOT_VALID,
                );
                return;
            }
            let Some(port_value) = parse_port(&port.text()) else {
                super::message::ErrorManager::show_error(
                    Some(dialog.upcast_ref()),
                    &super::message::ErrorManager::PORT_NOT_VALID,
                );
                return;
            };

            crate::uisettings::with_mut(|v| {
                v.multiplayer_nickname.set_value(nickname_text.clone())
            });
            crate::uisettings::with_mut(|v| v.multiplayer_ip.set_value(address_text.clone()));
            crate::uisettings::with_mut(|v| v.multiplayer_port.set_value(port_value as u32));
            if let Err(error) = crate::configuration::qt_config::save_multiplayer_values() {
                log::error!("Could not save multiplayer settings: {error}");
            }

            status.set_text("Connecting…");
            connect.set_sensitive(false);

            // Join blocks for up to CONNECTION_TIMEOUT_MS. Upstream runs it on
            // a QFuture for the same reason.
            let (state_sender, state_receiver) = std::sync::mpsc::channel();
            let state_handle = room_member.bind_on_state_changed(move |state| {
                let _ = state_sender.send(*state);
            });
            let (error_sender, error_receiver) = std::sync::mpsc::channel();
            let error_handle = room_member.bind_on_error(move |error| {
                let _ = error_sender.send(*error);
            });
            {
                let room_member: Arc<RoomMember> = Arc::clone(&room_member);
                std::thread::Builder::new()
                    .name("DirectConnect".to_string())
                    .spawn(move || {
                        room_member.join(
                            &nickname_text,
                            &address_text,
                            port_value,
                            0,
                            &network::room::NO_PREFERRED_IP,
                            &password_text,
                            "",
                        );
                    })
                    .expect("failed to spawn the DirectConnect thread");
            }

            glib::timeout_add_local(
                std::time::Duration::from_millis(100),
                glib::clone!(
                    #[strong]
                    dialog,
                    #[strong]
                    status,
                    #[strong]
                    connect,
                    #[strong]
                    on_joined,
                    #[strong]
                    room_member,
                    move || {
                        if let Ok(error) = error_receiver.try_recv() {
                            room_member.unbind_on_state_changed(&state_handle);
                            room_member.unbind_on_error(&error_handle);
                            status.set_text("Could not connect to the room.");
                            connect.set_sensitive(true);
                            super::message::ErrorManager::show_error(
                                Some(dialog.upcast_ref()),
                                super::message::ErrorManager::for_room_member_error(error),
                            );
                            return glib::ControlFlow::Break;
                        }

                        match state_receiver.try_recv() {
                            Ok(state) => match state {
                                RoomMemberState::Joining => glib::ControlFlow::Continue,
                                RoomMemberState::Joined | RoomMemberState::Moderator => {
                                    room_member.unbind_on_state_changed(&state_handle);
                                    room_member.unbind_on_error(&error_handle);
                                    on_joined();
                                    dialog.close();
                                    glib::ControlFlow::Break
                                }
                                RoomMemberState::Idle | RoomMemberState::Uninitialized => {
                                    status.set_text("Could not connect to the room.");
                                    glib::ControlFlow::Continue
                                }
                            },
                            Err(std::sync::mpsc::TryRecvError::Empty) => {
                                glib::ControlFlow::Continue
                            }
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                room_member.unbind_on_state_changed(&state_handle);
                                room_member.unbind_on_error(&error_handle);
                                status.set_text("Could not connect to the room.");
                                connect.set_sensitive(true);
                                super::message::ErrorManager::show_error(
                                    Some(dialog.upcast_ref()),
                                    &super::message::ErrorManager::UNABLE_TO_CONNECT,
                                );
                                glib::ControlFlow::Break
                            }
                        }
                    }
                ),
            );
        }
    ));

    dialog.present();
}
