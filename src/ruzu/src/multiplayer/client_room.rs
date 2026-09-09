// Port of `yuzu/multiplayer/client_room.cpp` and `chat_room.cpp`
// (`ClientRoomWindow` and the `ChatRoom` widget it embeds).
//
// Upstream splits the window from the chat widget because the same widget is
// reused by the host dialog. Hosting is not ported, so both live here for now;
// the split can come back with `host_room`.
//
// Every RoomMember callback runs on the receive thread. GTK is not thread safe,
// so each one only pushes onto a queue and the UI drains it from a timeout on
// the main loop — upstream gets the same effect from queued Qt signals.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gtk::glib;
use gtk::prelude::*;

use network::room::StatusMessageTypes;
use network::room_member::{
    ChatEntry, RoomMember, RoomMemberError, RoomMemberState, StatusMessageEntry,
};

/// Upstream `ChatRoom::MaxChatMessageLength`.
const MAX_CHAT_MESSAGE_LENGTH: usize = 500;

/// One line as it should appear in the log.
enum RoomEvent {
    Chat(ChatEntry),
    Status(StatusMessageEntry),
    Members(Vec<String>),
    Disconnected,
    Error(RoomMemberError),
}

/// Upstream `ChatRoom::AppendStatusMessage` wording.
fn status_line(entry: &StatusMessageEntry) -> String {
    let who = if entry.username.is_empty() {
        entry.nickname.clone()
    } else {
        format!("{} ({})", entry.nickname, entry.username)
    };
    match entry.message_type {
        StatusMessageTypes::IdMemberJoin => format!("{who} has joined"),
        StatusMessageTypes::IdMemberLeave => format!("{who} has left"),
        StatusMessageTypes::IdMemberKicked => format!("{who} has been kicked"),
        StatusMessageTypes::IdMemberBanned => format!("{who} has been banned"),
        StatusMessageTypes::IdAddressUnbanned => format!("{who} has been unbanned"),
    }
}

/// Upstream `ChatRoom::AppendChatMessage`.
fn chat_line(entry: &ChatEntry) -> String {
    if entry.username.is_empty() {
        format!("<{}> {}", entry.nickname, entry.message)
    } else {
        format!(
            "<{} ({})> {}",
            entry.nickname, entry.username, entry.message
        )
    }
}

/// Shows the room window. Maps to upstream `ClientRoomWindow::ClientRoomWindow`.
pub fn show(parent: &gtk::ApplicationWindow, room_member: Arc<RoomMember>) {
    let info = room_member.get_room_information();
    let dialog = gtk::Window::builder()
        .title(if info.name.is_empty() {
            "Current Room".to_string()
        } else {
            format!("{} ({})", info.name, info.member_slots)
        })
        .transient_for(parent)
        .default_width(640)
        .default_height(460)
        .build();
    crate::hotkeys::install_secondary_window_shortcuts(&dialog, parent);

    let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
    column.set_margin_top(12);
    column.set_margin_bottom(12);
    column.set_margin_start(12);
    column.set_margin_end(12);

    let description = gtk::Label::new(Some(&info.description));
    description.set_xalign(0.0);
    description.set_wrap(true);
    description.add_css_class("dim-label");
    column.append(&description);

    let panes = gtk::Paned::new(gtk::Orientation::Horizontal);
    panes.set_vexpand(true);

    let log = gtk::TextView::new();
    log.set_editable(false);
    log.set_cursor_visible(false);
    log.set_wrap_mode(gtk::WrapMode::WordChar);
    let log_scroller = gtk::ScrolledWindow::builder()
        .child(&log)
        .hexpand(true)
        .build();
    panes.set_start_child(Some(&log_scroller));

    let members = gtk::ListBox::new();
    let members_scroller = gtk::ScrolledWindow::builder()
        .child(&members)
        .width_request(180)
        .build();
    panes.set_end_child(Some(&members_scroller));
    panes.set_resize_end_child(false);
    column.append(&panes);

    let entry_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let message = gtk::Entry::builder()
        .placeholder_text("Say something")
        .hexpand(true)
        .max_length(MAX_CHAT_MESSAGE_LENGTH as i32)
        .build();
    let send = gtk::Button::with_label("Send");
    entry_row.append(&message);
    entry_row.append(&send);
    column.append(&entry_row);

    dialog.set_child(Some(&column));

    // Upstream disables the input while not connected; same rule here.
    let connected = room_member.is_connected();
    message.set_sensitive(connected);
    send.set_sensitive(connected);

    let send_message = {
        let message = message.clone();
        let room_member = Arc::clone(&room_member);
        move || {
            let text = message.text().to_string();
            if text.trim().is_empty() {
                return;
            }
            room_member.send_chat_message(&text);
            message.set_text("");
        }
    };
    {
        let send_message = send_message.clone();
        send.connect_clicked(move |_| send_message());
    }
    {
        let send_message = send_message.clone();
        message.connect_activate(move |_| send_message());
    }

    // The receive thread only enqueues; the UI drains on the main loop.
    let queue: Arc<Mutex<VecDeque<RoomEvent>>> = Arc::new(Mutex::new(VecDeque::new()));

    let chat_handle = {
        let queue = Arc::clone(&queue);
        room_member.bind_on_chat_message_received(move |entry: &ChatEntry| {
            queue
                .lock()
                .unwrap()
                .push_back(RoomEvent::Chat(entry.clone()));
        })
    };
    let status_handle = {
        let queue = Arc::clone(&queue);
        room_member.bind_on_status_message_received(move |entry: &StatusMessageEntry| {
            queue
                .lock()
                .unwrap()
                .push_back(RoomEvent::Status(entry.clone()));
        })
    };
    let info_handle = {
        let queue = Arc::clone(&queue);
        let room_member_for_info = Arc::clone(&room_member);
        room_member.bind_on_room_information_changed(move |_info| {
            let names = room_member_for_info
                .get_member_information()
                .into_iter()
                .map(|member| {
                    if member.username.is_empty() {
                        member.nickname
                    } else {
                        format!("{} ({})", member.nickname, member.username)
                    }
                })
                .collect();
            queue.lock().unwrap().push_back(RoomEvent::Members(names));
        })
    };
    let state_handle = {
        let queue = Arc::clone(&queue);
        room_member.bind_on_state_changed(move |state: &RoomMemberState| {
            if *state == RoomMemberState::Idle || *state == RoomMemberState::Uninitialized {
                queue.lock().unwrap().push_back(RoomEvent::Disconnected);
            }
        })
    };
    let error_handle = {
        let queue = Arc::clone(&queue);
        room_member.bind_on_error(move |error| {
            queue.lock().unwrap().push_back(RoomEvent::Error(*error));
        })
    };

    // Keep the handles so the close path can explicitly mirror upstream's
    // `RoomMember::Unbind`. A callback handle is an `Arc`; dropping our clone
    // alone would leave the clone stored by `RoomMember` registered.
    let handles = Rc::new(RefCell::new(Some((
        chat_handle,
        status_handle,
        info_handle,
        state_handle,
        error_handle,
    ))));

    // Seed the member list with what is already known.
    let initial: Vec<String> = room_member
        .get_member_information()
        .into_iter()
        .map(|member| member.nickname)
        .collect();
    queue.lock().unwrap().push_back(RoomEvent::Members(initial));

    glib::timeout_add_local(
        std::time::Duration::from_millis(100),
        glib::clone!(
            #[strong]
            log,
            #[strong]
            members,
            #[strong]
            message,
            #[strong]
            send,
            #[strong]
            queue,
            #[strong]
            dialog,
            move || {
                if !dialog.is_visible() {
                    return glib::ControlFlow::Break;
                }
                let drained: Vec<RoomEvent> = queue.lock().unwrap().drain(..).collect();
                for event in drained {
                    match event {
                        RoomEvent::Chat(entry) => append_line(&log, &chat_line(&entry)),
                        RoomEvent::Status(entry) => append_line(&log, &status_line(&entry)),
                        RoomEvent::Members(names) => {
                            while let Some(child) = members.first_child() {
                                members.remove(&child);
                            }
                            for name in names {
                                let label = gtk::Label::new(Some(&name));
                                label.set_xalign(0.0);
                                label.set_margin_start(6);
                                label.set_margin_end(6);
                                members.append(&label);
                            }
                        }
                        RoomEvent::Disconnected => {
                            append_line(&log, "Disconnected from the room");
                            message.set_sensitive(false);
                            send.set_sensitive(false);
                        }
                        RoomEvent::Error(error) => {
                            super::message::ErrorManager::show_error(
                                Some(dialog.upcast_ref()),
                                super::message::ErrorManager::for_room_member_error(error),
                            );
                        }
                    }
                }
                glib::ControlFlow::Continue
            }
        ),
    );

    dialog.connect_close_request(move |_| {
        if let Some((chat, status, info, state, error)) = handles.borrow_mut().take() {
            room_member.unbind_on_chat_message_received(&chat);
            room_member.unbind_on_status_message_received(&status);
            room_member.unbind_on_room_information_changed(&info);
            room_member.unbind_on_state_changed(&state);
            room_member.unbind_on_error(&error);
        }
        glib::Propagation::Proceed
    });

    dialog.present();
}

fn append_line(log: &gtk::TextView, line: &str) {
    let buffer = log.buffer();
    let mut end = buffer.end_iter();
    if buffer.char_count() > 0 {
        buffer.insert(&mut end, "\n");
    }
    buffer.insert(&mut end, line);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_line_includes_the_web_username_only_when_present() {
        let mut entry = ChatEntry {
            nickname: "nick".into(),
            username: String::new(),
            message: "hi".into(),
        };
        assert_eq!(chat_line(&entry), "<nick> hi");
        entry.username = "web".into();
        assert_eq!(chat_line(&entry), "<nick (web)> hi");
    }

    #[test]
    fn status_lines_match_upstream_wording() {
        let entry = |message_type| StatusMessageEntry {
            message_type,
            nickname: "nick".into(),
            username: String::new(),
        };
        assert_eq!(
            status_line(&entry(StatusMessageTypes::IdMemberJoin)),
            "nick has joined"
        );
        assert_eq!(
            status_line(&entry(StatusMessageTypes::IdMemberLeave)),
            "nick has left"
        );
        assert_eq!(
            status_line(&entry(StatusMessageTypes::IdMemberKicked)),
            "nick has been kicked"
        );
        assert_eq!(
            status_line(&entry(StatusMessageTypes::IdMemberBanned)),
            "nick has been banned"
        );
    }
}
