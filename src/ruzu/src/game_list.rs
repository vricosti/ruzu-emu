// SPDX-License-Identifier: GPL-3.0-or-later
//
// Game list view — counterpart of Eden `GameList` / `GameListWorker`
// (`~/Dev/emulators/eden/src/yuzu/game/` and `src/qt_common/game_list/`). It reads the
// configured game directories from ruzu's own config, scans them for Switch
// executables, and shows them grouped under one expandable row per directory.
// Activating a game row (double-click / Enter) boots it.
//
// Divergence from upstream, deliberate: Eden exposes "add a game directory" as
// a fake row appended *inside* the tree, which reads as an item belonging to
// the scanned folder. Here that action lives in a toolbar above the list, while
// the tree contains upstream's Favorites root plus real directories and games.
// Directory and game actions otherwise live in per-row context menus, matching
// `GameList::PopupContextMenu`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib};

use ruzu_core::file_sys::content_archive::NCA;
use ruzu_core::file_sys::control_metadata::NACP;
use ruzu_core::file_sys::fs_filesystem::OpenMode;
use ruzu_core::file_sys::kernel_executable::KIP;
use ruzu_core::file_sys::nca_metadata::TitleType;
use ruzu_core::file_sys::partition_filesystem::ResultStatus as FsResultStatus;
use ruzu_core::file_sys::patch_manager::{Patch, PatchManager};
use ruzu_core::file_sys::program_metadata::ProgramMetadata;
use ruzu_core::file_sys::registered_cache::{
    get_cr_type_from_nca_type, get_update_title_id, ContentProvider, ContentProviderEntry,
    ContentProviderUnion, ContentProviderUnionSlot, ExternalUpdateEntry, ManualContentProvider,
};
use ruzu_core::file_sys::vfs::vfs_real::RealVfsFilesystem;
use ruzu_core::file_sys::vfs::vfs_types::VirtualFile;
use ruzu_core::hle::service::filesystem::filesystem::FileSystemController;
use ruzu_core::loader::loader::{
    get_loader, identify_file, is_bootable_game_container, FileType, ResultStatus,
    System as LoaderSystem,
};

use crate::configuration::qt_config;
use crate::main_window::StartGameType;
use crate::uisettings::{self, GameDir};
use crate::util::controller_navigation::{ControllerNavigation, NavigationKey};

/// Pixel size of the game icon shown in the list.
const ICON_SIZE: i32 = 64;

/// Pixel size of the folder icon on a directory row.
const FOLDER_ICON_SIZE: i32 = 48;

/// Upstream's colorful-theme `folder`, `bad_folder` and `star` icons. Keep
/// local copies so the game list does not depend on the host icon theme or the
/// zuyu tree.
const FOLDER_ICON_PNG: &[u8] = include_bytes!("../assets/game-list-folder.png");
const BAD_FOLDER_ICON_PNG: &[u8] = include_bytes!("../assets/game-list-bad-folder.png");
const FAVORITES_ICON_PNG: &[u8] = include_bytes!("../assets/game-list-star.png");

/// Icon shown on a filesystem directory row.
///
/// Port of the `CustomDir` branch of upstream `GameListDir`
/// (`qt_common/game_list/game_list_p.h`), which selects the icon from the
/// directory's presence on disk:
/// `icon_name = QFileInfo::exists(path) ? "folder" : "bad_folder";`
fn folder_icon_png(path: &str) -> &'static [u8] {
    if Path::new(path).exists() {
        FOLDER_ICON_PNG
    } else {
        BAD_FOLDER_ICON_PNG
    }
}

/// Ruzu-specific default requested for newly added filesystem directories.
const NEW_DIRECTORY_DEEP_SCAN: bool = true;

/// Switch executable extensions listed in the game view. Mirrors
/// `GameList::supported_file_extensions`.
const SUPPORTED_EXTENSIONS: &[&str] = &["nsp", "xci", "nca", "nro", "nso", "kip"];

/// Process-wide frontend provider. The mutex adapter keeps the provider stable
/// for the non-owning union slot while allowing the game-list worker to refill
/// it safely.
struct SharedManualContentProvider {
    inner: Mutex<ManualContentProvider>,
}

impl ContentProvider for SharedManualContentProvider {
    fn refresh(&mut self) {
        self.inner.get_mut().unwrap().refresh();
    }

    fn has_entry(
        &self,
        title_id: u64,
        record_type: ruzu_core::file_sys::nca_metadata::ContentRecordType,
    ) -> bool {
        self.inner.lock().unwrap().has_entry(title_id, record_type)
    }

    fn get_entry_version(&self, title_id: u64) -> Option<u32> {
        self.inner.lock().unwrap().get_entry_version(title_id)
    }

    fn get_entry_unparsed(
        &self,
        title_id: u64,
        record_type: ruzu_core::file_sys::nca_metadata::ContentRecordType,
    ) -> Option<VirtualFile> {
        self.inner
            .lock()
            .unwrap()
            .get_entry_unparsed(title_id, record_type)
    }

    fn get_entry_raw(
        &self,
        title_id: u64,
        record_type: ruzu_core::file_sys::nca_metadata::ContentRecordType,
    ) -> Option<VirtualFile> {
        self.inner
            .lock()
            .unwrap()
            .get_entry_raw(title_id, record_type)
    }

    fn list_entries_filter(
        &self,
        title_type: Option<TitleType>,
        record_type: Option<ruzu_core::file_sys::nca_metadata::ContentRecordType>,
        title_id: Option<u64>,
    ) -> Vec<ContentProviderEntry> {
        self.inner
            .lock()
            .unwrap()
            .list_entries_filter(title_type, record_type, title_id)
    }

    fn list_update_versions(&self, title_id: u64) -> Vec<ExternalUpdateEntry> {
        self.inner.lock().unwrap().list_update_versions(title_id)
    }

    fn get_entry_for_version(
        &self,
        title_id: u64,
        content_type: ruzu_core::file_sys::nca_metadata::ContentRecordType,
        version: u32,
    ) -> Option<VirtualFile> {
        self.inner
            .lock()
            .unwrap()
            .get_entry_for_version(title_id, content_type, version)
    }
}

struct FrontendContentProviders {
    vfs: Arc<RealVfsFilesystem>,
    manual: Box<SharedManualContentProvider>,
    union: Arc<Mutex<ContentProviderUnion>>,
}

fn frontend_content_providers() -> &'static FrontendContentProviders {
    static PROVIDERS: OnceLock<FrontendContentProviders> = OnceLock::new();
    PROVIDERS.get_or_init(|| {
        let mut manual = Box::new(SharedManualContentProvider {
            inner: Mutex::new(ManualContentProvider::default()),
        });
        let union = Arc::new(Mutex::new(ContentProviderUnion::new()));
        unsafe {
            union.lock().unwrap().set_slot(
                ContentProviderUnionSlot::FrontendManual,
                (&mut *manual as *mut SharedManualContentProvider) as *mut dyn ContentProvider,
            );
        }
        FrontendContentProviders {
            vfs: RealVfsFilesystem::new(),
            manual,
            union,
        }
    })
}

pub(crate) fn frontend_content_provider_union() -> Arc<Mutex<ContentProviderUnion>> {
    Arc::clone(&frontend_content_providers().union)
}

pub(crate) fn frontend_vfs() -> Arc<RealVfsFilesystem> {
    Arc::clone(&frontend_content_providers().vfs)
}

// ---------------------------------------------------------------------------
// GameEntry — a GObject row model for the ColumnView.
// ---------------------------------------------------------------------------
mod imp {
    use std::cell::{Cell, RefCell};

    use gtk::glib;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub struct GameEntry {
        pub name: RefCell<String>,
        pub developer: RefCell<String>,
        pub version: RefCell<String>,
        pub kind: RefCell<String>,
        pub architecture: RefCell<String>,
        pub size: RefCell<String>,
        pub play_time: RefCell<String>,
        pub add_ons: RefCell<String>,
        pub path: RefCell<String>,
        pub icon: RefCell<Option<gtk::gdk::Texture>>,
        /// Application program id. Zero for homebrew without a title id.
        pub program_id: Cell<u64>,
        /// Directory rows group the games found beneath them.
        pub is_folder: Cell<bool>,
        /// The first group is upstream's `GameListFavorites`, not a filesystem
        /// directory even though it is expandable like one.
        pub is_favorites: Cell<bool>,
        /// Ruzu packages may expose a read-only group of bundled homebrew.
        /// Unlike a configured directory, this row cannot be moved or removed.
        pub is_built_in: Cell<bool>,
        /// Whether this directory is scanned recursively (directory rows only).
        pub deep_scan: Cell<bool>,
        /// Child rows, for directory rows.
        pub children: RefCell<Option<gtk::gio::ListStore>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for GameEntry {
        const NAME: &'static str = "RuzuGameEntry";
        type Type = super::GameEntry;
    }

    impl ObjectImpl for GameEntry {}
}

glib::wrapper! {
    pub struct GameEntry(ObjectSubclass<imp::GameEntry>);
}

impl GameEntry {
    /// A game row.
    fn new_game(
        name: &str,
        developer: &str,
        version: &str,
        kind: &str,
        architecture: &str,
        size: &str,
        play_time: &str,
        add_ons: &str,
        path: &str,
        icon: Option<gdk::Texture>,
        program_id: u64,
    ) -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        *imp.name.borrow_mut() = name.to_owned();
        *imp.developer.borrow_mut() = developer.to_owned();
        *imp.version.borrow_mut() = version.to_owned();
        *imp.kind.borrow_mut() = kind.to_owned();
        *imp.architecture.borrow_mut() = architecture.to_owned();
        *imp.size.borrow_mut() = size.to_owned();
        *imp.play_time.borrow_mut() = play_time.to_owned();
        *imp.add_ons.borrow_mut() = add_ons.to_owned();
        *imp.path.borrow_mut() = path.to_owned();
        *imp.icon.borrow_mut() = icon;
        imp.program_id.set(program_id);
        imp.is_folder.set(false);
        imp.is_favorites.set(false);
        imp.is_built_in.set(false);
        obj
    }

    /// A directory row, holding the games found under it.
    fn new_folder(
        name: &str,
        path: &str,
        deep_scan: bool,
        is_built_in: bool,
        children: gio::ListStore,
    ) -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        *imp.name.borrow_mut() = name.to_owned();
        *imp.path.borrow_mut() = path.to_owned();
        *imp.icon.borrow_mut() = embedded_icon(folder_icon_png(path));
        imp.is_folder.set(true);
        imp.is_favorites.set(false);
        imp.is_built_in.set(is_built_in);
        imp.deep_scan.set(deep_scan);
        *imp.children.borrow_mut() = Some(children);
        obj
    }

    /// Upstream `GameListFavorites`: an expandable, non-filesystem root row.
    fn new_favorites(children: gio::ListStore) -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        *imp.name.borrow_mut() = crate::i18n::tr("Favorites");
        *imp.icon.borrow_mut() = embedded_icon(FAVORITES_ICON_PNG);
        imp.is_folder.set(true);
        imp.is_favorites.set(true);
        imp.is_built_in.set(false);
        *imp.children.borrow_mut() = Some(children);
        obj
    }

    /// Upstream clones every column item when adding a game to Favorites.
    fn clone_game(&self) -> Self {
        debug_assert!(!self.is_folder());
        Self::new_game(
            &self.name(),
            &self.developer(),
            &self.version(),
            &self.kind(),
            &self.architecture(),
            &self.size(),
            &self.play_time(),
            &self.add_ons(),
            &self.path(),
            self.icon(),
            self.program_id(),
        )
    }

    fn name(&self) -> String {
        self.imp().name.borrow().clone()
    }
    fn developer(&self) -> String {
        self.imp().developer.borrow().clone()
    }
    fn version(&self) -> String {
        self.imp().version.borrow().clone()
    }
    fn kind(&self) -> String {
        self.imp().kind.borrow().clone()
    }
    fn architecture(&self) -> String {
        self.imp().architecture.borrow().clone()
    }
    fn size(&self) -> String {
        self.imp().size.borrow().clone()
    }
    fn play_time(&self) -> String {
        self.imp().play_time.borrow().clone()
    }
    fn add_ons(&self) -> String {
        self.imp().add_ons.borrow().clone()
    }
    fn path(&self) -> String {
        self.imp().path.borrow().clone()
    }
    fn icon(&self) -> Option<gdk::Texture> {
        self.imp().icon.borrow().clone()
    }
    fn program_id(&self) -> u64 {
        self.imp().program_id.get()
    }
    fn is_folder(&self) -> bool {
        self.imp().is_folder.get()
    }
    fn is_favorites(&self) -> bool {
        self.imp().is_favorites.get()
    }
    fn is_built_in(&self) -> bool {
        self.imp().is_built_in.get()
    }
    fn deep_scan(&self) -> bool {
        self.imp().deep_scan.get()
    }
    fn children(&self) -> Option<gio::ListStore> {
        self.imp().children.borrow().clone()
    }
}

/// The game list: a toolbar over either the tree or an empty-state placeholder.
///
/// Kept as a struct so the toolbar actions can rebuild the tree in place, the
/// way upstream re-runs `GameListWorker` after the directory list changes.
struct GameListView {
    root: gtk::Box,
    stack: gtk::Stack,
    filter_bar: gtk::Box,
    filter_entry: gtk::SearchEntry,
    filter_result: gtk::Label,
    column_view: gtk::ColumnView,
    file_type_column: gtk::ColumnViewColumn,
    architecture_column: gtk::ColumnViewColumn,
    size_column: gtk::ColumnViewColumn,
    play_time_column: gtk::ColumnViewColumn,
    add_ons_column: gtk::ColumnViewColumn,
    store: gio::ListStore,
    /// Children and root item for upstream's first `GameListFavorites` row.
    /// The root is removed while filtering or when no favorite id is configured.
    favorites: gio::ListStore,
    favorites_root: GameEntry,
    /// Unfiltered children for every directory in `store`, in matching order.
    /// Upstream hides rows in its item model; GTK's tree list has no row-hidden
    /// API, so the visible child stores are rebuilt from this retained source.
    all_games: RefCell<Vec<Vec<GameEntry>>>,
    /// Kept so a rescan can restore the selected directory: rebuilding the
    /// store clears the selection, which would otherwise disable the
    /// per-directory toolbar actions after every single use of them.
    selection: gtk::SingleSelection,
    controller_navigation: ControllerNavigation,
    hid_core: Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>,
    play_time_manager: Arc<frontend_common::play_time_manager::PlayTimeManager>,
    on_activate: Rc<dyn Fn(String, StartGameType)>,
    /// GTK equivalent of Eden's `GameList::CreateShortcut` signal. The owner
    /// remains `GMainWindow::OnGameListCreateShortcut`.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    on_create_shortcut: Rc<dyn Fn(u64, String, crate::util::game::ShortcutTarget)>,
    on_refresh: Rc<dyn Fn()>,
    refresh_button: gtk::Button,
    runtime_lock: Rc<dyn Fn() -> bool>,
    property_dialog:
        RefCell<Option<Rc<crate::configuration::configure_per_game::ConfigurePerGame>>>,
    /// Eden runs `GameListWorker` outside the UI thread. The generation makes
    /// a result from an older refresh harmless when a newer scan supersedes it.
    scan_generation: Arc<AtomicU64>,
    scan_result_sender: mpsc::Sender<GameListScanResult>,
    scan_result_receiver: RefCell<mpsc::Receiver<GameListScanResult>>,
}

struct ScannedDirectory {
    name: String,
    path: String,
    deep_scan: bool,
    is_built_in: bool,
    games: Vec<GameFile>,
}

#[derive(Clone)]
struct ScanDirectory {
    name: String,
    path: String,
    deep_scan: bool,
    is_built_in: bool,
}

impl ScanDirectory {
    fn configured(directory: GameDir) -> Self {
        Self {
            name: directory.path.clone(),
            path: directory.path,
            deep_scan: directory.deep_scan,
            is_built_in: false,
        }
    }

    fn packaged_free_games(path: PathBuf) -> Self {
        Self {
            name: crate::i18n::tr("Free Games"),
            path: path.to_string_lossy().into_owned(),
            deep_scan: true,
            is_built_in: true,
        }
    }
}

struct GameListScanResult {
    generation: u64,
    directories: Vec<ScannedDirectory>,
    directory_to_select: Option<String>,
}

type ContextMenuHandler = Rc<dyn Fn(GameEntry, gtk::Widget, u32, f64, f64)>;

/// Stack page names.
const PAGE_LIST: &str = "list";
const PAGE_EMPTY: &str = "empty";

/// Handle to a built game list, letting the owner rescan it after the
/// configured directories change (e.g. once a yuzu config has been imported).
///
/// Holding it also keeps the view alive for the widget's lifetime.
#[derive(Clone)]
pub struct GameListHandle(Rc<GameListView>);

impl GameListHandle {
    /// Re-read the configured directories and rebuild the list.
    pub fn reload(&self) {
        self.0.reload();
    }

    /// Upstream `GameListModel::RefreshGameDirectory`.
    pub fn refresh_game_directory(&self) {
        self.0.reload();
    }

    /// Upstream `GameListModel::RefreshExternalContent`.
    ///
    /// Ruzu rebuilds its frontend manual content provider as part of the same
    /// worker pass as the game-directory scan. `refresh_game_directory`
    /// therefore already performs the external-content repopulation that Eden
    /// starts as a second `Repopulate()` call.
    pub fn refresh_external_content(&self) {
        log::info!("Game list: external content refreshed with directory scan");
    }

    pub fn set_refresh_enabled(&self, enabled: bool) {
        self.0.refresh_button.set_sensitive(enabled);
    }

    /// Give keyboard navigation back to the list after returning from a game.
    pub fn focus(&self) {
        self.0.column_view.grab_focus();
    }

    /// Upstream `GameList::SetFilterVisible`, `SetFilterFocus`, and
    /// `ClearFilter` as driven by `GMainWindow::OnToggleFilterBar`.
    pub fn set_filter_visible(&self, visible: bool) {
        self.0.set_filter_visible(visible);
    }

    /// Snapshot the program-id and icon roles exposed by Eden's game-list
    /// model. The lobby remains responsible for lookup/filter semantics.
    pub fn program_ids_and_icons(&self) -> Vec<(u64, Option<gdk::Texture>)> {
        self.0
            .all_games
            .borrow()
            .iter()
            .flatten()
            .map(|game| (game.program_id(), game.icon()))
            .collect()
    }
}

/// Build the game list widget. `on_activate` is invoked with the game's path
/// when a game row is activated (double-click / Enter).
pub fn build<
    F: Fn(String, StartGameType) + 'static,
    S: Fn(u64, String, crate::util::game::ShortcutTarget) + 'static,
    T: Fn() + 'static,
    R: Fn() -> bool + 'static,
>(
    hid_core: &Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>,
    play_time_manager: &Arc<frontend_common::play_time_manager::PlayTimeManager>,
    on_activate: F,
    on_create_shortcut: S,
    on_refresh: T,
    runtime_lock: R,
) -> (gtk::Widget, GameListHandle) {
    install_list_css();

    let store = gio::ListStore::new::<GameEntry>();
    let favorites = gio::ListStore::new::<GameEntry>();
    let favorites_root = GameEntry::new_favorites(favorites.clone());

    // --- Tree ------------------------------------------------------------
    // One expandable row per configured directory; its games are the children.
    let tree = gtk::TreeListModel::new(store.clone(), false, true, |item| {
        item.downcast_ref::<GameEntry>()
            .and_then(GameEntry::children)
            .map(Cast::upcast)
    });

    let selection = gtk::SingleSelection::new(Some(tree));
    // Upstream opens the game list with nothing selected; GTK's default is to
    // auto-select the first row, which would highlight a game the user never
    // picked.
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    selection.set_selected(gtk::INVALID_LIST_POSITION);

    let column_view = gtk::ColumnView::new(Some(selection.clone()));
    column_view.add_css_class("data-table");
    column_view.add_css_class("ruzu-game-list");
    // The banding comes from the CSS below, not from GTK's separators.
    column_view.set_show_row_separators(false);
    column_view.set_show_column_separators(false);
    column_view.connect_map(|view| {
        view.grab_focus();
    });

    let on_activate: Rc<dyn Fn(String, StartGameType)> = Rc::new(on_activate);
    let on_create_shortcut: Rc<dyn Fn(u64, String, crate::util::game::ShortcutTarget)> =
        Rc::new(on_create_shortcut);
    let on_refresh: Rc<dyn Fn()> = Rc::new(on_refresh);
    let context_view: Rc<RefCell<Weak<GameListView>>> = Rc::new(RefCell::new(Weak::new()));
    let on_context_menu: ContextMenuHandler = {
        let context_view = Rc::clone(&context_view);
        let on_activate = Rc::clone(&on_activate);
        Rc::new(move |entry, anchor, position, x, y| {
            let Some(view) = context_view.borrow().upgrade() else {
                return;
            };
            view.selection.set_selected(position);
            view.popup_context_menu(&entry, &anchor, x, y, Rc::clone(&on_activate));
        })
    };

    column_view.append_column(&make_name_column(Rc::clone(&on_context_menu)));
    let file_type_column = make_text_column(
        &crate::i18n::tr("File type"),
        GameEntry::kind,
        Rc::clone(&on_context_menu),
    );
    let architecture_column = make_text_column(
        &crate::i18n::tr("Architecture"),
        GameEntry::architecture,
        Rc::clone(&on_context_menu),
    );
    let size_column = make_text_column(
        &crate::i18n::tr("Size"),
        GameEntry::size,
        Rc::clone(&on_context_menu),
    );
    let play_time_column = make_text_column(
        &crate::i18n::tr("Play time"),
        GameEntry::play_time,
        Rc::clone(&on_context_menu),
    );
    let add_ons_column = make_text_column(
        &crate::i18n::tr("Add-ons"),
        GameEntry::add_ons,
        on_context_menu,
    );
    for column in [
        &file_type_column,
        &architecture_column,
        &size_column,
        &play_time_column,
        &add_ons_column,
    ] {
        column_view.append_column(column);
    }

    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&column_view)
        .build();

    // --- Empty state ------------------------------------------------------
    let empty = build_empty_state();

    let stack = gtk::Stack::new();
    stack.add_named(&scroller, Some(PAGE_LIST));
    stack.add_named(&empty.root, Some(PAGE_EMPTY));

    // --- Toolbar ----------------------------------------------------------
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    // `ruzu-toolbar` draws the separating rule; without it the strip blends
    // into the list and reintroduces exactly the ambiguity this layout is
    // meant to remove.
    toolbar.add_css_class("ruzu-toolbar");
    toolbar.set_margin_top(4);
    toolbar.set_margin_bottom(4);
    toolbar.set_margin_start(6);
    toolbar.set_margin_end(6);

    // `Button::builder().label(..).icon_name(..)` is not additive — setting
    // `icon_name` replaces the label child — so build the icon+label row
    // explicitly.
    let add_button = icon_label_button("list-add-symbolic", "Add Game Directory");
    let refresh_button = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Rescan game directories")
        .build();
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);

    toolbar.append(&add_button);
    toolbar.append(&refresh_button);
    toolbar.append(&spacer);

    let filter_bar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    filter_bar.set_margin_top(8);
    filter_bar.set_margin_bottom(8);
    filter_bar.set_margin_start(8);
    filter_bar.set_margin_end(8);
    let filter_label = gtk::Label::new(Some(&crate::i18n::tr("Filter:")));
    let filter_entry = gtk::SearchEntry::new();
    filter_entry.set_placeholder_text(Some(&crate::i18n::tr("Enter pattern to filter")));
    filter_entry.set_hexpand(true);
    let filter_result = gtk::Label::new(None);
    let filter_close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text(crate::i18n::tr("Close"))
        .build();
    filter_bar.append(&filter_label);
    filter_bar.append(&filter_entry);
    filter_bar.append(&filter_result);
    filter_bar.append(&filter_close);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&toolbar);
    root.append(&stack);
    root.append(&filter_bar);

    let (scan_result_sender, scan_result_receiver) = mpsc::channel();

    let view = Rc::new(GameListView {
        root: root.clone(),
        stack,
        filter_bar,
        filter_entry: filter_entry.clone(),
        filter_result,
        column_view: column_view.clone(),
        file_type_column,
        architecture_column,
        size_column,
        play_time_column,
        add_ons_column,
        store,
        favorites,
        favorites_root,
        all_games: RefCell::new(Vec::new()),
        selection: selection.clone(),
        controller_navigation: ControllerNavigation::new(hid_core),
        hid_core: Arc::clone(hid_core),
        play_time_manager: Arc::clone(play_time_manager),
        on_activate,
        on_create_shortcut,
        on_refresh,
        refresh_button: refresh_button.clone(),
        runtime_lock: Rc::new(runtime_lock),
        property_dialog: RefCell::new(None),
        scan_generation: Arc::new(AtomicU64::new(0)),
        scan_result_sender,
        scan_result_receiver: RefCell::new(scan_result_receiver),
    });
    *context_view.borrow_mut() = Rc::downgrade(&view);

    // Activate (double-click / Enter) → boot a game; on a directory row, toggle
    // it open instead, which is what a tree row activation should do.
    column_view.connect_activate({
        let view = Rc::downgrade(&view);
        move |_, position| {
            if let Some(view) = view.upgrade() {
                view.activate_position(position);
            }
        }
    });

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed({
        let view = Rc::downgrade(&view);
        move |_, keyval, _, _| {
            let Some(key) = navigation_key_for_gdk(keyval) else {
                return glib::Propagation::Proceed;
            };
            if view
                .upgrade()
                .is_some_and(|view| view.handle_navigation(key))
            {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }
    });
    column_view.add_controller(keys);

    // HID callbacks can run outside GTK's main context. Drain their actions on
    // the UI thread and discard presses while the game list is not active,
    // matching upstream's `IsPoweredOn` / `isActiveWindow` guards.
    glib::timeout_add_local(std::time::Duration::from_millis(1), {
        let view = Rc::downgrade(&view);
        move || {
            let Some(view) = view.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let list_is_active = view.root.is_mapped()
                && view
                    .parent_window()
                    .is_some_and(|window| window.is_active());
            if list_is_active {
                for key in view.controller_navigation.take_pending_keys() {
                    view.handle_navigation(key);
                }
            } else {
                view.controller_navigation.discard_pending_keys();
            }
            glib::ControlFlow::Continue
        }
    });

    // `GameListWorker::ProcessEvents`: transfer plain scan results back to
    // GTK, where GObjects and textures must be created.
    glib::timeout_add_local(std::time::Duration::from_millis(16), {
        let view = Rc::downgrade(&view);
        move || {
            let Some(view) = view.upgrade() else {
                return glib::ControlFlow::Break;
            };
            view.process_scan_results();
            glib::ControlFlow::Continue
        }
    });

    // Toolbar + empty-state actions.
    for button in [&add_button, &empty.add_button] {
        let view = Rc::clone(&view);
        button.connect_clicked(move |_| view.prompt_add_directory());
    }
    refresh_button.connect_clicked({
        let view = Rc::downgrade(&view);
        move |_| {
            if let Some(view) = view.upgrade() {
                (view.on_refresh)();
            }
        }
    });
    filter_entry.connect_search_changed({
        let view = Rc::downgrade(&view);
        move |entry| {
            if let Some(view) = view.upgrade() {
                view.apply_filter(&entry.text());
            }
        }
    });
    filter_close.connect_clicked({
        let view = Rc::downgrade(&view);
        move |_| {
            let Some(view) = view.upgrade() else { return };
            view.set_filter_visible(false);
            uisettings::with_mut(|values| values.show_filter_bar.set_value(false));
            if let Some(action) = gio::Application::default()
                .and_downcast::<gtk::Application>()
                .and_then(|app| app.lookup_action("show_filter_bar"))
                .and_downcast::<gio::SimpleAction>()
            {
                action.set_state(&false.to_variant());
            }
            if let Err(error) = qt_config::save_view_values() {
                log::error!("Failed to save View menu settings: {error}");
            }
        }
    });

    // Populate after all row actions are connected.
    view.reload();
    view.set_filter_visible(uisettings::with(|values| {
        *values.show_filter_bar.get_value()
    }));

    (root.upcast(), GameListHandle(view))
}

/// A button showing an icon beside a text label.
fn icon_label_button(icon_name: &str, label: &str) -> gtk::Button {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&gtk::Image::from_icon_name(icon_name));
    content.append(&gtk::Label::new(Some(label)));

    let button = gtk::Button::new();
    button.set_child(Some(&content));
    button
}

/// The centred call-to-action shown when no game directory is configured.
struct EmptyState {
    root: gtk::Box,
    add_button: gtk::Button,
}

fn build_empty_state() -> EmptyState {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_valign(gtk::Align::Center);
    root.set_halign(gtk::Align::Center);
    root.set_hexpand(true);
    root.set_vexpand(true);

    let icon = gtk::Image::from_icon_name("folder-symbolic");
    icon.set_pixel_size(64);
    icon.add_css_class("dim-label");
    root.append(&icon);

    let title = gtk::Label::new(Some("No games found"));
    title.add_css_class("title-2");
    root.append(&title);

    let subtitle = gtk::Label::new(Some(
        "Add the folder that holds your Switch titles to get started.",
    ));
    subtitle.add_css_class("dim-label");
    root.append(&subtitle);

    let add_button = gtk::Button::with_label("Add Game Directory");
    add_button.add_css_class("suggested-action");
    add_button.set_halign(gtk::Align::Center);
    root.append(&add_button);

    EmptyState { root, add_button }
}

impl GameListView {
    fn set_filter_visible(&self, visible: bool) {
        self.filter_bar.set_visible(visible);
        if visible {
            self.filter_entry.grab_focus();
        } else {
            self.filter_entry.set_text("");
        }
    }

    /// Upstream `GameList::OnTextChanged`.
    fn apply_filter(&self, text: &str) {
        let query = text.to_lowercase();
        // Upstream hides the complete Favorites group while filtering, then
        // restores it only when at least one favorite remains.
        let has_configured_favorites = uisettings::with(|values| !values.favorited_ids.is_empty());
        self.set_favorites_visible(query.is_empty() && has_configured_favorites);

        let all_games = self.all_games.borrow();
        let total = all_games.iter().map(Vec::len).sum::<usize>();
        let mut visible = 0usize;

        for (directory_index, games) in all_games.iter().enumerate() {
            let Some(children) = self
                .directory_root(directory_index)
                .and_then(|entry| entry.children())
            else {
                continue;
            };
            children.remove_all();
            for game in games {
                if game_matches_filter(game, &query) {
                    children.append(game);
                    visible += 1;
                }
            }
        }

        let result = crate::i18n::tr_args("%1 of %n result(s)", &[visible.to_string()])
            .replace("%n", &total.to_string());
        self.filter_result.set_text(&result);
    }

    fn activate_position(&self, position: u32) {
        let Some(row) = self
            .selection
            .model()
            .and_then(|model| model.item(position))
            .and_downcast::<gtk::TreeListRow>()
        else {
            return;
        };
        let Some(entry) = row.item().and_downcast::<GameEntry>() else {
            return;
        };
        if entry.is_folder() {
            row.set_expanded(!row.is_expanded());
        } else {
            (self.on_activate)(entry.path(), StartGameType::Normal);
        }
    }

    fn handle_navigation(&self, key: NavigationKey) -> bool {
        let Some(model) = self.selection.model() else {
            return false;
        };
        let count = model.n_items();
        if count == 0 {
            return false;
        }

        let selected = self.selection.selected();
        match key {
            NavigationKey::Down => {
                let next = if selected == gtk::INVALID_LIST_POSITION {
                    0
                } else {
                    (selected + 1).min(count - 1)
                };
                self.select_position(next);
            }
            NavigationKey::Up => {
                let next = if selected == gtk::INVALID_LIST_POSITION {
                    0
                } else {
                    selected.saturating_sub(1)
                };
                self.select_position(next);
            }
            NavigationKey::Left | NavigationKey::Right => {
                if selected == gtk::INVALID_LIST_POSITION {
                    self.select_position(0);
                    return true;
                }
                let Some(row) = model.item(selected).and_downcast::<gtk::TreeListRow>() else {
                    return false;
                };
                if key == NavigationKey::Right {
                    if row.is_expandable() && !row.is_expanded() {
                        row.set_expanded(true);
                    } else if let Some(child) = row.child_row(0) {
                        self.select_position(child.position());
                    }
                } else if row.is_expanded() {
                    row.set_expanded(false);
                } else if let Some(parent) = row.parent() {
                    self.select_position(parent.position());
                }
            }
            NavigationKey::Enter => {
                if selected == gtk::INVALID_LIST_POSITION {
                    self.select_position(0);
                } else {
                    self.activate_position(selected);
                }
            }
            NavigationKey::Escape => return false,
        }
        true
    }

    fn select_position(&self, position: u32) {
        self.selection.set_selected(position);
        self.column_view.grab_focus();
    }

    /// `GameList::PopupContextMenu`: show the menu owned by the clicked row.
    fn popup_context_menu(
        self: &Rc<Self>,
        entry: &GameEntry,
        anchor: &gtk::Widget,
        x: f64,
        y: f64,
        on_activate: Rc<dyn Fn(String, StartGameType)>,
    ) {
        if entry.is_favorites() {
            self.popup_favorites_context_menu(anchor, x, y);
        } else if entry.is_built_in() {
            self.popup_built_in_directory_context_menu(entry, anchor, x, y);
        } else if entry.is_folder() {
            self.popup_directory_context_menu(entry, anchor, x, y);
        } else {
            self.popup_game_context_menu(entry, anchor, x, y, on_activate);
        }
    }

    /// Ruzu-specific menu for the read-only packaged free-game directory.
    fn popup_built_in_directory_context_menu(
        self: &Rc<Self>,
        entry: &GameEntry,
        anchor: &gtk::Widget,
        x: f64,
        y: f64,
    ) {
        let menu = gio::Menu::new();
        menu.append(
            Some(&crate::i18n::tr("Open Directory Location")),
            Some("game-list.open-directory"),
        );

        let actions = gio::SimpleActionGroup::new();
        let open_directory = gio::SimpleAction::new("open-directory", None);
        let path = entry.path();
        let view = Rc::downgrade(self);
        open_directory.connect_activate(move |_, _| {
            if let Some(view) = view.upgrade() {
                open_directory_location(Path::new(&path), view.parent_window().as_ref());
            }
        });
        actions.add_action(&open_directory);
        show_context_menu(anchor, &menu, &actions, x, y);
    }

    /// Upstream `GameList::AddFavoritesPopup`.
    fn popup_favorites_context_menu(self: &Rc<Self>, anchor: &gtk::Widget, x: f64, y: f64) {
        let menu = gio::Menu::new();
        menu.append(
            Some(&crate::i18n::tr("Clear")),
            Some("game-list.clear-favorites"),
        );

        let actions = gio::SimpleActionGroup::new();
        let clear = gio::SimpleAction::new("clear-favorites", None);
        clear.connect_activate({
            let view = Rc::downgrade(self);
            move |_, _| {
                if let Some(view) = view.upgrade() {
                    view.clear_favorites();
                }
            }
        });
        actions.add_action(&clear);

        show_context_menu(anchor, &menu, &actions, x, y);
    }

    /// `GameList::AddPermDirPopup` followed by `AddCustomDirPopup`.
    fn popup_directory_context_menu(
        self: &Rc<Self>,
        entry: &GameEntry,
        anchor: &gtk::Widget,
        x: f64,
        y: f64,
    ) {
        let path = entry.path();
        let (position, count) = filesystem_directory_position(&path);

        let menu = gio::Menu::new();
        menu.append(
            Some(&crate::i18n::tr("▲ Move Up")),
            Some("game-list.move-up"),
        );
        menu.append(
            Some(&crate::i18n::tr("▼ Move Down")),
            Some("game-list.move-down"),
        );
        menu.append(
            Some(&crate::i18n::tr("Open Directory Location")),
            Some("game-list.open-directory"),
        );
        menu.append(
            Some(&crate::i18n::tr("Scan Subfolders")),
            Some("game-list.scan-subfolders"),
        );
        menu.append(
            Some(&crate::i18n::tr("Remove Game Directory")),
            Some("game-list.remove-directory"),
        );

        let actions = gio::SimpleActionGroup::new();

        let move_up = gio::SimpleAction::new("move-up", None);
        move_up.set_enabled(position.is_some_and(|index| index > 0));
        {
            let view = Rc::downgrade(self);
            let path = path.clone();
            move_up.connect_activate(move |_, _| {
                if let Some(view) = view.upgrade() {
                    view.move_directory(&path, -1);
                }
            });
        }
        actions.add_action(&move_up);

        let move_down = gio::SimpleAction::new("move-down", None);
        move_down.set_enabled(position.is_some_and(|index| index + 1 < count));
        {
            let view = Rc::downgrade(self);
            let path = path.clone();
            move_down.connect_activate(move |_, _| {
                if let Some(view) = view.upgrade() {
                    view.move_directory(&path, 1);
                }
            });
        }
        actions.add_action(&move_down);

        let open_directory = gio::SimpleAction::new("open-directory", None);
        {
            let path = path.clone();
            let view = Rc::downgrade(self);
            open_directory.connect_activate(move |_, _| {
                if let Some(view) = view.upgrade() {
                    open_directory_location(Path::new(&path), view.parent_window().as_ref());
                }
            });
        }
        actions.add_action(&open_directory);

        let deep_scan = gio::SimpleAction::new_stateful(
            "scan-subfolders",
            None,
            &entry.deep_scan().to_variant(),
        );
        {
            let view = Rc::downgrade(self);
            let path = path.clone();
            deep_scan.connect_activate(move |action, _| {
                let enabled = !action
                    .state()
                    .and_then(|state| state.get::<bool>())
                    .unwrap_or(false);
                action.set_state(&enabled.to_variant());
                if let Some(view) = view.upgrade() {
                    view.set_deep_scan(&path, enabled);
                }
            });
        }
        actions.add_action(&deep_scan);

        let remove_directory = gio::SimpleAction::new("remove-directory", None);
        {
            let view = Rc::downgrade(self);
            remove_directory.connect_activate(move |_, _| {
                if let Some(view) = view.upgrade() {
                    view.remove_directory(&path);
                }
            });
        }
        actions.add_action(&remove_directory);

        show_context_menu(anchor, &menu, &actions, x, y);
    }

    /// Upstream `GameList::AddGamePopup`.
    fn popup_game_context_menu(
        self: &Rc<Self>,
        entry: &GameEntry,
        anchor: &gtk::Widget,
        x: f64,
        y: f64,
        on_activate: Rc<dyn Fn(String, StartGameType)>,
    ) {
        let path = entry.path();
        let program_id = entry.program_id();

        // `program_id == 0` hides the same title-id-dependent actions as
        // upstream's `setVisible(program_id != 0)` calls.
        let menu = gio::Menu::new();

        if program_id != 0 {
            let favorite_section = gio::Menu::new();
            favorite_section.append(
                Some(&crate::i18n::tr("Favorite")),
                Some("game-list.toggle-favorite"),
            );
            menu.append_section(None, &favorite_section);
        }

        let start_section = gio::Menu::new();
        start_section.append(
            Some(&crate::i18n::tr("Start Game")),
            Some("game-list.start-game"),
        );
        start_section.append(
            Some(&crate::i18n::tr("Start Game without Custom Configuration")),
            Some("game-list.start-game-global"),
        );
        menu.append_section(None, &start_section);

        let locations = gio::Menu::new();
        if program_id != 0 {
            locations.append(
                Some(&crate::i18n::tr("Open Save Data Location")),
                Some("game-list.open-save-data"),
            );
            locations.append(
                Some(&crate::i18n::tr("Open Mod Data Location")),
                Some("game-list.open-mod-data"),
            );
            locations.append(
                Some(&crate::i18n::tr("Open Transferable Pipeline Cache")),
                Some("game-list.open-pipeline-cache"),
            );
        }
        menu.append_section(None, &locations);

        let commands = gio::Menu::new();
        let remove = gio::Menu::new();
        let remove_individual = gio::Menu::new();
        if program_id != 0 {
            remove_individual.append(
                Some(&crate::i18n::tr("Remove Installed Update")),
                Some("game-list.remove-update"),
            );
            remove_individual.append(
                Some(&crate::i18n::tr("Remove All Installed DLC")),
                Some("game-list.remove-dlc"),
            );
        }
        remove_individual.append(
            Some(&crate::i18n::tr("Remove Custom Configuration")),
            Some("game-list.remove-custom-config"),
        );
        remove_individual.append(
            Some(&crate::i18n::tr("Remove Play Time Data")),
            Some("game-list.remove-play-time"),
        );
        remove_individual.append(
            Some(&crate::i18n::tr("Remove Cache Storage")),
            Some("game-list.remove-cache-storage"),
        );
        if program_id != 0 {
            remove_individual.append(
                Some(&crate::i18n::tr("Remove OpenGL Pipeline Cache")),
                Some("game-list.remove-gl-cache"),
            );
            remove_individual.append(
                Some(&crate::i18n::tr("Remove Vulkan Pipeline Cache")),
                Some("game-list.remove-vk-cache"),
            );
        }
        remove.append_section(None, &remove_individual);
        if program_id != 0 {
            let remove_all = gio::Menu::new();
            remove_all.append(
                Some(&crate::i18n::tr("Remove All Pipeline Caches")),
                Some("game-list.remove-all-caches"),
            );
            remove_all.append(
                Some(&crate::i18n::tr("Remove All Installed Contents")),
                Some("game-list.remove-all-content"),
            );
            remove.append_section(None, &remove_all);
        }
        commands.append_submenu(Some(&crate::i18n::tr("Remove")), &remove);

        let dump_romfs = gio::Menu::new();
        dump_romfs.append(
            Some(&crate::i18n::tr("Dump RomFS")),
            Some("game-list.dump-romfs"),
        );
        dump_romfs.append(
            Some(&crate::i18n::tr("Dump RomFS to SDMC")),
            Some("game-list.dump-romfs-sdmc"),
        );
        commands.append_submenu(Some(&crate::i18n::tr("Dump RomFS")), &dump_romfs);
        commands.append(
            Some(&crate::i18n::tr("Verify Integrity")),
            Some("game-list.verify-integrity"),
        );
        if program_id != 0 {
            commands.append(
                Some(&crate::i18n::tr("Copy Title ID to Clipboard")),
                Some("game-list.copy-title-id"),
            );
        }
        #[cfg(not(target_os = "macos"))]
        {
            let shortcuts = gio::Menu::new();
            shortcuts.append(
                Some(&crate::i18n::tr("Add to Desktop")),
                Some("game-list.shortcut-desktop"),
            );
            shortcuts.append(
                Some(&crate::i18n::tr("Add to Applications Menu")),
                Some("game-list.shortcut-applications"),
            );
            commands.append_submenu(Some(&crate::i18n::tr("Create Shortcut")), &shortcuts);
        }
        menu.append_section(None, &commands);

        let properties_section = gio::Menu::new();
        properties_section.append(
            Some(&crate::i18n::tr("Configure Game")),
            Some("game-list.properties"),
        );
        menu.append_section(None, &properties_section);

        let actions = gio::SimpleActionGroup::new();
        let start_game = gio::SimpleAction::new("start-game", None);
        {
            let path = path.clone();
            let on_activate = Rc::clone(&on_activate);
            start_game
                .connect_activate(move |_, _| on_activate(path.clone(), StartGameType::Normal));
        }
        actions.add_action(&start_game);

        let start_game_global = gio::SimpleAction::new("start-game-global", None);
        {
            let path = path.clone();
            start_game_global
                .connect_activate(move |_, _| on_activate(path.clone(), StartGameType::Global));
        }
        actions.add_action(&start_game_global);

        if program_id != 0 {
            let favorite = gio::SimpleAction::new_stateful(
                "toggle-favorite",
                None,
                &uisettings::with(|values| values.favorited_ids.contains(&program_id)).to_variant(),
            );
            favorite.connect_activate({
                let view = Rc::downgrade(self);
                move |action, _| {
                    let enabled = !action
                        .state()
                        .and_then(|value| value.get::<bool>())
                        .unwrap_or(false);
                    action.set_state(&enabled.to_variant());
                    if let Some(view) = view.upgrade() {
                        view.set_favorite(program_id, enabled);
                    }
                }
            });
            actions.add_action(&favorite);

            let open_save_data = gio::SimpleAction::new("open-save-data", None);
            {
                let view = Rc::downgrade(self);
                open_save_data.connect_activate(move |_, _| {
                    if let Some(view) = view.upgrade() {
                        view.open_save_data_location(program_id);
                    }
                });
            }
            actions.add_action(&open_save_data);

            let open_mod_data = gio::SimpleAction::new("open-mod-data", None);
            {
                let view = Rc::downgrade(self);
                open_mod_data.connect_activate(move |_, _| {
                    if let Some(view) = view.upgrade() {
                        view.open_mod_data_location(program_id);
                    }
                });
            }
            actions.add_action(&open_mod_data);

            let open_pipeline_cache = gio::SimpleAction::new("open-pipeline-cache", None);
            {
                let view = Rc::downgrade(self);
                open_pipeline_cache.connect_activate(move |_, _| {
                    if let Some(view) = view.upgrade() {
                        view.open_pipeline_cache_location(program_id);
                    }
                });
            }
            actions.add_action(&open_pipeline_cache);

            let copy_title_id = gio::SimpleAction::new("copy-title-id", None);
            copy_title_id.connect_activate(move |_, _| {
                if let Some(display) = gdk::Display::default() {
                    display.clipboard().set_text(&format!("{program_id:016X}"));
                }
            });
            actions.add_action(&copy_title_id);
        }

        let remove_play_time = gio::SimpleAction::new("remove-play-time", None);
        {
            let view = Rc::downgrade(self);
            remove_play_time.connect_activate(move |_, _| {
                if let Some(view) = view.upgrade() {
                    view.play_time_manager.reset_program_play_time(program_id);
                    view.reload();
                }
            });
        }
        actions.add_action(&remove_play_time);

        #[cfg(not(target_os = "macos"))]
        for (name, target) in [
            (
                "shortcut-desktop",
                crate::util::game::ShortcutTarget::Desktop,
            ),
            (
                "shortcut-applications",
                crate::util::game::ShortcutTarget::Applications,
            ),
        ] {
            let shortcut = gio::SimpleAction::new(name, None);
            let on_create_shortcut = Rc::clone(&self.on_create_shortcut);
            let path = path.clone();
            shortcut.connect_activate(move |_, _| {
                on_create_shortcut(program_id, path.clone(), target);
            });
            actions.add_action(&shortcut);
        }

        for (name, detail) in [
            (
                "remove-update",
                "Removing installed updates is not available yet.",
            ),
            ("remove-dlc", "Removing installed DLC is not available yet."),
            (
                "remove-custom-config",
                "Removing custom configurations is not available yet.",
            ),
            (
                "remove-cache-storage",
                "Removing cache storage is not available yet.",
            ),
            (
                "remove-gl-cache",
                "Removing OpenGL pipeline caches is not available yet.",
            ),
            (
                "remove-vk-cache",
                "Removing Vulkan pipeline caches is not available yet.",
            ),
            (
                "remove-all-caches",
                "Removing all pipeline caches is not available yet.",
            ),
            (
                "remove-all-content",
                "Removing installed contents is not available yet.",
            ),
            ("dump-romfs", "Dumping RomFS is not available yet."),
            (
                "dump-romfs-sdmc",
                "Dumping RomFS to SDMC is not available yet.",
            ),
            (
                "verify-integrity",
                "Integrity verification is not available yet.",
            ),
        ] {
            add_unavailable_action(&actions, name, self.parent_window(), detail);
        }

        let properties = gio::SimpleAction::new("properties", None);
        {
            let view = Rc::downgrade(self);
            let entry = entry.clone();
            properties.connect_activate(move |_, _| {
                if let Some(view) = view.upgrade() {
                    view.open_properties(&entry);
                }
            });
        }
        actions.add_action(&properties);

        show_context_menu(anchor, &menu, &actions, x, y);
    }

    fn open_properties(self: &Rc<Self>, entry: &GameEntry) {
        if let Some(dialog) = self.property_dialog.borrow().as_ref() {
            dialog.present();
            return;
        }

        let path = PathBuf::from(entry.path());
        let properties = crate::configuration::configure_per_game::GameProperties {
            name: entry.name(),
            developer: entry.developer(),
            version: entry.version(),
            title_id: entry.program_id(),
            format: entry.kind(),
            size: entry.size(),
            filename: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string(),
            path,
            icon: entry.icon(),
        };
        let dialog = crate::configuration::configure_per_game::ConfigurePerGame::new(
            self.parent_window().as_ref(),
            properties,
            Arc::clone(&self.hid_core),
            (self.runtime_lock)(),
        );
        dialog.connect_closed({
            let view = Rc::downgrade(self);
            move || {
                if let Some(view) = view.upgrade() {
                    view.property_dialog.borrow_mut().take();
                }
            }
        });
        dialog.present();
        *self.property_dialog.borrow_mut() = Some(dialog);
    }

    fn open_save_data_location(&self, program_id: u64) {
        let root = common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::NANDDir)
            .join("user/save");
        let title = format!("{program_id:016X}");
        let found = find_directory_named(&root, &title, 4);
        let path = found.unwrap_or_else(|| {
            root.join("0000000000000000")
                .join("00000000000000000000000000000000")
                .join(title)
        });
        if let Err(error) = std::fs::create_dir_all(&path) {
            log::error!(
                "Failed to create save data directory {}: {error}",
                path.display()
            );
            crate::gtk_compat::show_warning(
                self.parent_window().as_ref(),
                "Error Opening Save Data Folder",
                "The save data directory could not be created.",
            );
            return;
        }
        open_directory_location(&path, self.parent_window().as_ref());
    }

    /// `GMainWindow::OnGameListOpenFolder`, `GameListOpenTarget::ModData`.
    fn open_mod_data_location(&self, program_id: u64) {
        let path = common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::LoadDir)
            .join(format!("{program_id:016X}"));
        if !path.is_dir() {
            crate::gtk_compat::show_warning(
                self.parent_window().as_ref(),
                "Error Opening Mod Data Folder",
                "Folder does not exist!",
            );
            return;
        }
        open_directory_location(&path, self.parent_window().as_ref());
    }

    /// `GMainWindow::OnTransferableShaderCacheOpenFile`.
    fn open_pipeline_cache_location(&self, program_id: u64) {
        let path = common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::ShaderDir)
            .join(format!("{program_id:016x}"));
        if let Err(error) = std::fs::create_dir_all(&path) {
            log::error!(
                "Failed to create pipeline cache directory {}: {error}",
                path.display()
            );
            crate::gtk_compat::show_warning(
                self.parent_window().as_ref(),
                "Error Opening Transferable Pipeline Cache",
                "Failed to create the pipeline cache directory for this title.",
            );
            return;
        }
        open_directory_location(&path, self.parent_window().as_ref());
    }

    fn parent_window(&self) -> Option<gtk::Window> {
        self.root.root().and_downcast::<gtk::Window>()
    }

    /// Return the Nth configured-directory root independently of whether the
    /// optional Favorites row currently occupies position zero.
    fn directory_root(&self, directory_index: usize) -> Option<GameEntry> {
        (0..self.store.n_items())
            .filter_map(|position| self.store.item(position).and_downcast::<GameEntry>())
            .filter(|entry| !entry.is_favorites())
            .nth(directory_index)
    }

    fn favorites_root_position(&self) -> Option<u32> {
        (0..self.store.n_items()).find(|position| {
            self.store
                .item(*position)
                .and_downcast::<GameEntry>()
                .is_some_and(|entry| entry.is_favorites())
        })
    }

    /// Upstream hides Favorites when empty or while a filter is active.
    fn set_favorites_visible(&self, visible: bool) {
        match (visible, self.favorites_root_position()) {
            (true, None) => {
                self.store.insert(0, &self.favorites_root);
                let Some(row) = self
                    .selection
                    .model()
                    .and_then(|model| model.item(0))
                    .and_downcast::<gtk::TreeListRow>()
                else {
                    return;
                };
                row.set_expanded(uisettings::with(|values| {
                    *values.favorites_expanded.get_value()
                }));
                let store = self.store.clone();
                let favorites_root = self.favorites_root.clone();
                row.connect_expanded_notify(move |row| {
                    // Removing the GTK root to emulate Qt's hidden row emits a
                    // synthetic collapse. It is not a user action and must not
                    // overwrite upstream's persistent expanded state.
                    let root_is_visible = store_contains_entry(&store, &favorites_root);
                    if !root_is_visible {
                        return;
                    }
                    uisettings::with_mut(|values| {
                        values.favorites_expanded.set_value(row.is_expanded())
                    });
                    if let Err(error) = qt_config::save_favorites_expanded() {
                        log::error!("Failed to save Favorites expanded state: {error}");
                    }
                });
            }
            (false, Some(position)) => self.store.remove(position),
            _ => {}
        }
    }

    /// Upstream `GameList::AddFavorite`: rebuild cloned rows in configured-id
    /// order from the already scanned directory entries.
    fn rebuild_favorites(&self) {
        let ids = uisettings::with(|values| values.favorited_ids.clone());
        let favorites = favorite_entries(&ids, &self.all_games.borrow());
        self.favorites.remove_all();
        for game in favorites {
            self.favorites.append(&game);
        }
    }

    /// Upstream `GameList::ToggleFavorite` plus `SaveConfig`.
    fn set_favorite(&self, program_id: u64, enabled: bool) {
        let (ids, reveal_first_favorite) = uisettings::with_mut(|values| {
            let reveal_first_favorite = enabled && values.favorited_ids.is_empty();
            if enabled {
                if !values.favorited_ids.contains(&program_id) {
                    values.favorited_ids.push(program_id);
                }
            } else if let Some(position) =
                values.favorited_ids.iter().position(|id| *id == program_id)
            {
                values.favorited_ids.remove(position);
            }
            if reveal_first_favorite {
                // Qt keeps a hidden root alive and expanded. GTK has to insert
                // a new visible row, so explicitly reproduce that reveal.
                values.favorites_expanded.set_value(true);
            }
            (values.favorited_ids.clone(), reveal_first_favorite)
        });
        if let Err(error) = qt_config::save_favorited_ids(&ids) {
            log::error!("Failed to save favorite title: {error}");
        }
        if reveal_first_favorite {
            if let Err(error) = qt_config::save_favorites_expanded() {
                log::error!("Failed to reveal the first favorite: {error}");
            }
        }
        self.rebuild_favorites();
        self.apply_filter(&self.filter_entry.text());
    }

    /// Upstream `GameList::AddFavoritesPopup` clear action.
    fn clear_favorites(&self) {
        uisettings::with_mut(|values| values.favorited_ids.clear());
        if let Err(error) = qt_config::save_favorited_ids(&[]) {
            log::error!("Failed to clear favorite titles: {error}");
        }
        self.rebuild_favorites();
        self.apply_filter(&self.filter_entry.text());
    }

    /// Rescan every configured directory and rebuild the tree — upstream
    /// re-runs `GameListWorker` after the directory list changes.
    fn reload(&self) {
        self.update_column_visibility();

        // Rebuilding the store drops the selection; remember which directory
        // was picked so it can be restored afterwards.
        let previously_selected = selected_directory_path(&self.selection);

        let dirs = uisettings::with(|v| v.game_dirs.clone());
        let mut scannable: Vec<ScanDirectory> = dirs
            .into_iter()
            .filter(GameDir::is_filesystem_path)
            .map(ScanDirectory::configured)
            .collect();
        if let Some(directory) = crate::free_games::packaged_directory() {
            scannable.push(ScanDirectory::packaged_free_games(directory));
        }
        let directory_to_select =
            preferred_directory_path(previously_selected.as_deref(), &scannable);

        self.store.remove_all();
        self.all_games.borrow_mut().clear();
        for dir in &scannable {
            self.store.append(&GameEntry::new_folder(
                &dir.name,
                &dir.path,
                dir.deep_scan,
                dir.is_built_in,
                gio::ListStore::new::<GameEntry>(),
            ));
        }
        self.rebuild_favorites();

        self.stack.set_visible_child_name(if scannable.is_empty() {
            PAGE_EMPTY
        } else {
            PAGE_LIST
        });

        if let Some(path) = directory_to_select.as_deref() {
            self.select_directory(path);
        }
        self.apply_filter(&self.filter_entry.text());

        let generation = self.scan_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let current_generation = Arc::clone(&self.scan_generation);
        let sender = self.scan_result_sender.clone();
        let spawn_result = std::thread::Builder::new()
            .name("GameListWorker".to_string())
            .spawn(move || {
                let mut metadata_reader = MetadataReader::new();
                clear_frontend_manual_content_provider();
                if current_generation.load(Ordering::Acquire) != generation {
                    return;
                }

                let mut directories = Vec::with_capacity(scannable.len());
                for directory in scannable {
                    if current_generation.load(Ordering::Acquire) != generation {
                        return;
                    }
                    if !directory.is_built_in {
                        populate_frontend_manual_content_provider(&[GameDir {
                            path: directory.path.clone(),
                            deep_scan: directory.deep_scan,
                            expanded: true,
                        }]);
                    }
                    let games = scan_dir_games(
                        Path::new(&directory.path),
                        directory.deep_scan,
                        &mut metadata_reader,
                    );
                    directories.push(ScannedDirectory {
                        name: directory.name,
                        path: directory.path,
                        deep_scan: directory.deep_scan,
                        is_built_in: directory.is_built_in,
                        games,
                    });
                }

                if current_generation.load(Ordering::Acquire) == generation {
                    let _ = sender.send(GameListScanResult {
                        generation,
                        directories,
                        directory_to_select,
                    });
                }
            });
        if let Err(error) = spawn_result {
            log::error!("Failed to start GameListWorker: {error}");
        }
    }

    /// Drain `GameListWorker` results and materialize their GTK rows.
    fn process_scan_results(&self) {
        let generation = self.scan_generation.load(Ordering::Acquire);
        let result = take_current_scan_result(&self.scan_result_receiver.borrow(), generation);
        let Some(result) = result else { return };

        self.store.remove_all();
        self.all_games.borrow_mut().clear();

        let mut total = 0;
        for directory in result.directories {
            total += directory.games.len();
            let children = gio::ListStore::new::<GameEntry>();
            let mut all_games = Vec::with_capacity(directory.games.len());
            for game in directory.games {
                let icon = game.icon.as_ref().and_then(|bytes| {
                    gdk::Texture::from_bytes(&glib::Bytes::from(bytes.as_slice())).ok()
                });
                let entry = GameEntry::new_game(
                    &game.name,
                    &game.developer,
                    &game.version,
                    &game.kind,
                    &game.architecture,
                    &human_size(game.size),
                    &frontend_common::play_time_manager::PlayTimeManager::get_readable_play_time(
                        self.play_time_manager.get_play_time(game.program_id),
                    ),
                    &game.add_ons,
                    &game.path.to_string_lossy(),
                    icon,
                    game.program_id,
                );
                children.append(&entry);
                all_games.push(entry);
            }
            self.all_games.borrow_mut().push(all_games);
            self.store.append(&GameEntry::new_folder(
                &directory.name,
                &directory.path,
                directory.deep_scan,
                directory.is_built_in,
                children,
            ));
        }
        self.rebuild_favorites();

        log::info!(
            "Game list: found {total} game(s) across {} directory(ies)",
            self.all_games.borrow().len()
        );
        if let Some(path) = result.directory_to_select {
            self.select_directory(&path);
        }
        self.apply_filter(&self.filter_entry.text());
    }

    /// Eden `GameTree::UpdateColumnVisibility`.
    fn update_column_visibility(&self) {
        uisettings::with(|values| {
            self.file_type_column
                .set_visible(*values.show_types.get_value());
            self.architecture_column.set_visible(true);
            self.size_column.set_visible(*values.show_size.get_value());
            self.play_time_column
                .set_visible(*values.show_play_time.get_value());
            self.add_ons_column
                .set_visible(*values.show_add_ons.get_value());
        });
    }

    /// Re-select the directory row for `path` after a rescan.
    ///
    /// The tree model is flat while every directory is collapsed, so the row
    /// index equals the directory index — but expanded directories contribute
    /// their games, so search the model rather than assuming.
    fn select_directory(&self, path: &str) {
        let model = self.selection.model();
        let Some(model) = model else { return };
        for position in 0..model.n_items() {
            let matches = model
                .item(position)
                .and_downcast::<gtk::TreeListRow>()
                .and_then(|row| row.item())
                .and_downcast::<GameEntry>()
                .is_some_and(|entry| entry.is_folder() && entry.path() == path);
            if matches {
                self.selection.set_selected(position);
                return;
            }
        }
    }

    /// Ask for a directory and add it — upstream `GMainWindow::OnGameListAddDirectory`.
    fn prompt_add_directory(self: &Rc<Self>) {
        let parent = self.root.root().and_downcast::<gtk::Window>();
        let view = Rc::clone(self);
        crate::gtk_compat::select_folder(parent.as_ref(), "Select Game Directory", move |result| {
            let Some(folder) = result else { return };
            let Some(path) = folder.path() else { return };
            view.add_directory(&path.to_string_lossy());
        });
    }

    /// Add `path` to the configured directories, unless it is already there.
    fn add_directory(&self, path: &str) {
        let already_present = uisettings::with(|v| v.game_dirs.iter().any(|d| d.path == path));
        if already_present {
            log::info!("Game list: {path} is already a game directory");
            return;
        }
        uisettings::with_mut(|v| {
            v.game_dirs.push(GameDir {
                path: path.to_string(),
                // User-facing ruzu default: discover titles in nested folders
                // immediately. The context-menu action can still disable it
                // per directory.
                deep_scan: NEW_DIRECTORY_DEEP_SCAN,
                expanded: true,
            })
        });
        self.persist();
        self.reload();
        self.select_directory(path);
    }

    /// Remove the directory at `path` — upstream's "Remove Game Directory".
    fn remove_directory(&self, path: &str) {
        uisettings::with_mut(|v| v.game_dirs.retain(|d| d.path != path));
        self.persist();
        self.reload();
    }

    /// Move a custom directory by one visible row, matching
    /// `GameList::AddPermDirPopup`.
    fn move_directory(&self, path: &str, direction: isize) {
        let moved = uisettings::with_mut(|values| {
            move_filesystem_directory(&mut values.game_dirs, path, direction)
        });
        if !moved {
            return;
        }
        self.persist();
        self.reload();
        self.select_directory(path);
    }

    /// Toggle recursive scanning for `path` — upstream's "Scan Subfolders".
    fn set_deep_scan(&self, path: &str, deep_scan: bool) {
        uisettings::with_mut(|v| {
            if let Some(dir) = v.game_dirs.iter_mut().find(|d| d.path == path) {
                dir.deep_scan = deep_scan;
            }
        });
        self.persist();
        self.reload();
    }

    /// Write the directory list back to ruzu's own config.
    fn persist(&self) {
        let dirs = uisettings::with(|v| v.game_dirs.clone());
        if let Err(e) = qt_config::save_game_dirs(&dirs) {
            log::error!("Failed to save game directories: {e}");
        }
    }
}

pub(crate) fn navigation_key_for_gdk(keyval: gdk::Key) -> Option<NavigationKey> {
    match keyval {
        gdk::Key::Return | gdk::Key::KP_Enter => Some(NavigationKey::Enter),
        gdk::Key::Escape => Some(NavigationKey::Escape),
        gdk::Key::Down => Some(NavigationKey::Down),
        gdk::Key::Left => Some(NavigationKey::Left),
        gdk::Key::Right => Some(NavigationKey::Right),
        gdk::Key::Up => Some(NavigationKey::Up),
        _ => None,
    }
}

/// Path of the directory row currently selected, if any.
fn selected_directory_path(selection: &gtk::SingleSelection) -> Option<String> {
    selection
        .selected_item()
        .and_downcast::<gtk::TreeListRow>()
        .and_then(|row| row.item())
        .and_downcast::<GameEntry>()
        .filter(|entry| entry.is_folder() && !entry.is_favorites())
        .map(|entry| entry.path())
}

/// Find each configured favorite in the scanned directories and clone its row,
/// matching upstream `GameList::AddFavorite`'s first-match behavior.
fn favorite_entries(ids: &[u64], directories: &[Vec<GameEntry>]) -> Vec<GameEntry> {
    ids.iter()
        .filter_map(|program_id| {
            directories
                .iter()
                .flatten()
                .find(|game| game.program_id() == *program_id)
                .map(GameEntry::clone_game)
        })
        .collect()
}

fn store_contains_entry(store: &gio::ListStore, expected: &GameEntry) -> bool {
    (0..store.n_items()).any(|position| {
        store
            .item(position)
            .and_downcast::<GameEntry>()
            .is_some_and(|entry| entry == *expected)
    })
}

/// Preserve the selected directory across a reload. If there was no usable
/// selection and only one filesystem directory exists, select that directory
/// so the toolbar actions target it immediately.
fn preferred_directory_path(
    previously_selected: Option<&str>,
    directories: &[ScanDirectory],
) -> Option<String> {
    if let Some(path) = previously_selected {
        if directories.iter().any(|directory| directory.path == path) {
            return Some(path.to_owned());
        }
    }

    if directories.len() == 1 {
        return Some(directories[0].path.clone());
    }

    None
}

/// Install the game-list CSS once.
///
/// Two effects upstream gets from Qt for free:
///  * `QTreeView::alternatingRowColors`, set on the game list in `main.ui`,
///    which produces the grey/white banding;
///  * `QPalette::Highlight` for the selected row.
///
/// GTK4's `ColumnView` has no alternating-row property, so the banding is done
/// with `:nth-child(even)` over the row widgets, derived from
/// `@theme_base_color` so it stays legible in both light and dark themes (see
/// `main_window::update_ui_theme`).
fn install_list_css() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Some(display) = gdk::Display::default() else {
            return;
        };
        let provider = gtk::CssProvider::new();
        provider.load_from_data(&format!(
            ".ruzu-game-list > listview > row:nth-child(even) {{\
                 background-color: shade(@theme_base_color, {ALTERNATE_ROW_SHADE});\
             }}\
             .ruzu-game-list > listview > row:nth-child(odd) {{\
                 background-color: @theme_base_color;\
             }}\
             .ruzu-game-list > listview > row:selected {{\
                 background-color: {SELECTION_BG};\
                 color: #ffffff;\
             }}\
             .ruzu-game-list > listview > row:selected:focus {{\
                 outline: none;\
             }}\
             .ruzu-toolbar {{\
                 background-color: shade(@theme_bg_color, 1.02);\
                 border-bottom: 1px solid @borders;\
             }}\
             popover.ruzu-context-menu > contents,\
             popover.ruzu-context-menu contents {{\
                 border-radius: 0;\
             }}\
             popover.ruzu-context-menu > contents {{\
                 padding-top: 3px;\
                 padding-bottom: 3px;\
             }}\
             popover.ruzu-context-menu modelbutton {{\
                 min-height: 20px;\
                 padding-top: 2px;\
                 padding-bottom: 2px;\
             }}"
        ));
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
}

/// Selected-row background — Qt's `QPalette::Highlight` as the Fusion style
/// defines it, sampled from yuzu's own game list.
///
/// This is deliberately *not* GTK's `@theme_selected_bg_color`. yuzu runs its
/// default ("colorful") theme without a stylesheet, so its highlight comes from
/// the Qt style palette, which is a fixed blue rather than the desktop accent
/// colour. Inheriting the GTK accent instead makes the row orange on Ubuntu's
/// Yaru theme, purple on some others — a different colour per desktop, where
/// yuzu is blue everywhere.
const SELECTION_BG: &str = "#308CC6";

/// Alternating-row shade, likewise sampled from yuzu (`#F7F7F7` over white).
/// Expressed as a shade factor so it also works on a dark theme.
const ALTERNATE_ROW_SHADE: f32 = 0.97;

/// The "Name" column: expander, icon, and label, so a directory row can be
/// collapsed and its games are indented under it. Upstream likewise puts the
/// icon inside the Name column rather than in a column of its own.
fn make_name_column(on_context_menu: ContextMenuHandler) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let picture = gtk::Picture::new();
        // GTK 4.8 renamed this pair to ContentFit::Contain.
        picture.set_keep_aspect_ratio(true);
        picture.set_can_shrink(true);
        let label = gtk::Label::builder().xalign(0.0).build();
        row.append(&picture);
        row.append(&label);

        let expander = gtk::TreeExpander::new();
        expander.set_child(Some(&row));
        install_context_menu_gesture(&expander, item, Rc::clone(&on_context_menu));

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

        let Some(entry) = tree_row.item().and_downcast::<GameEntry>() else {
            return;
        };
        let Some(row) = expander.child().and_downcast::<gtk::Box>() else {
            return;
        };
        let Some(picture) = row.first_child().and_downcast::<gtk::Picture>() else {
            return;
        };
        let Some(label) = row.last_child().and_downcast::<gtk::Label>() else {
            return;
        };

        if entry.is_folder() {
            picture.set_size_request(FOLDER_ICON_SIZE, FOLDER_ICON_SIZE);
            picture.set_paintable(entry.icon().as_ref());
        } else {
            picture.set_size_request(ICON_SIZE, ICON_SIZE);
            picture.set_paintable(entry.icon().as_ref());
        }
        label.set_label(&entry.name());
    });

    let column = gtk::ColumnViewColumn::new(Some(&crate::i18n::tr("Name")), Some(factory));
    column.set_expand(true);
    column.set_resizable(true);
    column
}

/// Decode one of the embedded upstream game-list icons.
fn embedded_icon(png: &'static [u8]) -> Option<gdk::Texture> {
    gdk::Texture::from_bytes(&glib::Bytes::from_static(png)).ok()
}

/// Build one plain-text column bound to a `GameEntry` string getter.
fn make_text_column(
    title: &str,
    getter: fn(&GameEntry) -> String,
    on_context_menu: ContextMenuHandler,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let label = gtk::Label::builder().xalign(0.0).build();
        install_context_menu_gesture(&label, item, Rc::clone(&on_context_menu));
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(label) = item.child().and_downcast::<gtk::Label>() else {
            return;
        };
        let text = item
            .item()
            .and_downcast::<gtk::TreeListRow>()
            .and_then(|row| row.item())
            .and_downcast::<GameEntry>()
            .map(|entry| getter(&entry))
            .unwrap_or_default();
        label.set_label(&text);
    });

    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_resizable(true);
    column
}

/// Attach upstream's custom-context-menu behavior directly to one recycled
/// `ColumnView` cell. The `TreeListRow` held by the `ListItem` is the reliable
/// GTK4 equivalent of Qt's `QTreeView::indexAt(menu_location)`.
fn install_context_menu_gesture(
    anchor: &impl IsA<gtk::Widget>,
    item: &gtk::ListItem,
    on_context_menu: ContextMenuHandler,
) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(gdk::BUTTON_SECONDARY);
    let item = item.downgrade();
    let anchor = anchor.clone().upcast::<gtk::Widget>();
    let menu_anchor = anchor.clone();
    gesture.connect_pressed(move |gesture, _, x, y| {
        let Some(item) = item.upgrade() else { return };
        let Some(tree_row) = item.item().and_downcast::<gtk::TreeListRow>() else {
            return;
        };
        let Some(entry) = tree_row.item().and_downcast::<GameEntry>() else {
            return;
        };
        on_context_menu(entry, menu_anchor.clone(), tree_row.position(), x, y);
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    anchor.add_controller(gesture);
}

/// Present a GTK menu at the click point. The action group is installed on the
/// clicked cell so `game-list.*` resolves exactly for this popup.
fn show_context_menu(
    anchor: &gtk::Widget,
    menu: &gio::Menu,
    actions: &gio::SimpleActionGroup,
    x: f64,
    y: f64,
) {
    // `from_model` defaults to GTK's touch-oriented sliding pages, which only
    // open after clicking the chevron. Eden's QMenu uses traditional nested
    // popovers that open when the pointer enters their row.
    let popover = gtk::PopoverMenu::from_model_full(menu, context_menu_flags());
    // Upstream `QMenu` uses straight edges with the default Fusion style.
    // Override GTK themes that round popovers so the title menu matches it.
    popover.add_css_class("ruzu-context-menu");
    popover.set_has_arrow(false);
    popover.insert_action_group("game-list", Some(actions));
    popover.set_parent(anchor);
    popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    popover.connect_closed(|popover| {
        let popover = popover.clone();
        glib::idle_add_local_once(move || popover.unparent());
    });
    popover.popup();
}

fn context_menu_flags() -> gtk::PopoverMenuFlags {
    gtk::PopoverMenuFlags::NESTED
}

fn add_unavailable_action(
    actions: &gio::SimpleActionGroup,
    name: &str,
    parent: Option<gtk::Window>,
    detail: &'static str,
) {
    let action = gio::SimpleAction::new(name, None);
    action.connect_activate(move |_, _| {
        crate::gtk_compat::show_warning(parent.as_ref(), "Game List", detail);
    });
    actions.add_action(&action);
}

fn find_directory_named(root: &Path, name: &str, remaining_depth: usize) -> Option<PathBuf> {
    if remaining_depth == 0 {
        return None;
    }
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.file_name().and_then(|part| part.to_str()) == Some(name) {
            return Some(path);
        }
        if let Some(found) = find_directory_named(&path, name, remaining_depth - 1) {
            return Some(found);
        }
    }
    None
}

/// Upstream `GMainWindow::OnGameListOpenDirectory`.
fn open_directory_location(path: &Path, parent: Option<&gtk::Window>) {
    if !path.is_dir() {
        crate::gtk_compat::show_warning(parent, "Error Opening Folder", "Folder does not exist!");
        return;
    }

    if let Err(error) = crate::util::game::open_folder(path) {
        log::error!("Failed to open directory {}: {error}", path.display());
        crate::gtk_compat::show_warning(
            parent,
            "Error Opening Folder",
            "The folder could not be opened in the system file manager.",
        );
    }
}

/// Visible index and visible directory count for `path`.
fn filesystem_directory_position(path: &str) -> (Option<usize>, usize) {
    uisettings::with(|values| {
        let paths: Vec<&str> = values
            .game_dirs
            .iter()
            .filter(|directory| directory.is_filesystem_path())
            .map(|directory| directory.path.as_str())
            .collect();
        (
            paths.iter().position(|candidate| *candidate == path),
            paths.len(),
        )
    })
}

/// Swap one custom directory with the adjacent visible custom directory.
fn move_filesystem_directory(directories: &mut [GameDir], path: &str, direction: isize) -> bool {
    let visible: Vec<usize> = directories
        .iter()
        .enumerate()
        .filter(|(_, directory)| directory.is_filesystem_path())
        .map(|(index, _)| index)
        .collect();
    let Some(visible_index) = visible
        .iter()
        .position(|index| directories[*index].path == path)
    else {
        return false;
    };
    let target = visible_index as isize + direction;
    if !(0..visible.len() as isize).contains(&target) {
        return false;
    }
    directories.swap(visible[visible_index], visible[target as usize]);
    true
}

// ---------------------------------------------------------------------------
// Scanning
// ---------------------------------------------------------------------------

/// A discovered game file, enriched with metadata read from the container.
struct GameFile {
    /// Display name: the real title from the control data if available, else the
    /// filename.
    name: String,
    developer: String,
    version: String,
    kind: String,
    architecture: String,
    size: u64,
    path: PathBuf,
    program_id: u64,
    add_ons: String,
    /// Icon JPEG bytes from the control data, if any.
    icon: Option<Vec<u8>>,
}

/// `GameListWorker::ProcessEvents` equivalent for the channel adaptation.
/// Drain every completed scan so an obsolete result can never be applied
/// after the most recent refresh.
fn take_current_scan_result(
    receiver: &mpsc::Receiver<GameListScanResult>,
    generation: u64,
) -> Option<GameListScanResult> {
    receiver
        .try_iter()
        .filter(|result| result.generation == generation)
        .last()
}

/// Scan one directory and return the games it holds, sorted by title.
///
/// Mirrors upstream `GameListWorker::ScanFileSystem`: a candidate file is only
/// listed once a `Loader` accepts it *and* reports a real file type. Container
/// formats must additionally carry Application/Program content, which keeps
/// update/DLC-only packages out of the list.
///
/// ```cpp
/// const auto file_type = loader->GetFileType();
/// if (file_type == Loader::FileType::Unknown || file_type == Loader::FileType::Error) {
///     return true;   // skip
/// }
/// ```
fn scan_dir_games(dir: &Path, deep_scan: bool, reader: &mut MetadataReader) -> Vec<GameFile> {
    let mut candidates = Vec::new();
    collect_candidates(dir, deep_scan, &mut candidates);

    // Load each candidate once, keeping only what the loader accepts, and take
    // its title + icon from the same loader (upstream reuses the one loader for
    // `GetFileType` / `ReadTitle` / `ReadIcon` too).
    let mut games = Vec::with_capacity(candidates.len());
    for mut game in candidates {
        let Some(metadata) = reader.read(&game.path.to_string_lossy()) else {
            log::debug!(
                "Game list: skipping {} (no loader accepted it)",
                game.path.display()
            );
            continue;
        };
        if let Some(title) = metadata.title {
            game.name = title;
        }
        game.developer = metadata.developer;
        game.version = metadata.version;
        game.icon = metadata.icon;
        game.program_id = metadata.program_id;
        game.add_ons = metadata.add_ons;
        game.architecture = metadata.architecture;
        games.push(game);
    }

    games.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    games
}

/// Upstream `ScanTarget::FillManualContentProvider` pass over every configured
/// filesystem game directory.
pub(crate) fn populate_manual_content_provider(
    vfs: &Arc<RealVfsFilesystem>,
    provider: &mut ManualContentProvider,
    directories: &[GameDir],
) {
    let add_container_content = *common::settings::values()
        .ext_content_from_game_dirs
        .get_value();

    let mut candidates = Vec::new();
    for directory in directories {
        collect_candidates(
            Path::new(&directory.path),
            directory.deep_scan,
            &mut candidates,
        );
    }

    for candidate in candidates {
        let Some(file) = vfs.arc_open_file(&candidate.path.to_string_lossy(), OpenMode::READ)
        else {
            continue;
        };
        match identify_file(&file) {
            FileType::NCA => {
                let nca = NCA::new(file.clone(), None);
                if nca.get_status() == FsResultStatus::Success {
                    provider.add_entry(
                        TitleType::Application,
                        get_cr_type_from_nca_type(nca.get_type() as u8),
                        nca.get_title_id(),
                        file,
                    );
                }
            }
            FileType::NSP | FileType::XCI => {
                if add_container_content {
                    let _ = provider.add_entries_from_container(file, false, None);
                }
            }
            _ => {}
        }
    }
}

fn clear_frontend_manual_content_provider() {
    let providers = frontend_content_providers();
    let mut manual = providers
        .manual
        .inner
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    manual.clear_all_entries();
}

fn populate_frontend_manual_content_provider(directories: &[GameDir]) {
    let providers = frontend_content_providers();
    let mut manual = providers
        .manual
        .inner
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    populate_manual_content_provider(&providers.vfs, &mut manual, directories);
}

/// Collect candidate game files under `dir`, recursively when `deep_scan` is set.
fn collect_candidates(dir: &Path, deep_scan: bool, games: &mut Vec<GameFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // `directory_entry::status()` in upstream follows symbolic links.
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            if deep_scan {
                collect_candidates(&path, true, games);
            }
            continue;
        }
        let is_extracted_nca_main = path.file_name().and_then(|name| name.to_str()) == Some("main");
        let ext_lower = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_lowercase);
        if !is_extracted_nca_main
            && !ext_lower
                .as_deref()
                .is_some_and(|ext| SUPPORTED_EXTENSIONS.contains(&ext))
        {
            continue;
        }
        let name = if is_extracted_nca_main {
            path.parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or("main")
                .to_owned()
        } else {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("")
                .to_owned()
        };
        games.push(GameFile {
            name,
            developer: String::new(),
            version: "1.0.0".to_string(),
            kind: ext_lower.as_deref().unwrap_or("NCA").to_uppercase(),
            architecture: String::new(),
            size: metadata.len(),
            path,
            program_id: 0,
            add_ons: String::new(),
            icon: None,
        });
    }
}

/// Reads a game's control-data metadata (title, icon) without booting it.
///
/// Mirrors upstream `GameListWorker`'s use of `Loader::GetLoader` +
/// `ReadTitle`/`ReadIcon`. The loader only needs a lightweight
/// `loader::System` (content provider + filesystem controller), not the full
/// emulation `Core::System`. Keys come from the global `KeyManager` singleton.
struct MetadataReader {
    vfs: Arc<RealVfsFilesystem>,
    content_provider: Arc<Mutex<ContentProviderUnion>>,
    controller: Arc<Mutex<FileSystemController>>,
    loader_system: LoaderSystem,
}

impl MetadataReader {
    fn new() -> Self {
        let providers = frontend_content_providers();
        let vfs = Arc::clone(&providers.vfs);
        let content_provider = Arc::clone(&providers.union);
        let mut controller = FileSystemController::new();
        controller.set_content_provider(content_provider.clone());
        controller.create_factories(vfs.clone(), false);
        let controller = Arc::new(Mutex::new(controller));
        let loader_system = LoaderSystem::new(
            Some(Arc::clone(&content_provider)),
            Some(Arc::clone(&controller)),
        );
        Self {
            vfs,
            content_provider,
            controller,
            loader_system,
        }
    }

    /// Metadata for a game the loader accepted; `None` when the file is not a
    /// bootable title (no loader, or `FileType::Unknown` / `FileType::Error`).
    fn read(&mut self, path: &str) -> Option<GameMetadata> {
        let file = self.vfs.arc_open_file(path, OpenMode::READ)?;
        let loader = get_loader(&mut self.loader_system, file.clone(), 0, 0)?;

        let file_type = loader.get_file_type();
        if matches!(file_type, FileType::Unknown | FileType::Error) {
            return None;
        }
        if matches!(file_type, FileType::NSP | FileType::XCI)
            && !is_bootable_game_container(file.clone(), file_type, 0, 0)
        {
            return None;
        }

        let mut title = String::new();
        let title = if loader.read_title(&mut title) == ResultStatus::Success && !title.is_empty() {
            Some(title)
        } else {
            None
        };

        let mut icon = Vec::new();
        let icon = if loader.read_icon(&mut icon) == ResultStatus::Success && !icon.is_empty() {
            Some(icon)
        } else {
            None
        };

        let mut program_id = 0;
        if loader.read_program_id(&mut program_id) != ResultStatus::Success {
            program_id = 0;
        }

        let mut control = NACP::new();
        let (developer, version) =
            if loader.read_control_data(&mut control) == ResultStatus::Success {
                (control.get_developer_name(), control.get_version_string())
            } else {
                (String::new(), "1.0.0".to_string())
            };

        let mut update_raw = None;
        loader.read_update_raw(&mut update_raw);
        let (patches, architecture) = {
            let controller = self
                .controller
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let content_provider = self
                .content_provider
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let patch_manager = PatchManager::new(program_id, &controller, &*content_provider);
            let patches = patch_manager.get_patches(update_raw);
            let architecture = normalize_architecture_label(get_game_list_cached_string(
                program_id,
                "arch.txt",
                || read_program_architecture(file_type, &file, &patch_manager, &*content_provider),
            ));
            (patches, architecture)
        };
        let rom_fs_updatable = loader.is_rom_fs_updatable();
        let add_ons = get_game_list_cached_string(program_id, "pv.txt", || {
            format_patch_name_versions(&patches, file_type, rom_fs_updatable)
        });

        Some(GameMetadata {
            title,
            icon,
            program_id,
            developer,
            version,
            add_ons,
            architecture,
        })
    }
}

/// What [`MetadataReader::read`] recovers from a container.
struct GameMetadata {
    title: Option<String>,
    icon: Option<Vec<u8>>,
    program_id: u64,
    developer: String,
    version: String,
    add_ons: String,
    architecture: String,
}

fn architecture_label(is_64_bit: Option<bool>) -> String {
    match is_64_bit {
        Some(true) => "aarch64".to_owned(),
        Some(false) => "aarch32".to_owned(),
        None => "Unknown".to_owned(),
    }
}

fn normalize_architecture_label(label: String) -> String {
    match label.as_str() {
        "AArch64" | "aarch64" => "aarch64".to_owned(),
        "AArch32" | "aarch32" => "aarch32".to_owned(),
        _ => label,
    }
}

/// Read only the NPDM bit which selects the guest instruction set.
///
/// `pv.txt` remains Eden's patch-version cache verbatim. Architecture has its
/// own `arch.txt` cache so a warm game-list scan only reads a few bytes and
/// Eden can continue consuming `pv.txt` unchanged.
fn read_program_architecture(
    file_type: FileType,
    file: &VirtualFile,
    patch_manager: &PatchManager<'_>,
    content_provider: &dyn ContentProvider,
) -> String {
    let is_64_bit = match file_type {
        // Both standalone loaders use ProgramMetadata::GetDefault(), whose
        // upstream instruction-set bit is 64-bit.
        FileType::NRO | FileType::NSO => Some(true),
        FileType::KIP => {
            let kip = KIP::new(file);
            (kip.get_status() == FsResultStatus::Success).then(|| kip.is_64_bit())
        }
        FileType::DeconstructedRomDirectory => file
            .get_containing_directory()
            .and_then(|exefs| architecture_from_exefs(exefs, patch_manager)),
        FileType::NCA | FileType::NSP | FileType::XCI | FileType::NAX => content_provider
            .get_entry_raw(
                patch_manager.get_title_id(),
                ruzu_core::file_sys::nca_metadata::ContentRecordType::Program,
            )
            .or_else(|| {
                content_provider.get_entry_raw(
                    get_update_title_id(patch_manager.get_title_id()),
                    ruzu_core::file_sys::nca_metadata::ContentRecordType::Program,
                )
            })
            .and_then(|program| NCA::new(program, None).get_exefs())
            .and_then(|exefs| architecture_from_exefs(exefs, patch_manager)),
        FileType::Error | FileType::Unknown => None,
    };
    architecture_label(is_64_bit)
}

fn architecture_from_exefs(
    exefs: ruzu_core::file_sys::vfs::vfs_types::VirtualDir,
    patch_manager: &PatchManager<'_>,
) -> Option<bool> {
    let exefs = patch_manager.patch_exefs(exefs);
    let npdm = exefs.get_file("main.npdm")?;
    let mut metadata = ProgramMetadata::new();
    (metadata.load(npdm) == FsResultStatus::Success).then(|| metadata.is_64_bit_program())
}

/// Eden `FormatPatchNameVersions` used by the game-list Add-ons column.
fn format_patch_name_versions(
    patches: &[Patch],
    file_type: FileType,
    rom_fs_updatable: bool,
) -> String {
    patches
        .iter()
        .filter(|patch| rom_fs_updatable || patch.name != "Update")
        .map(|patch| {
            let name = if patch.enabled {
                patch.name.clone()
            } else {
                format!("[D] {}", patch.name)
            };
            if patch.version.is_empty() {
                name
            } else {
                let version = if patch.name == "Update" && patch.version == "PACKED" {
                    ruzu_core::loader::loader::get_file_type_string(file_type).to_owned()
                } else {
                    patch.version.clone()
                };
                format!("{name} ({version})")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Eden `GetGameListCachedObject(QString)` specialization.
fn get_game_list_cached_string(
    program_id: u64,
    extension: &str,
    generator: impl FnOnce() -> String,
) -> String {
    if !uisettings::with(|values| *values.cache_game_list.get_value()) || program_id == 0 {
        return generator();
    }

    let path = common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::CacheDir)
        .join("game_list")
        .join(format!("{program_id:016X}.{extension}"));
    if let Ok(bytes) = std::fs::read(&path) {
        return String::from_utf8_lossy(&bytes).into_owned();
    }

    let value = generator();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(error) = std::fs::write(&path, value.as_bytes()) {
        log::error!(
            "Failed to write game-list cache {}: {error}",
            path.display()
        );
    }
    value
}

/// Upstream `ContainsAllWords` plus the title-id branch in
/// `GameList::OnTextChanged`.
fn filter_fields_match(name: &str, path: &str, program_id: u64, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }

    let filename = Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let haystack = format!("{filename} {name}").to_lowercase();
    let contains_all_words = query.split_whitespace().all(|word| haystack.contains(word));
    let title_id = format!("{program_id:016x}");
    contains_all_words || title_id.contains(query)
}

fn game_matches_filter(game: &GameEntry, query: &str) -> bool {
    filter_fields_match(&game.name(), &game.path(), game.program_id(), query)
}

/// Human-readable byte size (KiB / MiB / GiB), matching yuzu's display style.
fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_menu_uses_traditional_nested_submenus() {
        assert_eq!(context_menu_flags(), gtk::PopoverMenuFlags::NESTED);
    }
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn make_temp_dir() -> PathBuf {
        let counter = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("ruzu-game-list-{}-{counter}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn missing_game_directory_uses_the_bad_folder_icon() {
        // The two embedded assets must stay distinguishable for this test.
        assert_ne!(FOLDER_ICON_PNG, BAD_FOLDER_ICON_PNG);

        let existing = make_temp_dir();
        assert_eq!(folder_icon_png(existing.to_str().unwrap()), FOLDER_ICON_PNG);

        let missing = existing.join("gone");
        assert!(!missing.exists());
        assert_eq!(
            folder_icon_png(missing.to_str().unwrap()),
            BAD_FOLDER_ICON_PNG
        );

        // A directory that disappears after being registered switches icons.
        std::fs::remove_dir_all(&existing).unwrap();
        assert_eq!(
            folder_icon_png(existing.to_str().unwrap()),
            BAD_FOLDER_ICON_PNG
        );
    }

    #[test]
    fn human_size_matches_yuzu_formatting() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(21_599_437), "20.6 MiB");
        assert_eq!(human_size(7_301_444_403), "6.8 GiB");
    }

    #[test]
    fn filter_matches_all_words_or_title_id_like_upstream() {
        let path = "/games/Sample Adventure [0100123456789ABC].nsp";
        assert!(filter_fields_match(
            "Sample Adventure",
            path,
            0x0100_1234_5678_9ABC,
            "adventure sample"
        ));
        assert!(filter_fields_match(
            "Sample Adventure",
            path,
            0x0100_1234_5678_9ABC,
            "56789abc"
        ));
        assert!(!filter_fields_match(
            "Sample Adventure",
            path,
            0x0100_1234_5678_9ABC,
            "missing sample"
        ));
    }

    #[test]
    fn supported_extensions_cover_every_switch_container() {
        // Mirrors `GameList::supported_file_extensions`; dropping one silently
        // hides a whole class of dumps.
        for ext in ["nsp", "xci", "nca", "nro", "nso", "kip"] {
            assert!(SUPPORTED_EXTENSIONS.contains(&ext), "{ext} missing");
        }
    }

    #[test]
    fn newly_added_directories_scan_subfolders_by_default() {
        assert!(NEW_DIRECTORY_DEEP_SCAN);
    }

    #[test]
    fn scan_result_discards_obsolete_refresh_generations() {
        let (sender, receiver) = mpsc::channel();
        for generation in [1, 3, 2] {
            sender
                .send(GameListScanResult {
                    generation,
                    directories: Vec::new(),
                    directory_to_select: None,
                })
                .unwrap();
        }

        let result = take_current_scan_result(&receiver, 3).unwrap();
        assert_eq!(result.generation, 3);
        assert!(take_current_scan_result(&receiver, 3).is_none());
    }

    #[test]
    fn favorites_clone_first_matching_games_in_configured_order() {
        let first = GameEntry::new_game(
            "First copy",
            "",
            "1.0.0",
            "NSP",
            "aarch64",
            "1 B",
            "",
            "",
            "/games/first.nsp",
            None,
            1,
        );
        let duplicate = GameEntry::new_game(
            "Duplicate",
            "",
            "1.0.0",
            "NSP",
            "aarch64",
            "1 B",
            "",
            "",
            "/games/duplicate.nsp",
            None,
            1,
        );
        let second = GameEntry::new_game(
            "Second",
            "",
            "1.0.0",
            "XCI",
            "aarch64",
            "2 B",
            "",
            "",
            "/games/second.xci",
            None,
            2,
        );
        let directories = vec![vec![first.clone(), second], vec![duplicate]];

        let favorites = favorite_entries(&[2, 1, 3], &directories);

        assert_eq!(favorites.len(), 2);
        assert_eq!(favorites[0].program_id(), 2);
        assert_eq!(favorites[0].architecture(), "aarch64");
        assert_eq!(favorites[1].name(), "First copy");
        assert_ne!(favorites[1], first, "favorite rows must be cloned");
    }

    #[test]
    fn favorites_root_is_expandable_but_not_a_directory() {
        let children = gio::ListStore::new::<GameEntry>();
        let favorites = GameEntry::new_favorites(children);

        assert!(favorites.is_folder());
        assert!(favorites.is_favorites());
        assert!(favorites.path().is_empty());
    }

    #[test]
    fn removed_favorites_root_is_not_treated_as_user_visible() {
        let store = gio::ListStore::new::<GameEntry>();
        let favorites = GameEntry::new_favorites(gio::ListStore::new::<GameEntry>());

        store.append(&favorites);
        assert!(store_contains_entry(&store, &favorites));
        store.remove(0);
        assert!(!store_contains_entry(&store, &favorites));
    }

    #[test]
    fn keyboard_navigation_matches_controller_actions() {
        assert_eq!(
            navigation_key_for_gdk(gdk::Key::Return),
            Some(NavigationKey::Enter)
        );
        assert_eq!(
            navigation_key_for_gdk(gdk::Key::KP_Enter),
            Some(NavigationKey::Enter)
        );
        assert_eq!(
            navigation_key_for_gdk(gdk::Key::Down),
            Some(NavigationKey::Down)
        );
        assert_eq!(
            navigation_key_for_gdk(gdk::Key::Left),
            Some(NavigationKey::Left)
        );
        assert_eq!(
            navigation_key_for_gdk(gdk::Key::Right),
            Some(NavigationKey::Right)
        );
        assert_eq!(
            navigation_key_for_gdk(gdk::Key::Up),
            Some(NavigationKey::Up)
        );
        assert_eq!(navigation_key_for_gdk(gdk::Key::F1), None);
    }

    #[test]
    fn deep_scan_matches_upstream_unbounded_recursion() {
        let root = make_temp_dir();
        let nested = root.join("one/two/three/four/five/six");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join("direct.nsp"), []).unwrap();
        std::fs::write(nested.join("nested.nro"), []).unwrap();

        let mut shallow = Vec::new();
        collect_candidates(&root, false, &mut shallow);
        assert_eq!(shallow.len(), 1);
        assert_eq!(shallow[0].path, root.join("direct.nsp"));

        let mut recursive = Vec::new();
        collect_candidates(&root, true, &mut recursive);
        assert_eq!(recursive.len(), 2);
        assert!(recursive
            .iter()
            .any(|game| game.path == root.join("direct.nsp")));
        assert!(recursive
            .iter()
            .any(|game| game.path == nested.join("nested.nro")));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn collect_candidates_accepts_extracted_nca_main() {
        let root = make_temp_dir();
        let extracted = root.join("extracted_program");
        std::fs::create_dir_all(&extracted).unwrap();
        std::fs::write(extracted.join("main"), []).unwrap();

        let mut games = Vec::new();
        collect_candidates(&root, true, &mut games);

        assert_eq!(games.len(), 1);
        assert_eq!(games[0].path, extracted.join("main"));
        assert_eq!(games[0].name, "extracted_program");
        assert_eq!(games[0].kind, "NCA");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sole_directory_is_selected_after_reload() {
        let directory = ScanDirectory {
            name: String::from(r"D:\Games\Switch"),
            path: String::from(r"D:\Games\Switch"),
            deep_scan: false,
            is_built_in: false,
        };

        assert_eq!(
            preferred_directory_path(None, std::slice::from_ref(&directory)),
            Some(directory.path.clone())
        );
        assert_eq!(
            preferred_directory_path(Some(&directory.path), std::slice::from_ref(&directory)),
            Some(directory.path)
        );
        assert_eq!(preferred_directory_path(Some("removed"), &[]), None);
    }

    #[test]
    fn packaged_free_games_are_named_and_marked_read_only() {
        let directory = ScanDirectory::packaged_free_games(PathBuf::from("/opt/freegames"));

        assert_eq!(directory.name, "Free Games");
        assert_eq!(directory.path, "/opt/freegames");
        assert!(directory.deep_scan);
        assert!(directory.is_built_in);

        let entry = GameEntry::new_folder(
            &directory.name,
            &directory.path,
            directory.deep_scan,
            directory.is_built_in,
            gio::ListStore::new::<GameEntry>(),
        );
        assert!(entry.is_folder());
        assert!(entry.is_built_in());
        assert!(!entry.is_favorites());
        assert_eq!(entry.name(), "Free Games");
    }

    #[test]
    fn directory_context_move_preserves_non_filesystem_entries() {
        let mut directories = vec![
            GameDir {
                path: "SDMC".to_string(),
                deep_scan: false,
                expanded: true,
            },
            GameDir {
                path: "/games/one".to_string(),
                deep_scan: false,
                expanded: true,
            },
            GameDir {
                path: "UserNAND".to_string(),
                deep_scan: false,
                expanded: true,
            },
            GameDir {
                path: "/games/two".to_string(),
                deep_scan: true,
                expanded: false,
            },
        ];

        assert!(move_filesystem_directory(
            &mut directories,
            "/games/two",
            -1
        ));
        assert_eq!(directories[0].path, "SDMC");
        assert_eq!(directories[1].path, "/games/two");
        assert_eq!(directories[2].path, "UserNAND");
        assert_eq!(directories[3].path, "/games/one");
        assert!(!move_filesystem_directory(
            &mut directories,
            "/games/two",
            -1
        ));
        assert!(!move_filesystem_directory(&mut directories, "/missing", 1));
    }

    #[test]
    fn patch_versions_match_eden_game_list_format() {
        let patches = vec![
            Patch {
                enabled: true,
                name: "Update".to_owned(),
                version: "1.2.3".to_owned(),
                patch_type: ruzu_core::file_sys::patch_manager::PatchType::Update,
                program_id: 1,
                title_id: 1,
                source: ruzu_core::file_sys::patch_manager::PatchSource::Unknown,
                location: String::new(),
                numeric_version: 0,
            },
            Patch {
                enabled: false,
                name: "Example Mod".to_owned(),
                version: String::new(),
                patch_type: ruzu_core::file_sys::patch_manager::PatchType::Mod,
                program_id: 1,
                title_id: 1,
                source: ruzu_core::file_sys::patch_manager::PatchSource::Unknown,
                location: String::new(),
                numeric_version: 0,
            },
        ];
        assert_eq!(
            format_patch_name_versions(&patches, FileType::NSP, true),
            "Update (1.2.3)\n[D] Example Mod"
        );
        assert_eq!(
            format_patch_name_versions(&patches, FileType::NRO, false),
            "[D] Example Mod"
        );
    }

    #[test]
    fn architecture_column_uses_switch_instruction_set_names() {
        assert_eq!(architecture_label(Some(true)), "aarch64");
        assert_eq!(architecture_label(Some(false)), "aarch32");
        assert_eq!(architecture_label(None), "Unknown");
        assert_eq!(
            normalize_architecture_label("AArch64".to_owned()),
            "aarch64"
        );
        assert_eq!(
            normalize_architecture_label("AArch32".to_owned()),
            "aarch32"
        );
    }
}
