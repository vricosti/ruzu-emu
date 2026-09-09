// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/eden/src/yuzu/configuration/configure_hotkeys.cpp`
// (`ConfigureHotkeys`), whose widget tree lives in `configure_hotkeys.ui`.
//
// Upstream shows a `QTreeView` over a `QStandardItemModel` with three columns
// (Action / Hotkey / Controller Hotkey), grouped by context ("Main Window"),
// plus a hint label and the Clear All / Restore Defaults buttons. Double-clicking
// a binding opens `SequenceDialog` to record a new one.
//
// The default bindings come from `UISettings::default_hotkeys` in
// `crate::uisettings`, matching upstream ownership.

use std::rc::Rc;
use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};
use hid_core::hid_core::{HIDCore, EmulatedControllerHandle};
use hid_core::hid_types::{NpadButton, NpadIdType};

use gtk::prelude::*;

use super::configure_dialog::Page;

/// The context every default hotkey belongs to — upstream's group row.
const CONTEXT: &str = "Main Window";

/// Column widths, roughly matching the Qt tree's resize-to-contents result.
const ACTION_COLUMN_WIDTH: i32 = 420;
const HOTKEY_COLUMN_WIDTH: i32 = 150;

/// Build the Hotkeys tab — upstream `ConfigureHotkeys`.
pub fn page(hid: &Arc<parking_lot::Mutex<HIDCore>>) -> (Page, Rc<ControllerCapture>) {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.set_margin_top(10);
    column.set_margin_bottom(10);
    column.set_margin_start(10);
    column.set_margin_end(10);

    // Hint label + Clear All / Restore Defaults, on one row like `configure_hotkeys.ui`.
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let hint = gtk::Label::new(Some("Double-click on a binding to change it."));
    hint.set_xalign(0.0);
    hint.set_hexpand(true);
    let clear_all = gtk::Button::with_label("Clear All");
    let restore_defaults = gtk::Button::with_label("Restore Defaults");
    header.append(&hint);
    header.append(&clear_all);
    header.append(&restore_defaults);
    column.append(&header);

    // --- The binding tree -------------------------------------------------
    // GTK4's `ColumnView` is the closest analogue of `QTreeView` + model; a
    // `TreeListModel` supplies the one expandable "Main Window" group row that
    // upstream's `QStandardItemModel` produces.
    let store = gtk::gio::ListStore::new::<HotkeyRow>();
    store.append(&HotkeyRow::group(CONTEXT));

    let configured = crate::uisettings::with(|values| values.shortcuts.clone());
    let rows = Rc::new(
        configured
            .iter()
            .map(|shortcut| {
                HotkeyRow::binding(
                    &shortcut.name,
                    &shortcut.keyseq,
                    &shortcut.controller_keyseq,
                )
            })
            .collect::<Vec<_>>(),
    );
    let capture = Rc::new(ControllerCapture {
        controller: hid.lock().get_emulated_controller(NpadIdType::Player1),
        rows: Rc::clone(&rows), active: RefCell::new(None), timer: RefCell::new(None),
    });
    let child_store = gtk::gio::ListStore::new::<HotkeyRow>();
    for row in rows.iter() {
        child_store.append(row);
    }

    let tree = gtk::TreeListModel::new(store.clone(), false, true, move |item| {
        let row = item.downcast_ref::<HotkeyRow>()?;
        if !row.is_group() {
            return None;
        }
        Some(child_store.clone().upcast())
    });

    let selection = gtk::SingleSelection::new(Some(tree));
    let view = gtk::ColumnView::new(Some(selection));
    view.set_vexpand(true);

    view.append_column(&expander_column(
        "Action",
        ACTION_COLUMN_WIDTH,
        Rc::clone(&rows),
        |row| row.action(),
        Rc::clone(&capture),
    ));
    view.append_column(&hotkey_column(HOTKEY_COLUMN_WIDTH, Rc::clone(&rows), Rc::clone(&capture)));
    view.append_column(&controller_hotkey_column(
        "Controller Hotkey",
        HOTKEY_COLUMN_WIDTH,
        |row| row.controller_hotkey(),
        Rc::clone(&capture),
    ));

    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&view)
        .build();
    column.append(&scroller);

    clear_all.connect_clicked({
        let rows = Rc::clone(&rows);
        let capture = Rc::clone(&capture);
        move |_| {
            capture.cancel();
            for row in rows.iter() {
                row.set_hotkey("");
                row.set_controller_hotkey("");
            }
        }
    });
    restore_defaults.connect_clicked({
        let rows = Rc::clone(&rows);
        let capture = Rc::clone(&capture);
        move |_| {
            capture.cancel();
            for (row, default) in rows.iter().zip(crate::uisettings::DEFAULT_HOTKEYS) {
                row.set_hotkey(default.keyseq);
                row.set_controller_hotkey(default.controller_keyseq);
            }
        }
    });

    column.connect_unmap({
        let capture = Rc::clone(&capture);
        move |_| capture.cancel()
    });
    let apply_capture = Rc::clone(&capture);
    let page = Page::new("Hotkeys", column, move || {
        apply_capture.cancel();
        crate::uisettings::with_mut(|values| {
            for (shortcut, row) in values.shortcuts.iter_mut().zip(rows.iter()) {
                shortcut.keyseq = row.hotkey();
                shortcut.controller_keyseq = row.controller_hotkey();
            }
        });
    });
    (page, capture)
}

struct PendingControllerCapture {
    row: HotkeyRow,
    previous: String,
    source: gtk::glib::WeakRef<gtk::Widget>,
    started: Instant,
    buttons: NpadButton,
    home: bool,
    screenshot: bool,
}

/// ConfigureHotkeys::ConfigureController/SetPollingResult. Owned by the page
/// and parent dialog so capture ends before ConfigureInput's close cleanup.
pub struct ControllerCapture {
    controller: EmulatedControllerHandle,
    rows: Rc<Vec<HotkeyRow>>,
    active: RefCell<Option<PendingControllerCapture>>,
    timer: RefCell<Option<gtk::glib::SourceId>>,
}

impl ControllerCapture {
    fn start(self: &Rc<Self>, source: &gtk::Widget, row: &HotkeyRow) {
        if self.active.borrow().is_some() { return; }
        *self.active.borrow_mut() = Some(PendingControllerCapture {
            row: row.clone(), previous: row.controller_hotkey(), source: source.downgrade(),
            started: Instant::now(), buttons: NpadButton::empty(), home: false, screenshot: false,
        });
        row.set_controller_hotkey(&crate::i18n::tr("[waiting]"));
        self.controller.lock().disable_configuration();
        let weak = Rc::downgrade(self);
        *self.timer.borrow_mut() = Some(gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
            let Some(capture) = weak.upgrade() else { return gtk::glib::ControlFlow::Break; };
            if capture.poll() {
                gtk::glib::ControlFlow::Continue
            } else {
                capture.timer.borrow_mut().take();
                capture.finish(false);
                gtk::glib::ControlFlow::Break
            }
        }));
    }

    fn poll(&self) -> bool {
        let reader = self.controller.lock().button_state_reader();
        let (npad, home, screenshot) = reader.read();
        let mut active = self.active.borrow_mut();
        let Some(active) = active.as_mut() else { return false; };
        active.buttons |= npad.raw;
        active.home |= home.raw != 0;
        active.screenshot |= screenshot.raw != 0;
        if !active.buttons.is_empty() || active.home || active.screenshot {
            active.row.set_controller_hotkey(&format!("{}...", get_button_combination_name(active.buttons, active.home, active.screenshot)));
        }
        active.started.elapsed() < Duration::from_millis(2500)
    }

    fn finish(&self, cancel: bool) {
        let Some(active) = self.active.borrow_mut().take() else { return; };
        if cancel || (active.buttons.is_empty() && !active.home && !active.screenshot) {
            active.row.set_controller_hotkey(&active.previous);
        } else {
            let sequence = get_button_combination_name(active.buttons, active.home, active.screenshot);
            if let Some(conflict) = self.rows.iter().find(|row| row.controller_hotkey() == sequence) {
                active.row.set_controller_hotkey(&active.previous);
                let parent = active.source.upgrade().and_then(|source| source.root()).and_downcast::<gtk::Window>();
                crate::gtk_compat::show_warning(parent.as_ref(), "Conflicting Key Sequence",
                    &format!("{} {}", crate::i18n::tr("The entered key sequence is already assigned to:"), conflict.action()));
            } else {
                active.row.set_controller_hotkey(&sequence);
            }
        }
        self.controller.lock().enable_configuration();
    }

    pub fn cancel(&self) {
        if let Some(timer) = self.timer.borrow_mut().take() { timer.remove(); }
        self.finish(true);
    }
}

impl Drop for ControllerCapture {
    fn drop(&mut self) { self.cancel(); }
}

fn get_button_combination_name(buttons: NpadButton, home: bool, screenshot: bool) -> String {
    let mut names = Vec::new();
    if home { names.push("Home"); }
    if screenshot { names.push("Screenshot"); }
    for (mask, name) in [
        (NpadButton::A, "A"), (NpadButton::B, "B"), (NpadButton::X, "X"), (NpadButton::Y, "Y"),
        (NpadButton::L | NpadButton::LEFT_SL | NpadButton::RIGHT_SL, "L"),
        (NpadButton::R | NpadButton::LEFT_SR | NpadButton::RIGHT_SR, "R"),
        (NpadButton::ZL, "ZL"), (NpadButton::ZR, "ZR"),
        (NpadButton::LEFT, "Dpad_Left"), (NpadButton::RIGHT, "Dpad_Right"),
        (NpadButton::UP, "Dpad_Up"), (NpadButton::DOWN, "Dpad_Down"),
        (NpadButton::STICK_L, "Left_Stick"), (NpadButton::STICK_R, "Right_Stick"),
        (NpadButton::MINUS, "Minus"), (NpadButton::PLUS, "Plus"),
    ] { if buttons.intersects(mask) { names.push(name); } }
    if names.is_empty() { crate::i18n::tr("Invalid") } else { names.join("+") }
}

/// RestoreHotkey/RestoreControllerHotkey, including upstream's same-value
/// exception to conflict detection. A conflict leaves the previous value intact.
fn restore_hotkey(row: &HotkeyRow, rows: &[HotkeyRow], controller: bool) -> Result<(), String> {
    let Some(default) = crate::uisettings::DEFAULT_HOTKEYS.iter().find(|key| key.name == row.action()) else {
        return Ok(());
    };
    let value = if controller { default.controller_keyseq } else { default.keyseq };
    let current = if controller { row.controller_hotkey() } else { row.hotkey() };
    let same = |left: &str, right: &str| if controller { left == right } else { same_key_sequence(left, right) };
    if !same(&current, value) {
        if let Some(conflict) = rows.iter().find(|candidate| {
            let candidate = if controller { candidate.controller_hotkey() } else { candidate.hotkey() };
            same(&candidate, value)
        }) { return Err(conflict.action()); }
    }
    if controller { row.set_controller_hotkey(value); } else { row.set_hotkey(value); }
    Ok(())
}

/// PopupContextMenu: Action and Hotkey columns edit the keyboard sequence;
/// only the Controller Hotkey column edits the controller sequence.
fn install_context_menu(
    widget: &impl IsA<gtk::Widget>, item: &gtk::ListItem,
    rows: Rc<Vec<HotkeyRow>>, capture: Rc<ControllerCapture>, controller: bool,
) {
    let click = gtk::GestureClick::new();
    click.set_button(3);
    let widget = widget.clone().upcast::<gtk::Widget>();
    let source = widget.downgrade();
    let item = item.downgrade();
    click.connect_pressed(move |gesture, _, x, y| {
        let (Some(source), Some(item)) = (source.upgrade(), item.upgrade()) else { return; };
        let Some(row) = item.item().and_downcast::<gtk::TreeListRow>()
            .and_then(|item| item.item()).and_downcast::<HotkeyRow>() else { return; };
        if row.is_group() { return; }
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let popover = gtk::Popover::new();
        popover.set_parent(&source);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        for restore in [true, false] {
            let button = gtk::Button::with_label(&crate::i18n::tr(if restore { "Restore Default" } else { "Clear" }));
            button.add_css_class("flat");
            button.connect_clicked({
                let row = row.clone();
                let rows = Rc::clone(&rows);
                let capture = Rc::clone(&capture);
                let popover = popover.downgrade();
                let source = source.downgrade();
                move |_| {
                    capture.cancel();
                    let result = if restore { restore_hotkey(&row, &rows, controller) } else {
                        if controller { row.set_controller_hotkey(""); } else { row.set_hotkey(""); }
                        Ok(())
                    };
                    if let Some(popover) = popover.upgrade() { popover.popdown(); }
                    if let Err(action) = result {
                        let parent = source.upgrade().and_then(|source| source.root()).and_downcast::<gtk::Window>();
                        crate::gtk_compat::show_warning(parent.as_ref(),
                            if controller { "Conflicting Button Sequence" } else { "Conflicting Key Sequence" },
                            &crate::i18n::tr(if controller {
                                "The default button sequence is already assigned to: %1"
                            } else { "The default key sequence is already assigned to: %1" })
                                .replace("%1", &crate::i18n::tr(&action)));
                    }
                }
            });
            content.append(&button);
        }
        popover.set_child(Some(&content));
        popover.connect_closed(|popover| popover.unparent());
        popover.popup();
    });
    widget.add_controller(click);
}

/// Editable keyboard-binding column. Upstream routes a double-click in either
/// the action or keyboard column to the keyboard `SequenceDialog`; the binding
/// cell is the GTK interaction target advertised by the hint above the table.
fn hotkey_column(width: i32, rows: Rc<Vec<HotkeyRow>>, capture: Rc<ControllerCapture>) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap().clone();
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        install_context_menu(&label, &item, Rc::clone(&rows), Rc::clone(&capture), false);

        let click = gtk::GestureClick::new();
        click.set_button(1);
        click.connect_pressed({
            let list_item = item.clone();
            let label = label.clone();
            let rows = Rc::clone(&rows);
            move |_, press_count, _, _| {
                if press_count != 2 {
                    return;
                }
                let Some(row) = list_item
                    .item()
                    .and_downcast::<gtk::TreeListRow>()
                    .and_then(|tree_row| tree_row.item())
                    .and_downcast::<HotkeyRow>()
                else {
                    return;
                };
                if row.is_group() {
                    return;
                }
                configure_keyboard_hotkey(&label, &row, Rc::clone(&rows));
            }
        });
        label.add_controller(click);
        item.set_child(Some(&label));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(label) = item.child().and_downcast::<gtk::Label>() else {
            return;
        };
        let Some(row) = item
            .item()
            .and_downcast::<gtk::TreeListRow>()
            .and_then(|tree_row| tree_row.item())
            .and_downcast::<HotkeyRow>()
        else {
            return;
        };
        label.set_text(&row.hotkey());
        row.register_hotkey_label(&label);
    });
    factory.connect_unbind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(label) = item.child().and_downcast::<gtk::Label>() else {
            return;
        };
        if let Some(row) = item
            .item()
            .and_downcast::<gtk::TreeListRow>()
            .and_then(|tree_row| tree_row.item())
            .and_downcast::<HotkeyRow>()
        {
            row.unregister_hotkey_label(&label);
        }
    });

    let column = gtk::ColumnViewColumn::new(Some("Hotkey"), Some(factory));
    column.set_fixed_width(width);
    column
}

fn configure_keyboard_hotkey(
    source: &impl IsA<gtk::Widget>,
    row: &HotkeyRow,
    rows: Rc<Vec<HotkeyRow>>,
) {
    let row = row.clone();
    let source = source.clone().upcast::<gtk::Widget>();
    let source_for_response = source.clone();
    crate::util::sequence_dialog::present(&source, move |sequence| {
        let conflict = rows.iter().find(|candidate| {
            candidate.action() != row.action()
                && !candidate.hotkey().is_empty()
                && same_key_sequence(&candidate.hotkey(), &sequence)
        });
        if let Some(conflict) = conflict {
            let detail = format!(
                "{} {}",
                crate::i18n::tr("The entered key sequence is already assigned to:"),
                conflict.action()
            );
            let parent = source_for_response.root().and_downcast::<gtk::Window>();
            crate::gtk_compat::show_warning(parent.as_ref(), "Conflicting Key Sequence", &detail);
            return;
        }
        row.set_hotkey(&sequence);
    });
}

fn same_key_sequence(left: &str, right: &str) -> bool {
    left.chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .eq(right
            .chars()
            .filter(|character| !character.is_whitespace())
            .flat_map(char::to_lowercase))
}

/// Column whose cells carry the tree expander — the first column, as in Qt.
fn expander_column(
    title: &str,
    width: i32,
    rows: Rc<Vec<HotkeyRow>>,
    get: fn(&HotkeyRow) -> String,
    capture: Rc<ControllerCapture>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let list_item = item.downcast_ref::<gtk::ListItem>().unwrap().clone();
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        let expander = gtk::TreeExpander::new();
        expander.set_child(Some(&label));
        install_context_menu(&expander, &list_item, Rc::clone(&rows), Rc::clone(&capture), false);

        let click = gtk::GestureClick::new();
        click.set_button(1);
        click.connect_pressed({
            let label = label.clone();
            let rows = Rc::clone(&rows);
            move |_, press_count, _, _| {
                if press_count != 2 {
                    return;
                }
                let Some(row) = list_item
                    .item()
                    .and_downcast::<gtk::TreeListRow>()
                    .and_then(|tree_row| tree_row.item())
                    .and_downcast::<HotkeyRow>()
                else {
                    return;
                };
                if !row.is_group() {
                    configure_keyboard_hotkey(&label, &row, Rc::clone(&rows));
                }
            }
        });
        expander.add_controller(click);
        item.downcast_ref::<gtk::ListItem>()
            .unwrap()
            .set_child(Some(&expander));
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(expander) = item.child().and_downcast::<gtk::TreeExpander>() else {
            return;
        };
        let Some(tree_row) = item.item().and_downcast::<gtk::TreeListRow>() else {
            return;
        };
        expander.set_list_row(Some(&tree_row));
        if let (Some(label), Some(row)) = (
            expander.child().and_downcast::<gtk::Label>(),
            tree_row.item().and_downcast::<HotkeyRow>(),
        ) {
            label.set_text(&get(&row));
        }
    });

    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_fixed_width(width);
    column
}

/// Controller-binding column, routed to ConfigureController on double-click.
fn controller_hotkey_column(title: &str, width: i32, get: fn(&HotkeyRow) -> String, capture: Rc<ControllerCapture>) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        install_context_menu(&label, item.downcast_ref::<gtk::ListItem>().unwrap(), Rc::clone(&capture.rows), Rc::clone(&capture), true);
        let click = gtk::GestureClick::new();
        click.set_button(1);
        click.connect_pressed({
            let capture = Rc::clone(&capture);
            let item = item.downcast_ref::<gtk::ListItem>().unwrap().downgrade();
            let label = label.downgrade();
            move |_, count, _, _| {
                if count != 2 { return; }
                let (Some(item), Some(label)) = (item.upgrade(), label.upgrade()) else { return; };
                if let Some(row) = item.item().and_downcast::<gtk::TreeListRow>()
                    .and_then(|item| item.item()).and_downcast::<HotkeyRow>() {
                    if !row.is_group() { capture.start(label.upcast_ref(), &row); }
                }
            }
        });
        label.add_controller(click);
        item.downcast_ref::<gtk::ListItem>()
            .unwrap()
            .set_child(Some(&label));
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(label) = item.child().and_downcast::<gtk::Label>() else {
            return;
        };
        let text = item
            .item()
            .and_downcast::<gtk::TreeListRow>()
            .and_then(|r| r.item())
            .and_downcast::<HotkeyRow>()
            .map(|row| {
                row.register_controller_label(&label);
                get(&row)
            })
            .unwrap_or_default();
        label.set_text(&text);
    });
    factory.connect_unbind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(label) = item.child().and_downcast::<gtk::Label>() else {
            return;
        };
        if let Some(row) = item
            .item()
            .and_downcast::<gtk::TreeListRow>()
            .and_then(|tree_row| tree_row.item())
            .and_downcast::<HotkeyRow>()
        {
            row.unregister_controller_label(&label);
        }
    });

    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_fixed_width(width);
    column
}

// A `GObject` row so the list model can hold it. Upstream uses
// `QStandardItem`s carrying the same three strings.
mod imp {
    use std::cell::RefCell;

    use gtk::glib;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub struct HotkeyRow {
        pub action: RefCell<String>,
        pub hotkey: RefCell<String>,
        pub controller_hotkey: RefCell<String>,
        pub is_group: RefCell<bool>,
        pub hotkey_labels: RefCell<Vec<glib::WeakRef<gtk::Label>>>,
        pub controller_labels: RefCell<Vec<glib::WeakRef<gtk::Label>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HotkeyRow {
        const NAME: &'static str = "RuzuHotkeyRow";
        type Type = super::HotkeyRow;
    }

    impl ObjectImpl for HotkeyRow {}
}

gtk::glib::wrapper! {
    /// One row of the hotkey tree: either the context group or a binding.
    pub struct HotkeyRow(ObjectSubclass<imp::HotkeyRow>);
}

impl HotkeyRow {
    /// The expandable context row ("Main Window").
    fn group(context: &str) -> Self {
        let this: Self = gtk::glib::Object::new();
        let imp = gtk::subclass::prelude::ObjectSubclassIsExt::imp(&this);
        *imp.action.borrow_mut() = context.to_string();
        *imp.is_group.borrow_mut() = true;
        this
    }

    /// A binding row.
    fn binding(action: &str, hotkey: &str, controller_hotkey: &str) -> Self {
        let this: Self = gtk::glib::Object::new();
        let imp = gtk::subclass::prelude::ObjectSubclassIsExt::imp(&this);
        *imp.action.borrow_mut() = action.to_string();
        *imp.hotkey.borrow_mut() = hotkey.to_string();
        *imp.controller_hotkey.borrow_mut() = controller_hotkey.to_string();
        this
    }

    fn is_group(&self) -> bool {
        *gtk::subclass::prelude::ObjectSubclassIsExt::imp(self)
            .is_group
            .borrow()
    }

    fn action(&self) -> String {
        gtk::subclass::prelude::ObjectSubclassIsExt::imp(self)
            .action
            .borrow()
            .clone()
    }

    fn hotkey(&self) -> String {
        gtk::subclass::prelude::ObjectSubclassIsExt::imp(self)
            .hotkey
            .borrow()
            .clone()
    }

    fn set_hotkey(&self, hotkey: &str) {
        let imp = gtk::subclass::prelude::ObjectSubclassIsExt::imp(self);
        *imp.hotkey.borrow_mut() = hotkey.to_owned();
        update_labels(&mut imp.hotkey_labels.borrow_mut(), hotkey);
    }

    fn register_hotkey_label(&self, label: &gtk::Label) {
        let imp = gtk::subclass::prelude::ObjectSubclassIsExt::imp(self);
        register_label(&mut imp.hotkey_labels.borrow_mut(), label);
    }

    fn unregister_hotkey_label(&self, label: &gtk::Label) {
        let imp = gtk::subclass::prelude::ObjectSubclassIsExt::imp(self);
        unregister_label(&mut imp.hotkey_labels.borrow_mut(), label);
    }

    fn controller_hotkey(&self) -> String {
        gtk::subclass::prelude::ObjectSubclassIsExt::imp(self)
            .controller_hotkey
            .borrow()
            .clone()
    }

    fn set_controller_hotkey(&self, hotkey: &str) {
        let imp = gtk::subclass::prelude::ObjectSubclassIsExt::imp(self);
        *imp.controller_hotkey.borrow_mut() = hotkey.to_owned();
        update_labels(&mut imp.controller_labels.borrow_mut(), hotkey);
    }

    fn register_controller_label(&self, label: &gtk::Label) {
        let imp = gtk::subclass::prelude::ObjectSubclassIsExt::imp(self);
        register_label(&mut imp.controller_labels.borrow_mut(), label);
    }

    fn unregister_controller_label(&self, label: &gtk::Label) {
        let imp = gtk::subclass::prelude::ObjectSubclassIsExt::imp(self);
        unregister_label(&mut imp.controller_labels.borrow_mut(), label);
    }
}

fn register_label(labels: &mut Vec<gtk::glib::WeakRef<gtk::Label>>, label: &gtk::Label) {
    labels.retain(|weak| weak.upgrade().is_some());
    if labels
        .iter()
        .filter_map(gtk::glib::WeakRef::upgrade)
        .any(|registered| registered == *label)
    {
        return;
    }
    labels.push(label.downgrade());
}

fn update_labels(labels: &mut Vec<gtk::glib::WeakRef<gtk::Label>>, text: &str) {
    labels.retain(|weak| {
        if let Some(label) = weak.upgrade() {
            label.set_text(text);
            true
        } else {
            false
        }
    });
}

fn unregister_label(labels: &mut Vec<gtk::glib::WeakRef<gtk::Label>>, label: &gtk::Label) {
    labels.retain(|weak| {
        weak.upgrade()
            .is_some_and(|registered| registered != *label)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_row_restore_checks_conflicts_without_touching_the_other_column() {
        let default = crate::uisettings::DEFAULT_HOTKEYS.iter()
            .find(|key| key.name == "Configure").unwrap();
        let row = HotkeyRow::binding(default.name, "F12", "A+B");
        let other = HotkeyRow::binding("Synthetic action", default.keyseq, "");
        let rows = vec![row.clone(), other.clone()];
        assert_eq!(restore_hotkey(&row, &rows, false), Err("Synthetic action".into()));
        assert_eq!(row.hotkey(), "F12");
        other.set_hotkey("F11");
        assert!(restore_hotkey(&row, &rows, false).is_ok());
        assert_eq!(row.hotkey(), default.keyseq);
        assert_eq!(row.controller_hotkey(), "A+B");
        other.set_hotkey(default.keyseq);
        assert!(restore_hotkey(&row, &rows, false).is_ok(), "restoring same value is allowed");

        let default = crate::uisettings::DEFAULT_HOTKEYS.iter()
            .find(|key| !key.controller_keyseq.is_empty()).unwrap();
        let row = HotkeyRow::binding(default.name, "F10", "A+B+X");
        let other = HotkeyRow::binding("Synthetic action", "", default.controller_keyseq);
        let rows = vec![row.clone(), other.clone()];
        assert!(restore_hotkey(&row, &rows, true).is_err());
        assert_eq!(row.controller_hotkey(), "A+B+X");
        other.set_controller_hotkey("");
        assert!(restore_hotkey(&row, &rows, true).is_ok());
        assert_eq!(row.controller_hotkey(), default.controller_keyseq);
        assert_eq!(row.hotkey(), "F10");
    }

    #[test]
    fn controller_combination_names_follow_upstream_order_and_side_buttons() {
        assert_eq!(get_button_combination_name(NpadButton::PLUS | NpadButton::B | NpadButton::A, true, true),
            "Home+Screenshot+A+B+Plus");
        assert_eq!(get_button_combination_name(NpadButton::LEFT_SL | NpadButton::RIGHT_SL | NpadButton::RIGHT_SR, false, false), "L+R");
        assert_eq!(get_button_combination_name(NpadButton::LEFT | NpadButton::STICK_R | NpadButton::MINUS, false, false),
            "Dpad_Left+Right_Stick+Minus");
    }

    #[test]
    #[ignore = "requires isolated GTK/SDL and input-factory state"]
    fn controller_capture_accumulates_detects_conflicts_and_cancels_cleanly() {
        use hid_core::frontend::emulated_controller::EmulatedController;
        use hid_core::hid_types::NpadStyleIndex;
        gtk::init().unwrap();
        let mut input = input_common::InputSubsystem::new();
        input.initialize();
        let controller = Arc::new(parking_lot::Mutex::new(EmulatedController::new(NpadIdType::Player1)));
        {
            let mut owner = controller.lock();
            owner.set_npad_style_index(NpadStyleIndex::Fullkey);
            for index in 0..2 {
                let mut params = common::param_package::ParamPackage::default();
                params.set_str("engine", "virtual_gamepad".into());
                params.set_str("guid", common::uuid::UUID::default().raw_string());
                params.set_int("port", 0);
                params.set_int("pad", 0);
                params.set_int("button", index as i32);
                owner.set_button_param(index, params);
            }
            owner.reload_input();
            owner.enable_configuration();
        }
        let row = HotkeyRow::binding("First", "", "Home");
        let conflict = HotkeyRow::binding("Second", "", "B");
        let capture = Rc::new(ControllerCapture {
            controller: Arc::clone(&controller), rows: Rc::new(vec![row.clone(), conflict]),
            active: RefCell::new(None), timer: RefCell::new(None),
        });
        let source = gtk::Label::new(None);
        capture.start(source.upcast_ref(), &row);
        assert!(!controller.lock().is_configuring_mode());
        let gamepad = input.get_virtual_gamepad_mut().unwrap();
        gamepad.set_button_state_by_id(0, 0, true);
        assert!(capture.poll());
        gamepad.set_button_state_by_id(0, 0, false);
        gamepad.set_button_state_by_id(0, 1, true);
        assert!(capture.poll());
        assert_eq!(row.controller_hotkey(), "A+B...");
        gamepad.set_button_state_by_id(0, 1, false);
        // Exercise the actual timeout source without sleeping 2.5 seconds.
        capture.active.borrow_mut().as_mut().unwrap().started = Instant::now() - Duration::from_secs(3);
        let deadline = Instant::now() + Duration::from_secs(2);
        while capture.active.borrow().is_some() && Instant::now() < deadline {
            while gtk::glib::MainContext::default().pending() { gtk::glib::MainContext::default().iteration(false); }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(capture.active.borrow().is_none());
        assert!(capture.timer.borrow().is_none());
        assert_eq!(row.controller_hotkey(), "A+B");
        assert!(controller.lock().is_configuring_mode());

        capture.start(source.upcast_ref(), &row);
        gamepad.set_button_state_by_id(0, 1, true);
        capture.poll();
        capture.timer.borrow_mut().take().unwrap().remove();
        capture.finish(false);
        assert_eq!(row.controller_hotkey(), "A+B", "conflict restores previous sequence");
        gamepad.set_button_state_by_id(0, 1, false);
        capture.start(source.upcast_ref(), &row);
        capture.cancel();
        assert_eq!(row.controller_hotkey(), "A+B");
        assert!(capture.timer.borrow().is_none());
        assert!(controller.lock().is_configuring_mode());
        capture.start(source.upcast_ref(), &row);
        capture.timer.borrow_mut().take().unwrap().remove();
        capture.finish(false);
        assert_eq!(row.controller_hotkey(), "A+B", "empty timeout restores previous sequence");
        for widget in gtk::Window::list_toplevels() {
            if let Ok(window) = widget.downcast::<gtk::Window>() { window.close(); }
        }
    }

    #[test]
    fn default_hotkeys_match_upstream_count() {
        // Upstream declares `std::array<Shortcut, 33> default_hotkeys`.
        assert_eq!(crate::uisettings::DEFAULT_HOTKEYS.len(), 33);
    }

    #[test]
    fn recently_added_defaults_keep_upstream_positional_order() {
        let tail: Vec<&str> = crate::uisettings::DEFAULT_HOTKEYS[27..]
            .iter()
            .map(|hotkey| hotkey.name)
            .collect();
        assert_eq!(
            tail,
            [
                "Toggle Turbo Speed",
                "Toggle Slow Speed",
                "Toggle Mouse Panning",
                "Toggle Renderdoc Capture",
                "Toggle Status Bar",
                "Toggle Performance Overlay",
            ]
        );
    }

    #[test]
    fn renderdoc_capture_has_no_default_binding() {
        // The only entry upstream ships with both hotkey strings empty.
        let entry = crate::uisettings::DEFAULT_HOTKEYS
            .iter()
            .find(|hotkey| hotkey.name == "Toggle Renderdoc Capture")
            .expect("entry present");
        assert_eq!(entry.keyseq, "");
        assert_eq!(entry.controller_keyseq, "");
    }

    #[test]
    fn duplicate_comparison_uses_native_sequence_semantics() {
        assert!(same_key_sequence("Ctrl+M", "ctrl + m"));
        assert!(!same_key_sequence("Ctrl+M", "Ctrl+N"));
    }
}
