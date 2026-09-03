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

use gtk::prelude::*;

use super::configure_dialog::Page;

/// The context every default hotkey belongs to — upstream's group row.
const CONTEXT: &str = "Main Window";

/// Column widths, roughly matching the Qt tree's resize-to-contents result.
const ACTION_COLUMN_WIDTH: i32 = 420;
const HOTKEY_COLUMN_WIDTH: i32 = 150;

/// Build the Hotkeys tab — upstream `ConfigureHotkeys`.
pub fn page() -> Page {
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
    ));
    view.append_column(&hotkey_column(HOTKEY_COLUMN_WIDTH, Rc::clone(&rows)));
    view.append_column(&text_column(
        "Controller Hotkey",
        HOTKEY_COLUMN_WIDTH,
        |row| row.controller_hotkey(),
    ));

    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&view)
        .build();
    column.append(&scroller);

    clear_all.connect_clicked({
        let rows = Rc::clone(&rows);
        move |_| {
            for row in rows.iter() {
                row.set_hotkey("");
                row.set_controller_hotkey("");
            }
        }
    });
    restore_defaults.connect_clicked({
        let rows = Rc::clone(&rows);
        move |_| {
            for (row, default) in rows.iter().zip(crate::uisettings::DEFAULT_HOTKEYS) {
                row.set_hotkey(default.keyseq);
                row.set_controller_hotkey(default.controller_keyseq);
            }
        }
    });

    Page::new("Hotkeys", column, move || {
        crate::uisettings::with_mut(|values| {
            for (shortcut, row) in values.shortcuts.iter_mut().zip(rows.iter()) {
                shortcut.keyseq = row.hotkey();
                shortcut.controller_keyseq = row.controller_hotkey();
            }
        });
    })
}

/// Editable keyboard-binding column. Upstream routes a double-click in either
/// the action or keyboard column to the keyboard `SequenceDialog`; the binding
/// cell is the GTK interaction target advertised by the hint above the table.
fn hotkey_column(width: i32, rows: Rc<Vec<HotkeyRow>>) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap().clone();
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);

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
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let list_item = item.downcast_ref::<gtk::ListItem>().unwrap().clone();
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        let expander = gtk::TreeExpander::new();
        expander.set_child(Some(&label));

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

/// Plain text column.
fn text_column(title: &str, width: i32, get: fn(&HotkeyRow) -> String) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
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
