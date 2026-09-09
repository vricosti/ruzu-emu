// Port of `yuzu/multiplayer/lobby.cpp`, `lobby_p.h`, and `lobby.ui` (`Lobby`).
//
// GTK's `ColumnView` + `TreeListModel` represents Eden's `QTreeView`: each
// announced room is a root row, with its description and member activity as
// expandable child rows. Game icons come from the same local game-list model
// that Eden searches by program id.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib};

use common::announce_multiplayer_room::{Room, RoomList};
use network::room_member::{RoomMember, RoomMemberState};

/// Upstream `LobbyItemGame` falls back to `QIcon::fromTheme("chip").pixmap(32)`
/// when the announced game is not in the local game list. That `chip` icon is
/// bundled with Eden's Qt themes rather than provided by the host icon theme,
/// so keep a local copy like the game list does for `folder` and `star`.
const CHIP_ICON_PNG: &[u8] = include_bytes!("../../assets/lobby-chip.png");

/// Upstream scales the decoration to 32x32 in `LobbyItemGame::data`.
const GAME_ICON_SIZE: i32 = 32;

/// Character budget used to wrap the expandable description and member-activity
/// rows. See the comment in `preferred_game_column`.
const DETAIL_WRAP_CHARS: i32 = 80;

mod row_imp {
    use std::cell::{Cell, RefCell};

    use gtk::subclass::prelude::*;
    use gtk::{gdk, gio, glib};

    #[derive(Default)]
    pub struct LobbyEntry {
        pub room_index: Cell<u32>,
        pub is_room: Cell<bool>,
        pub preferred_game: RefCell<String>,
        pub room_name: RefCell<String>,
        pub players: RefCell<String>,
        pub member_count: Cell<u32>,
        pub member_slots: Cell<u32>,
        pub host: RefCell<String>,
        pub has_password: Cell<bool>,
        pub icon: RefCell<Option<gdk::Texture>>,
        pub children: RefCell<Option<gio::ListStore>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LobbyEntry {
        const NAME: &'static str = "RedenLobbyEntry";
        type Type = super::LobbyEntry;
    }

    impl ObjectImpl for LobbyEntry {}
}

glib::wrapper! {
    pub struct LobbyEntry(ObjectSubclass<row_imp::LobbyEntry>);
}

impl LobbyEntry {
    fn room(index: usize, room: &Room, icon: Option<gdk::Texture>) -> Self {
        let entry: Self = glib::Object::new();
        let imp = entry.imp();
        imp.room_index.set(index as u32);
        imp.is_room.set(true);
        *imp.preferred_game.borrow_mut() = room.information.preferred_game.name.clone();
        *imp.room_name.borrow_mut() = room.information.name.clone();
        *imp.players.borrow_mut() =
            format!("{} / {}", room.members.len(), room.information.member_slots);
        imp.member_count.set(room.members.len() as u32);
        imp.member_slots.set(room.information.member_slots);
        *imp.host.borrow_mut() = room.information.host_username.clone();
        imp.has_password.set(room.has_password);
        *imp.icon.borrow_mut() = icon;

        let children = gio::ListStore::new::<LobbyEntry>();
        if !room.information.description.is_empty() {
            children.append(&Self::detail(
                index,
                format!("Description: {}", room.information.description),
            ));
        }
        if !room.members.is_empty() {
            let members = room
                .members
                .iter()
                .map(member_activity)
                .collect::<Vec<_>>()
                .join("\n");
            children.append(&Self::detail(index, members));
        }
        if children.n_items() != 0 {
            *imp.children.borrow_mut() = Some(children);
        }
        entry
    }

    fn detail(index: usize, text: String) -> Self {
        let entry: Self = glib::Object::new();
        let imp = entry.imp();
        imp.room_index.set(index as u32);
        *imp.preferred_game.borrow_mut() = text;
        entry
    }

    fn room_index(&self) -> usize {
        self.imp().room_index.get() as usize
    }

    fn is_room(&self) -> bool {
        self.imp().is_room.get()
    }

    fn preferred_game(&self) -> String {
        self.imp().preferred_game.borrow().clone()
    }

    fn room_name(&self) -> String {
        self.imp().room_name.borrow().clone()
    }

    fn players(&self) -> String {
        self.imp().players.borrow().clone()
    }

    fn member_count(&self) -> usize {
        self.imp().member_count.get() as usize
    }

    fn member_slots(&self) -> usize {
        self.imp().member_slots.get() as usize
    }

    fn host(&self) -> String {
        self.imp().host.borrow().clone()
    }

    fn has_password(&self) -> bool {
        self.imp().has_password.get()
    }

    fn icon(&self) -> Option<gdk::Texture> {
        self.imp().icon.borrow().clone()
    }

    fn children(&self) -> Option<gio::ListStore> {
        self.imp().children.borrow().clone()
    }
}

fn member_activity(member: &common::announce_multiplayer_room::Member) -> String {
    let name = if member.username.is_empty() || member.username == member.nickname {
        member.nickname.clone()
    } else {
        format!("{} ({})", member.nickname, member.username)
    };
    if member.game.name.is_empty() {
        format!("{name} is not playing a game")
    } else {
        format!("{name} is playing {}", member.game.name)
    }
}

/// `LobbyItemMemberList::data(Qt::ForegroundRole)` colors room occupancy so a
/// full or nearly-full room is visible without reading the number.
fn player_count_color(members: usize, max_players: usize) -> Option<&'static str> {
    if members >= max_players {
        Some("#ff3020")
    } else if members == max_players - 1 {
        Some("#ff8c20")
    } else if members == 0 {
        Some("#808080")
    } else if members < max_players - 1 {
        Some("#20a020")
    } else {
        None
    }
}

#[derive(Clone)]
struct LobbyFilter {
    text: String,
    games_owned: bool,
    hide_empty: bool,
    hide_full: bool,
}

/// Eden filters room name, preferred game and host, then applies the three
/// checkable filters from `LobbyFilterProxyModel::filterAcceptsRow`.
fn matches_filter(
    room: &Room,
    filter: &LobbyFilter,
    local_games: &[(u64, Option<gdk::Texture>)],
) -> bool {
    if filter.hide_empty && room.members.is_empty() {
        return false;
    }
    if filter.hide_full && room.members.len() >= room.information.member_slots as usize {
        return false;
    }
    if filter.games_owned
        && !local_games.iter().any(|(program_id, _)| {
            *program_id != 0 && *program_id == room.information.preferred_game.id
        })
    {
        return false;
    }
    if filter.text.is_empty() {
        return true;
    }
    let needle = filter.text.to_lowercase();
    room.information.name.to_lowercase().contains(&needle)
        || room
            .information
            .preferred_game
            .name
            .to_lowercase()
            .contains(&needle)
        || room
            .information
            .host_username
            .to_lowercase()
            .contains(&needle)
}

fn local_game_icon(
    local_games: &[(u64, Option<gdk::Texture>)],
    program_id: u64,
) -> Option<gdk::Texture> {
    local_games
        .iter()
        .find(|(local_id, _)| *local_id != 0 && *local_id == program_id)
        .and_then(|(_, icon)| icon.clone())
}

/// Eden treats the room verification UID as the audience of an externally
/// signed JWT. Anonymous users send an empty token.
fn external_room_token(verify_uid: &str) -> String {
    let values = common::settings::values();
    let username = values.eden_username.get_value().clone();
    let credential = values.eden_token.get_value().clone();
    if username.is_empty() || credential.is_empty() {
        return String::new();
    }
    let mut client = web_service::web_backend::Client::new(
        values.web_api_url.get_value().clone(),
        username,
        credential,
    );
    drop(values);

    let result = client.get_external_jwt(verify_uid);
    if result.returned_data.is_empty() {
        log::error!("Could not get external JWT, verification may fail");
    } else {
        log::info!(
            "Successfully requested external JWT: size={}",
            result.returned_data.len()
        );
    }
    result.returned_data
}

fn begin_join(
    dialog: gtk::Window,
    status: gtk::Label,
    room_member: Arc<RoomMember>,
    on_joined: Rc<dyn Fn()>,
    nickname: String,
    address: String,
    port: u16,
    password: String,
    verify_uid: String,
) {
    crate::uisettings::with_mut(|values| {
        values.multiplayer_nickname.set_value(nickname.clone());
        values.multiplayer_ip.set_value(address.clone());
        values.multiplayer_port.set_value(port);
    });
    if let Err(error) = crate::configuration::qt_config::save_multiplayer_values() {
        log::error!("Could not save multiplayer settings: {error}");
    }

    log::info!("Joining room at {address}:{port} as {nickname}");
    status.set_text("Connecting…");
    let (state_sender, state_receiver) = std::sync::mpsc::channel();
    let state_handle = room_member.bind_on_state_changed(move |state| {
        let _ = state_sender.send(*state);
    });
    let (error_sender, error_receiver) = std::sync::mpsc::channel();
    let error_handle = room_member.bind_on_error(move |error| {
        let _ = error_sender.send(*error);
    });
    let worker_member = Arc::clone(&room_member);
    std::thread::Builder::new()
        .name("LobbyJoin".to_string())
        .spawn(move || {
            let token = external_room_token(&verify_uid);
            worker_member.join(
                &nickname,
                &address,
                port,
                0,
                &network::room::NO_PREFERRED_IP,
                &password,
                &token,
            );
        })
        .expect("failed to spawn the LobbyJoin thread");

    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        if let Ok(error) = error_receiver.try_recv() {
            room_member.unbind_on_state_changed(&state_handle);
            room_member.unbind_on_error(&error_handle);
            status.set_text("Could not join the room.");
            super::message::ErrorManager::show_error(
                Some(dialog.upcast_ref()),
                super::message::ErrorManager::for_room_member_error(error),
            );
            return glib::ControlFlow::Break;
        }

        match state_receiver.try_recv() {
            Ok(RoomMemberState::Joining) => glib::ControlFlow::Continue,
            Ok(RoomMemberState::Joined | RoomMemberState::Moderator) => {
                room_member.unbind_on_state_changed(&state_handle);
                room_member.unbind_on_error(&error_handle);
                on_joined();
                dialog.close();
                glib::ControlFlow::Break
            }
            Ok(RoomMemberState::Idle | RoomMemberState::Uninitialized) => {
                status.set_text("Could not join the room.");
                glib::ControlFlow::Continue
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                room_member.unbind_on_state_changed(&state_handle);
                room_member.unbind_on_error(&error_handle);
                status.set_text("Could not join the room.");
                super::message::ErrorManager::show_error(
                    Some(dialog.upcast_ref()),
                    &super::message::ErrorManager::UNABLE_TO_CONNECT,
                );
                glib::ControlFlow::Break
            }
        }
    });
}

/// Upstream `Lobby::PasswordPrompt`.
fn prompt_password(parent: &gtk::Window, on_submit: impl Fn(String) + 'static) {
    let prompt = gtk::Window::builder()
        .title("Password Required to Join")
        .transient_for(parent)
        .modal(true)
        .default_width(360)
        .build();
    let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
    column.set_margin_top(12);
    column.set_margin_bottom(12);
    column.set_margin_start(12);
    column.set_margin_end(12);
    let password = gtk::Entry::builder()
        .placeholder_text("Password")
        .visibility(false)
        .build();
    column.append(&password);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let join = gtk::Button::with_label("Join");
    buttons.append(&cancel);
    buttons.append(&join);
    column.append(&buttons);
    prompt.set_child(Some(&column));

    cancel.connect_clicked(glib::clone!(
        #[weak]
        prompt,
        move |_| prompt.close()
    ));
    join.connect_clicked(glib::clone!(
        #[strong]
        prompt,
        #[strong]
        password,
        move |_| {
            let value = password.text().to_string();
            if value.is_empty() {
                return;
            }
            on_submit(value);
            prompt.close();
        }
    ));
    prompt.present();
}

fn compare_entries<T: Ord>(
    a: &glib::Object,
    b: &glib::Object,
    value: impl Fn(&LobbyEntry) -> T,
) -> gtk::Ordering {
    let Some(a) = a.downcast_ref::<LobbyEntry>() else {
        return gtk::Ordering::Equal;
    };
    let Some(b) = b.downcast_ref::<LobbyEntry>() else {
        return gtk::Ordering::Equal;
    };
    match value(a).cmp(&value(b)) {
        std::cmp::Ordering::Less => gtk::Ordering::Smaller,
        std::cmp::Ordering::Equal => gtk::Ordering::Equal,
        std::cmp::Ordering::Greater => gtk::Ordering::Larger,
    }
}

fn locale_sort_key(value: String) -> glib::CollationKey {
    // Qt's `setSortCaseSensitivity(Qt::CaseInsensitive)` is applied together
    // with `setSortLocaleAware(true)`. GLib's collation key supplies the same
    // current-locale ordering after Unicode case folding.
    glib::CollationKey::from(value.to_lowercase())
}

/// Upstream falls back to the literal `"Eden"` when neither the web-service
/// account nor the Switch profile provides a name, which makes every default
/// install announce itself under the same nickname. Reden generates a unique
/// handle instead, short enough to satisfy `is_valid_nickname` (6 + 8 = 14 of
/// the 20 allowed characters).
///
/// Note that upstream's middle branch, `Lobby::GetProfileUsername()`, has no
/// counterpart here: it reads `System::GetProfileManager()`, and the reden
/// lobby is not given a `System`.
fn generated_nickname() -> String {
    format!(
        "reden-{}",
        &common::uuid::UUID::make_random().raw_string()[..8]
    )
}

/// Decode the bundled `chip` fallback once and reuse it for every room row.
fn chip_icon() -> Option<gdk::Texture> {
    thread_local! {
        static CHIP: Option<gdk::Texture> =
            gdk::Texture::from_bytes(&glib::Bytes::from_static(CHIP_ICON_PNG)).ok();
    }
    CHIP.with(Clone::clone)
}

fn preferred_game_column() -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let icon = gtk::Image::new();
        icon.set_pixel_size(GAME_ICON_SIZE);
        let label = gtk::Label::builder().xalign(0.0).build();
        row.append(&icon);
        row.append(&label);
        let expander = gtk::TreeExpander::new();
        expander.set_child(Some(&row));
        item.set_child(Some(&expander));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(expander) = item.child().and_downcast::<gtk::TreeExpander>() else {
            return;
        };
        let Some(tree_row) = item.item().and_downcast::<gtk::TreeListRow>() else {
            return;
        };
        expander.set_list_row(Some(&tree_row));
        let Some(entry) = tree_row.item().and_downcast::<LobbyEntry>() else {
            return;
        };
        let Some(row) = expander.child().and_downcast::<gtk::Box>() else {
            return;
        };
        let Some(icon) = row.first_child().and_downcast::<gtk::Image>() else {
            return;
        };
        let Some(label) = row.last_child().and_downcast::<gtk::Label>() else {
            return;
        };
        icon.set_visible(entry.is_room());
        if entry.is_room() {
            if let Some(texture) = entry.icon().or_else(chip_icon) {
                icon.set_paintable(Some(&texture));
            } else {
                icon.set_paintable(gdk::Paintable::NONE);
            }
        }
        // Upstream spans the description and member-activity rows across every
        // column (`setFirstColumnSpanned`), which `GtkColumnView` cannot do:
        // cells are clipped to their own column. Wrapping those rows at a fixed
        // character budget keeps them readable and, more importantly, stops
        // them from driving the content-sized width of this column when a room
        // with a long description is expanded.
        if entry.is_room() {
            label.set_wrap(false);
            label.set_max_width_chars(-1);
        } else {
            label.set_wrap(true);
            label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            label.set_max_width_chars(DETAIL_WRAP_CHARS);
        }
        label.set_label(&entry.preferred_game());
    });

    // Upstream sizes every column but the last to its contents
    // (`resizeColumnToContents`), so do not expand or clamp this one.
    let column = gtk::ColumnViewColumn::new(Some("Preferred Game"), Some(factory));
    column.set_resizable(true);
    let sorter = gtk::CustomSorter::new(|a, b| {
        compare_entries(a, b, |entry| locale_sort_key(entry.preferred_game()))
    });
    column.set_sorter(Some(&sorter));
    column
}

fn room_name_column() -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        let lock = gtk::Image::from_icon_name("changes-prevent-symbolic");
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        row.append(&lock);
        row.append(&label);
        item.set_child(Some(&row));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(row) = item.child().and_downcast::<gtk::Box>() else {
            return;
        };
        let Some(lock) = row.first_child().and_downcast::<gtk::Image>() else {
            return;
        };
        let Some(label) = row.last_child().and_downcast::<gtk::Label>() else {
            return;
        };
        let entry = item
            .item()
            .and_downcast::<gtk::TreeListRow>()
            .and_then(|tree_row| tree_row.item())
            .and_downcast::<LobbyEntry>();
        if let Some(entry) = entry {
            lock.set_visible(entry.is_room() && entry.has_password());
            label.set_label(&entry.room_name());
        }
    });

    let column = gtk::ColumnViewColumn::new(Some("Room Name"), Some(factory));
    column.set_resizable(true);
    let sorter = gtk::CustomSorter::new(|a, b| {
        compare_entries(a, b, |entry| locale_sort_key(entry.room_name()))
    });
    column.set_sorter(Some(&sorter));
    column
}

fn text_column(title: &str, get: fn(&LobbyEntry) -> String) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let label = gtk::Label::builder().xalign(0.0).build();
        item.downcast_ref::<gtk::ListItem>()
            .unwrap()
            .set_child(Some(&label));
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(label) = item.child().and_downcast::<gtk::Label>() else {
            return;
        };
        let value = item
            .item()
            .and_downcast::<gtk::TreeListRow>()
            .and_then(|tree_row| tree_row.item())
            .and_downcast::<LobbyEntry>()
            .map(|entry| get(&entry))
            .unwrap_or_default();
        label.set_label(&value);
    });
    // `Host` is the trailing column: upstream calls `stretchLastSection()`.
    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_expand(true);
    column.set_resizable(true);
    let sorter = gtk::CustomSorter::new(move |a, b| {
        compare_entries(a, b, |entry| locale_sort_key(get(entry)))
    });
    column.set_sorter(Some(&sorter));
    column
}

fn players_column() -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let label = gtk::Label::builder().xalign(0.0).build();
        item.downcast_ref::<gtk::ListItem>()
            .unwrap()
            .set_child(Some(&label));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(label) = item.child().and_downcast::<gtk::Label>() else {
            return;
        };
        let entry = item
            .item()
            .and_downcast::<gtk::TreeListRow>()
            .and_then(|tree_row| tree_row.item())
            .and_downcast::<LobbyEntry>();
        let Some(entry) = entry else {
            label.set_text("");
            return;
        };
        let text = entry.players();
        if entry.is_room() {
            if let Some(color) = player_count_color(entry.member_count(), entry.member_slots()) {
                label.set_markup(&format!("<span foreground=\"{color}\">{text}</span>"));
                return;
            }
        }
        label.set_text(&text);
    });
    let column = gtk::ColumnViewColumn::new(Some("Players"), Some(factory));
    column.set_resizable(true);
    let sorter = gtk::CustomSorter::new(|a, b| compare_entries(a, b, LobbyEntry::member_count));
    column.set_sorter(Some(&sorter));
    column
}

fn selected_room_index(selection: &gtk::SingleSelection) -> Option<usize> {
    selection
        .selected_item()
        .and_downcast::<gtk::TreeListRow>()
        .and_then(|tree_row| tree_row.item())
        .and_downcast::<LobbyEntry>()
        .map(|entry| entry.room_index())
}

/// Shows the public room browser. Maps to upstream `Lobby::Lobby`.
pub fn show(
    parent: &gtk::ApplicationWindow,
    room_member: Arc<RoomMember>,
    game_list: crate::game_list::GameListHandle,
    on_joined: impl Fn() + 'static,
) {
    let dialog = gtk::Window::builder()
        .title("Public Room Browser")
        .transient_for(parent)
        .modal(true)
        .default_width(920)
        .default_height(540)
        .build();

    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.set_margin_top(8);
    column.set_margin_bottom(8);
    column.set_margin_start(8);
    column.set_margin_end(8);

    let saved_nickname =
        crate::uisettings::with(|values| values.multiplayer_nickname.get_value().clone());
    let web_username = common::settings::values().eden_username.get_value().clone();
    let nickname_value = if saved_nickname.is_empty() || saved_nickname == "Eden" {
        let chosen = if web_username.is_empty() {
            generated_nickname()
        } else {
            web_username
        };
        // Persist immediately so the generated handle stays the same across
        // launches; upstream only writes the nickname back when a join starts.
        crate::uisettings::with_mut(|values| values.multiplayer_nickname.set_value(chosen.clone()));
        if let Err(error) = crate::configuration::qt_config::save_multiplayer_values() {
            log::error!("Could not persist the default multiplayer nickname: {error}");
        }
        chosen
    } else {
        saved_nickname
    };
    let saved_filter = crate::uisettings::with(|values| {
        (
            values.multiplayer_filter_text.get_value().clone(),
            *values.multiplayer_filter_games_owned.get_value(),
            *values.multiplayer_filter_hide_empty.get_value(),
            *values.multiplayer_filter_hide_full.get_value(),
        )
    });

    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    toolbar.append(&gtk::Label::new(Some("Nickname")));
    let nickname = gtk::Entry::builder()
        .text(&nickname_value)
        .width_chars(12)
        .build();
    toolbar.append(&nickname);
    toolbar.append(&gtk::Label::new(Some("Filters")));
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search")
        .text(&saved_filter.0)
        .width_request(150)
        .build();
    toolbar.append(&search);
    let games_owned = gtk::CheckButton::with_label("Games I Own");
    games_owned.set_active(saved_filter.1);
    let hide_empty = gtk::CheckButton::with_label("Hide Empty Rooms");
    hide_empty.set_active(saved_filter.2);
    let hide_full = gtk::CheckButton::with_label("Hide Full Rooms");
    hide_full.set_active(saved_filter.3);
    toolbar.append(&games_owned);
    toolbar.append(&hide_empty);
    toolbar.append(&hide_full);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    toolbar.append(&spacer);
    let refresh = gtk::Button::with_label("Refresh List");
    toolbar.append(&refresh);
    column.append(&toolbar);

    let root_store = gio::ListStore::new::<LobbyEntry>();
    let view = gtk::ColumnView::new(None::<gtk::SingleSelection>);
    view.set_vexpand(true);
    view.set_hexpand(true);
    view.set_show_column_separators(true);
    let preferred_game = preferred_game_column();
    view.append_column(&preferred_game);
    view.append_column(&room_name_column());
    view.append_column(&players_column());
    view.append_column(&text_column("Host", LobbyEntry::host));

    let sorted_rooms = gtk::SortListModel::new(Some(root_store.clone()), view.sorter());
    let tree = gtk::TreeListModel::new(sorted_rooms, false, false, |item| {
        item.downcast_ref::<LobbyEntry>()
            .and_then(LobbyEntry::children)
            .map(Cast::upcast)
    });
    let selection = gtk::SingleSelection::new(Some(tree));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    selection.set_selected(gtk::INVALID_LIST_POSITION);
    view.set_model(Some(&selection));
    view.sort_by_column(Some(&preferred_game), gtk::SortType::Ascending);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .hexpand(true)
        .build();
    column.append(&scroller);

    let status = gtk::Label::new(Some("Press Refresh to load the room list."));
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    column.append(&status);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let close = gtk::Button::with_label("Close");
    let join = gtk::Button::with_label("Join");
    join.add_css_class("suggested-action");
    join.set_sensitive(false);
    buttons.append(&close);
    buttons.append(&join);
    column.append(&buttons);
    dialog.set_child(Some(&column));

    close.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| dialog.close()
    ));

    let rooms: Rc<RefCell<RoomList>> = Rc::new(RefCell::new(Vec::new()));
    let rebuild = {
        let rooms = Rc::clone(&rooms);
        let root_store = root_store.clone();
        let search = search.clone();
        let games_owned = games_owned.clone();
        let hide_empty = hide_empty.clone();
        let hide_full = hide_full.clone();
        let game_list = game_list.clone();
        let status = status.clone();
        let selection = selection.clone();
        let join = join.clone();
        move || {
            let filter = LobbyFilter {
                text: search.text().trim().to_string(),
                games_owned: games_owned.is_active(),
                hide_empty: hide_empty.is_active(),
                hide_full: hide_full.is_active(),
            };
            let all = rooms.borrow();
            let local_games = game_list.program_ids_and_icons();
            let mut shown: Vec<usize> = all
                .iter()
                .enumerate()
                .filter_map(|(index, room)| {
                    matches_filter(room, &filter, &local_games).then_some(index)
                })
                .collect();
            shown.sort_by_key(|index| {
                locale_sort_key(all[*index].information.preferred_game.name.clone())
            });
            root_store.remove_all();
            for index in &shown {
                let room = &all[*index];
                root_store.append(&LobbyEntry::room(
                    *index,
                    room,
                    local_game_icon(&local_games, room.information.preferred_game.id),
                ));
            }
            selection.set_selected(gtk::INVALID_LIST_POSITION);
            join.set_sensitive(false);
            if all.is_empty() {
                status.set_text("No rooms announced.");
            } else {
                status.set_text(&format!("{} of {} room(s)", shown.len(), all.len()));
            }
        }
    };

    for control in [&games_owned, &hide_empty, &hide_full] {
        let rebuild = rebuild.clone();
        control.connect_toggled(move |_| rebuild());
    }
    search.connect_search_changed({
        let rebuild = rebuild.clone();
        move |_| rebuild()
    });
    selection.connect_selected_notify(glib::clone!(
        #[strong]
        join,
        move |selection| join.set_sensitive(selected_room_index(selection).is_some())
    ));
    view.connect_activate(glib::clone!(
        #[strong]
        join,
        move |_, _| join.emit_clicked()
    ));

    let do_refresh = {
        let status = status.clone();
        let refresh = refresh.clone();
        let rooms = Rc::clone(&rooms);
        let rebuild = rebuild.clone();
        move || {
            status.set_text("Fetching the room list…");
            refresh.set_sensitive(false);
            refresh.set_label("Refreshing");
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("LobbyRefresh".to_string())
                .spawn(move || {
                    let session =
                        network::announce_multiplayer_session::AnnounceMultiplayerSession::new(
                            &network::network::RoomNetwork::default(),
                        );
                    let _ = sender.send(session.get_room_list());
                })
                .expect("failed to spawn the LobbyRefresh thread");

            glib::timeout_add_local(
                std::time::Duration::from_millis(100),
                glib::clone!(
                    #[strong]
                    status,
                    #[strong]
                    refresh,
                    #[strong]
                    rooms,
                    #[strong]
                    rebuild,
                    move || match receiver.try_recv() {
                        Ok(list) => {
                            *rooms.borrow_mut() = list;
                            rebuild();
                            refresh.set_sensitive(true);
                            refresh.set_label("Refresh List");
                            glib::ControlFlow::Break
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                            glib::ControlFlow::Continue
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            status.set_text("Could not reach the announce service.");
                            refresh.set_sensitive(true);
                            refresh.set_label("Refresh List");
                            glib::ControlFlow::Break
                        }
                    }
                ),
            );
        }
    };
    refresh.connect_clicked({
        let do_refresh = do_refresh.clone();
        move |_| do_refresh()
    });

    let on_joined: Rc<dyn Fn()> = Rc::new(on_joined);
    join.connect_clicked(glib::clone!(
        #[strong]
        dialog,
        #[strong]
        selection,
        #[strong]
        rooms,
        #[strong]
        status,
        #[strong]
        room_member,
        #[strong]
        on_joined,
        #[strong]
        nickname,
        #[strong]
        search,
        #[strong]
        games_owned,
        #[strong]
        hide_empty,
        #[strong]
        hide_full,
        move |_| {
            let current_state = room_member.get_state();
            if current_state == RoomMemberState::Joining {
                log::warn!("Join was clicked while a connection is already in progress");
                status.set_text("A room connection is already in progress.");
                return;
            }
            if room_member.is_connected() {
                log::warn!("Join was clicked while already connected to a room");
                status.set_text("Leave the current room before joining another one.");
                return;
            }
            if ruzu_core::internal_network::network_interface::get_selected_network_interface()
                .is_none()
            {
                ruzu_core::internal_network::network_interface::select_first_network_interface();
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

            let nickname_value = nickname.text().to_string();
            if !super::validation::is_valid_nickname(&nickname_value) {
                super::message::ErrorManager::show_error(
                    Some(dialog.upcast_ref()),
                    &super::message::ErrorManager::USERNAME_NOT_VALID,
                );
                return;
            }
            let Some(room_index) = selected_room_index(&selection) else {
                log::warn!("Join was clicked without a selected room");
                return;
            };
            let all = rooms.borrow();
            let Some(room) = all.get(room_index) else {
                log::warn!(
                    "Join was clicked for room index {room_index}, but only {} room(s) are known",
                    all.len()
                );
                return;
            };
            let address = room.ip.clone();
            let port = room.information.port;
            let verify_uid = room.verify_uid.clone();
            let has_password = room.has_password;
            drop(all);

            crate::uisettings::with_mut(|values| {
                values
                    .multiplayer_filter_text
                    .set_value(search.text().to_string());
                values
                    .multiplayer_filter_games_owned
                    .set_value(games_owned.is_active());
                values
                    .multiplayer_filter_hide_empty
                    .set_value(hide_empty.is_active());
                values
                    .multiplayer_filter_hide_full
                    .set_value(hide_full.is_active());
            });

            let join_room = {
                let dialog = dialog.clone();
                let status = status.clone();
                let room_member = Arc::clone(&room_member);
                let on_joined = Rc::clone(&on_joined);
                move |password: String| {
                    begin_join(
                        dialog.clone(),
                        status.clone(),
                        Arc::clone(&room_member),
                        Rc::clone(&on_joined),
                        nickname_value.clone(),
                        address.clone(),
                        port,
                        password,
                        verify_uid.clone(),
                    );
                }
            };
            if has_password {
                prompt_password(dialog.upcast_ref(), join_room);
            } else {
                join_room(String::new());
            }
        }
    ));

    dialog.present();
    do_refresh();
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::announce_multiplayer_room::{GameInfo, Member, RoomInformation};

    fn room(name: &str, game: &str, players: usize, slots: u32) -> Room {
        Room {
            information: RoomInformation {
                name: name.to_string(),
                preferred_game: GameInfo {
                    name: game.to_string(),
                    ..GameInfo::default()
                },
                member_slots: slots,
                host_username: "host".to_string(),
                ..RoomInformation::default()
            },
            members: vec![Member::default(); players],
            ..Room::default()
        }
    }

    #[test]
    fn member_activity_matches_upstream_wording() {
        let mut member = Member {
            nickname: "nick".into(),
            username: "web".into(),
            ..Member::default()
        };
        assert_eq!(member_activity(&member), "nick (web) is not playing a game");
        member.game.name = "Example Racing Game".into();
        assert_eq!(
            member_activity(&member),
            "nick (web) is playing Example Racing Game"
        );
    }

    #[test]
    fn room_entry_has_expandable_description_and_members() {
        let mut subject = room("room", "game", 1, 8);
        subject.information.description = "description".into();
        let entry = LobbyEntry::room(3, &subject, None);
        assert_eq!(entry.room_index(), 3);
        assert_eq!(entry.players(), "1 / 8");
        assert_eq!(entry.children().unwrap().n_items(), 2);
    }

    #[test]
    fn generated_nickname_passes_the_upstream_validator() {
        // 4..=20 alphanumeric characters, per `Validation::GetNickname`.
        let first = generated_nickname();
        assert!(first.starts_with("reden-"));
        assert_eq!(first.chars().count(), 14);
        assert!(super::super::validation::is_valid_nickname(&first));
        // The handle must not collide with the next default install.
        assert_ne!(first, generated_nickname());
    }

    #[test]
    fn player_count_colors_match_lobby_item_member_list() {
        assert_eq!(player_count_color(8, 8), Some("#ff3020"));
        assert_eq!(player_count_color(7, 8), Some("#ff8c20"));
        assert_eq!(player_count_color(0, 8), Some("#808080"));
        assert_eq!(player_count_color(3, 8), Some("#20a020"));
    }

    #[test]
    fn locale_sort_keys_remain_case_insensitive() {
        assert_eq!(
            locale_sort_key("Zelda".into()),
            locale_sort_key("zelda".into())
        );
    }
}
