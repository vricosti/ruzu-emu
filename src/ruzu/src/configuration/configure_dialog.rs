// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/eden/src/yuzu/configuration/configure_dialog.cpp`
// (`ConfigureDialog`), whose widget tree lives in `configure.ui`.
//
// Upstream layout:
//   * a `QListWidget` (`selectorList`) on the left with six rows;
//   * a `QTabWidget` (`tabWidget`) on the right whose tabs are *rebuilt* every
//     time the selection changes (`UpdateVisibleTabs` clears it and re-adds only
//     the tabs belonging to the selected row);
//   * a status label ("Some settings are only available when a game is not
//     running.") and a `QDialogButtonBox` with Cancel / OK along the bottom.
//
// The row → tabs mapping is upstream `PopulateSelectionList`:
//   General  → General, Hotkeys, UI, Web, Debug
//   System   → System, Profiles, Network, Filesystem, Applets
//   CPU      → CPU
//   Graphics → Graphics, Advanced, Extras
//   Audio    → Audio
//   Controls → Player 1..8, Advanced
//
// Note that upstream's tab *titles* come from each page's `accessibleName()`,
// which is why the "UI" page shows as "UI" and the graphics advanced page shows
// as "Advanced" rather than their class names.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;
use gtk::{glib, Window};

use super::{
    configure_applets, configure_audio, configure_cpu, configure_debug_tab, configure_filesystem,
    configure_general, configure_graphics, configure_graphics_advanced,
    configure_graphics_extensions, configure_hotkeys, configure_input, configure_network,
    configure_profile_manager, configure_system, configure_ui, configure_web,
};

/// Default dialog geometry. Upstream calls `adjustSize()` and lets Qt derive
/// the size from `configure.ui` plus the visible page. The current Eden dialog
/// resolves to 1280×720 on the same desktop/session used to validate Reden.
const DEFAULT_WIDTH: i32 = 1280;
const DEFAULT_HEIGHT: i32 = 720;

/// Fixed width of the left selector column, matching `configure.ui`'s
/// `selectorList` `maximumSize` of 120px.
const SELECTOR_WIDTH: i32 = 120;

/// A configuration page: the tab title plus its content widget.
///
/// Upstream stores the pages as `QWidget*` and reads the title back from
/// `accessibleName()`; carrying the title alongside the widget is the same
/// information without the Qt property round-trip.
pub struct Page {
    pub title: String,
    pub widget: gtk::Widget,
    /// Applies this page's widget state back into the settings — upstream
    /// `ApplyConfiguration()` on each tab.
    pub apply: Box<dyn Fn()>,
}

impl Page {
    pub fn new(title: &str, widget: impl IsA<gtk::Widget>, apply: impl Fn() + 'static) -> Self {
        Self {
            title: title.to_string(),
            widget: widget.upcast(),
            apply: Box::new(apply),
        }
    }
}

/// One row of the left selector list, with the pages it reveals.
struct Section {
    name: &'static str,
    pages: Vec<Page>,
    /// Applies the pages through their upstream owner. Most sections expose
    /// independent pages; Controls must retain `ConfigureInput`'s global-player
    /// storage bracket around all of its subpages.
    apply: fn(&[Page]),
}

fn apply_pages(pages: &[Page]) {
    for page in pages {
        (page.apply)();
    }
}

/// The configuration dialog — upstream `ConfigureDialog`.
pub struct ConfigureDialog {
    window: Window,
    notebook: gtk::Notebook,
    sections: Rc<Vec<Section>>,
    /// Input owners retained so closing the asynchronous GTK dialog can end
    /// configuration synchronously, before its page widgets are destroyed.
    input_subsystem: Rc<RefCell<input_common::InputSubsystem>>,
    hid_core: Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>,
    /// Index of the section currently shown in the notebook, so a re-selection
    /// of the same row doesn't rebuild the tabs (which would reset the tab
    /// position, unlike upstream's `QSignalBlocker`-guarded rebuild).
    shown: RefCell<Option<usize>>,
    /// Notifies the main-window owner after OK has applied every page. Upstream
    /// obtains the same edge from `QDialog::Accepted` and then refreshes its
    /// permanent status widgets.
    on_applied: RefCell<Option<Box<dyn Fn()>>>,
}

impl ConfigureDialog {
    /// Build the dialog. Mirrors the upstream constructor: create every tab,
    /// populate the selector list, then select row 0.
    pub fn new(
        parent: Option<&impl IsA<Window>>,
        input_subsystem: Rc<RefCell<input_common::InputSubsystem>>,
        hid_core: Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>,
        runtime_lock: bool,
    ) -> Rc<Self> {
        // Upstream `ConfigureDialog` sets this before constructing any page so
        // every widget represents the global configuration even while a title
        // is running with custom settings selected.
        common::settings::set_configuring_global(true);

        let window = Window::builder()
            .title("ruzu Configuration")
            .modal(true)
            .default_width(DEFAULT_WIDTH)
            .default_height(DEFAULT_HEIGHT)
            .build();
        // `ConfigureDialog dialog(this); dialog.exec()` is both window-modal
        // and parent-owned upstream. The transient relationship is required by
        // GTK for `modal(true)` to block and stay above the main window.
        if let Some(parent) = parent {
            window.set_transient_for(Some(parent));
            window.set_destroy_with_parent(true);
        }

        // Upstream constructs Advanced Graphics first and gives Graphics a
        // callback to `ExposeComputeOption` when a Vulkan device requires it.
        let advanced_graphics = configure_graphics_advanced::page();
        let graphics =
            configure_graphics::page(advanced_graphics.expose_compute_option, runtime_lock);

        // Upstream `PopulateSelectionList`'s six rows, in order.
        let sections = vec![
            Section {
                name: "General",
                pages: vec![
                    configure_general::page(),
                    configure_hotkeys::page(),
                    configure_ui::page(),
                    configure_web::page(),
                    configure_debug_tab::page(),
                ],
                apply: apply_pages,
            },
            Section {
                name: "System",
                pages: vec![
                    configure_system::page(),
                    configure_profile_manager::page(),
                    configure_network::page(),
                    configure_filesystem::page(),
                    configure_applets::page(),
                ],
                apply: apply_pages,
            },
            Section {
                name: "CPU",
                pages: vec![configure_cpu::page()],
                apply: apply_pages,
            },
            Section {
                name: "Graphics",
                pages: vec![
                    graphics,
                    advanced_graphics.page,
                    configure_graphics_extensions::page(),
                ],
                apply: apply_pages,
            },
            Section {
                name: "Audio",
                pages: vec![configure_audio::page()],
                apply: apply_pages,
            },
            Section {
                name: "Controls",
                pages: configure_input::pages(Rc::clone(&input_subsystem), Arc::clone(&hid_core)),
                apply: configure_input::apply_configuration,
            },
        ];

        // --- Left selector list (upstream `selectorList`) --------------------
        let selector = gtk::ListBox::new();
        selector.set_selection_mode(gtk::SelectionMode::Single);
        selector.set_width_request(SELECTOR_WIDTH);
        for section in &sections {
            let label = gtk::Label::new(Some(section.name));
            label.set_xalign(0.0);
            label.set_margin_top(2);
            label.set_margin_bottom(2);
            label.set_margin_start(4);
            selector.append(&label);
        }

        let selector_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(SELECTOR_WIDTH)
            .child(&selector)
            .build();

        // --- Right tab widget (upstream `tabWidget`) -------------------------
        let notebook = gtk::Notebook::new();
        notebook.set_hexpand(true);
        notebook.set_vexpand(true);
        notebook.set_scrollable(true);

        let split = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        split.set_margin_top(10);
        split.set_margin_start(10);
        split.set_margin_end(10);
        split.append(&selector_scroll);
        split.append(&notebook);

        // --- Bottom bar (upstream status label + `buttonBox`) ----------------
        let status = gtk::Label::new(Some(
            "Some settings are only available when a game is not running.",
        ));
        status.set_xalign(0.0);
        status.set_hexpand(true);

        let cancel = gtk::Button::with_label("Cancel");
        let ok = gtk::Button::with_label("OK");

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        buttons.set_margin_top(10);
        buttons.set_margin_bottom(10);
        buttons.set_margin_start(10);
        buttons.set_margin_end(10);
        buttons.append(&status);
        buttons.append(&cancel);
        buttons.append(&ok);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&split);
        root.append(&buttons);
        window.set_child(Some(&root));

        let this = Rc::new(Self {
            window,
            notebook,
            sections: Rc::new(sections),
            input_subsystem,
            hid_core,
            shown: RefCell::new(None),
            on_applied: RefCell::new(None),
        });

        // Upstream connects `itemSelectionChanged` to `UpdateVisibleTabs`.
        selector.connect_row_selected(glib::clone!(
            #[weak(rename_to = dialog)]
            this,
            move |_, row| {
                if let Some(row) = row {
                    dialog.update_visible_tabs(row.index() as usize);
                }
            }
        ));

        // Cancel discards; OK applies then closes — upstream wires the
        // `QDialogButtonBox`'s `rejected` / `accepted` the same way.
        cancel.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            this,
            move |_| dialog.window.close()
        ));
        ok.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            this,
            move |_| {
                dialog.apply_configuration();
                if let Some(callback) = dialog.on_applied.borrow().as_ref() {
                    callback();
                }
                dialog.window.close();
            }
        ));

        // Upstream: `ui->selectorList->setCurrentRow(0);`
        if let Some(first) = selector.row_at_index(0) {
            selector.select_row(Some(&first));
        }

        this
    }

    /// Rebuild the notebook so it holds exactly the selected section's pages —
    /// upstream `UpdateVisibleTabs`.
    fn update_visible_tabs(&self, section_index: usize) {
        if *self.shown.borrow() == Some(section_index) {
            return;
        }
        let Some(section) = self.sections.get(section_index) else {
            return;
        };

        while self.notebook.n_pages() > 0 {
            self.notebook.remove_page(Some(0));
        }
        for page in &section.pages {
            log::debug!("configure: showing tab {}", page.title);
            self.notebook
                .append_page(&page.widget, Some(&gtk::Label::new(Some(&page.title))));
        }
        *self.shown.borrow_mut() = Some(section_index);
    }

    /// Push every page's widget state back into the settings — upstream
    /// `ConfigureDialog::ApplyConfiguration`, which calls `ApplyConfiguration()`
    /// on each tab regardless of which one is currently visible.
    fn apply_configuration(&self) {
        for section in self.sections.iter() {
            (section.apply)(&section.pages);
        }
        // Upstream `GMainWindow::OnConfigure` calls `config->Save()` once the
        // dialog is accepted; without it the new bindings would live only in
        // this process and be gone next launch.
        if let Err(error) = super::qt_config::save_global_values() {
            log::error!("Failed to save global settings: {error}");
        }
        if let Err(error) = super::qt_config::save_control_values() {
            log::error!("Failed to save control settings: {error}");
        }
        if let Err(error) = super::qt_config::save_ui_language() {
            log::error!("Failed to save interface language: {error}");
        }
        common::settings::log_settings(&common::settings::values());
    }

    /// Show the dialog — upstream `ConfigureDialog::exec()`.
    pub fn present(&self) {
        crate::i18n::translate_widget_tree(&self.window);
        self.window.present();
    }

    /// Connect the main-window work performed after upstream's accepted
    /// configuration dialog has applied its values.
    pub fn connect_applied(&self, callback: impl Fn() + 'static) {
        *self.on_applied.borrow_mut() = Some(Box::new(callback));
    }

    /// Notify the owner once the GTK window closes so its `Rc` can be dropped,
    /// matching upstream's stack-owned dialog lifetime.
    pub fn connect_closed(&self, callback: impl Fn() + 'static) {
        let input_subsystem = Rc::clone(&self.input_subsystem);
        let hid_core = Arc::clone(&self.hid_core);
        self.window.connect_close_request(move |_| {
            finish_input_configuration(&input_subsystem, &hid_core);
            callback();
            glib::Propagation::Proceed
        });
    }
}

/// Finish the input lifetime owned by `ConfigureInput` before GTK releases its
/// pages. Qt destroys every `ConfigureInputPlayer` synchronously when
/// `ConfigureDialog::exec()` returns; the GTK close signal is asynchronous, so
/// relying only on `PlayerPage::drop` can leave both the physical engines and
/// guest-facing controllers in configuration mode while the game resumes.
fn finish_input_configuration(
    input_subsystem: &Rc<RefCell<input_common::InputSubsystem>>,
    hid_core: &Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>,
) {
    input_subsystem.borrow_mut().stop_mapping();

    let controllers = {
        let hid_core = hid_core.lock();
        let mut controllers = (0..8)
            .map(|index| hid_core.get_emulated_controller_by_index(index))
            .collect::<Vec<_>>();
        controllers
            .push(hid_core.get_emulated_controller(hid_core::hid_types::NpadIdType::Handheld));
        controllers
    };
    for controller in controllers {
        controller.lock().disable_configuration();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_dialog_disables_exactly_the_configure_input_controllers() {
        let input_subsystem = Rc::new(RefCell::new(input_common::InputSubsystem::new()));
        let hid_core = Arc::new(parking_lot::Mutex::new(hid_core::hid_core::HIDCore::new()));
        let (player_one, handheld, other) = {
            let hid = hid_core.lock();
            (
                hid.get_emulated_controller(hid_core::hid_types::NpadIdType::Player1),
                hid.get_emulated_controller(hid_core::hid_types::NpadIdType::Handheld),
                hid.get_emulated_controller(hid_core::hid_types::NpadIdType::Other),
            )
        };
        player_one.lock().enable_configuration();
        handheld.lock().enable_configuration();
        // `Other` has no ConfigureInputPlayer page and must not have temporary
        // state committed by this dialog's cleanup.
        other.lock().enable_configuration();

        finish_input_configuration(&input_subsystem, &hid_core);

        assert!(!player_one.lock().is_configuring_mode());
        assert!(!handheld.lock().is_configuring_mode());
        assert!(other.lock().is_configuring_mode());
    }
}
