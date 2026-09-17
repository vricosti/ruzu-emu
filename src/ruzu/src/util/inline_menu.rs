// SPDX-License-Identifier: GPL-3.0-or-later
//
// Gamescope/X11-only in-window menus. Based on the supplied Deck investigation
// and workaround (deck, 2026-09-16); ordinary desktop menus remain native.
//
// Why this exists
// ---------------
// In Steam Game Mode, games are hosted by gamescope's nested Xwayland. Every
// native GTK popup (`GtkPopover`, `GtkPopoverMenuBar`) maps a *new X window*.
// gamescope's XWM reconfigures that window in a tight loop, which burns CPU
// (~5000 synchronous X round-trips per second), wedges the GTK main loop in an
// answer-less `XTranslateCoordinates` / `XReconfigureWMWindow` round-trip, and
// can crash Xwayland in its composite path:
//
//   Dispatch -> ProcMapWindow -> MapWindow -> RealizeTree -> compRealizeWindow
//     -> compCheckRedirect -> compSetPixmapVisitWindow -> damageSetWindowPixmap
//     -> SIGSEGV  ("xwm: X11 I/O error! This is fatal. Aborting...")
//
// The reporter reproduced this with a small GTK app on two GTK versions;
// the same tests on their normal desktop did not reproduce it.
//
// Rendering the menu *inside* the toplevel's own window maps no X window at
// all, avoiding that reported trigger. This is not a fix to the compositor.
//
// Qt has no counterpart for this GTK-specific adapter. See DIFF.md for the
// intentional frontend adaptation; action ownership stays with existing owners.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;

pub(crate) fn enabled() -> bool {
    #[cfg(target_os = "linux")]
    {
        let x11 = gtk::gdk::Display::default()
            .is_some_and(|display| display.is::<gdk4_x11::X11Display>());
        return popup_policy(
            x11,
            std::env::var("RUZU_INLINE_MENUS").ok().as_deref(),
            &std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
            std::env::var_os("GAMESCOPE_WAYLAND_DISPLAY").is_some(),
        );
    }
    #[cfg(not(target_os = "linux"))]
    false
}

/// Shared by real keyboard events and controller navigation; no injected X keys.
pub(crate) fn focus_bar(window: &gtk::Window) -> bool {
    let Some(inner) = ACTIVE_MENUBAR.with(|slot| slot.borrow().upgrade()) else {
        return false;
    };
    if !inner.row.is_mapped() || inner.row.root().as_ref() != Some(window.upcast_ref()) {
        return false;
    }
    let button = inner.buttons.borrow().first().cloned();
    button.is_some_and(|button| button.grab_focus())
}

pub(crate) fn navigate(
    window: &gtk::Window,
    key: super::controller_navigation::NavigationKey,
) -> bool {
    use super::controller_navigation::NavigationKey as Key;
    if !enabled() {
        return false;
    }
    if key == Key::Escape {
        let mut focus = gtk::prelude::GtkWindowExt::focus(window);
        while let Some(widget) = focus {
            if widget.has_css_class("ruzu-inline-selector") {
                let button = widget.first_child().and_then(|model| model.next_sibling());
                let scroll = button.as_ref().and_then(|button| button.next_sibling());
                if let Some(scroll) = scroll.filter(|scroll| scroll.is_visible()) {
                    scroll.set_visible(false);
                    if let Some(button) = button {
                        button.grab_focus();
                    }
                    return true;
                }
            }
            focus = widget.parent();
        }
    }
    let popup = ACTIVE_POPUP.with(|slot| slot.borrow().clone());
    if let Some((overlay, panel)) = popup {
        if overlay
            .root()
            .as_ref()
            .is_some_and(|root| root == window.upcast_ref::<gtk::Root>())
        {
            match key {
                Key::Escape => hide_context_menu(),
                Key::Up => {
                    panel.child_focus(gtk::DirectionType::TabBackward);
                }
                Key::Down => {
                    panel.child_focus(gtk::DirectionType::TabForward);
                }
                _ => return false,
            }
            return true;
        }
    }
    let Some(inner) = ACTIVE_MENUBAR.with(|slot| slot.borrow().upgrade()) else {
        return false;
    };
    if !inner.row.is_mapped() || inner.row.root().as_ref() != Some(window.upcast_ref()) {
        return false;
    }
    if key == Key::Menu {
        toggle_inner(&inner, 0);
        return true;
    }
    let Some(index) = inner.open.get() else {
        return false;
    };
    match key {
        Key::Escape => cancel_inner(&inner),
        Key::Left | Key::Right => {
            let count = inner.buttons.borrow().len();
            if count != 0 {
                open_inner(
                    &inner,
                    (index + if key == Key::Right { 1 } else { count - 1 }) % count,
                );
            }
        }
        Key::Up | Key::Down => {
            if let Some(panel) = inner.panels.borrow().get(index) {
                panel.child_focus(if key == Key::Down {
                    gtk::DirectionType::TabForward
                } else {
                    gtk::DirectionType::TabBackward
                });
            }
        }
        _ => return false,
    }
    true
}

#[cfg(any(target_os = "linux", test))]
fn popup_policy(
    x11: bool,
    override_value: Option<&str>,
    desktop: &str,
    gamescope_socket: bool,
) -> bool {
    x11 && match override_value {
        Some("0") => false,
        Some("1") => true,
        _ => {
            gamescope_socket
                || desktop
                    .split(':')
                    .any(|part| part.eq_ignore_ascii_case("gamescope"))
        }
    }
}

/// Marks the always-visible row of top-level menu buttons.
pub(crate) const MENUBAR_CSS_CLASS: &str = "ruzu-inline-menubar";
/// Marks a drop-down panel (an overlay child that holds the menu items).
pub(crate) const PANEL_CSS_CLASS: &str = "ruzu-menu-panel";
const ITEM_CSS_CLASS: &str = "ruzu-menu-item";
const HEADER_CSS_CLASS: &str = "ruzu-menu-header";
const SEPARATOR_CSS_CLASS: &str = "ruzu-menu-separator";

fn install_css() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    thread_local! { static INSTALLED: Cell<bool> = const { Cell::new(false) }; }
    if INSTALLED.with(|installed| installed.replace(true)) {
        return;
    }
    let css = gtk::CssProvider::new();
    css.load_from_data(".ruzu-menu-panel { background: @theme_bg_color; color: @theme_fg_color; border: 1px solid alpha(currentColor, 0.35); padding: 4px; } .ruzu-menu-item { border-radius: 0; padding: 6px 12px; } .ruzu-menu-header { font-weight: bold; padding: 4px; } .ruzu-menu-separator { margin: 4px 0; }");
    gtk::style_context_add_provider_for_display(
        &display,
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

struct Inner {
    row: gtk::Box,
    /// Overlay that hosts the panels, if the bar has been attached to one.
    overlay: RefCell<Option<gtk::Overlay>>,
    buttons: RefCell<Vec<gtk::Button>>,
    panels: RefCell<Vec<gtk::Box>>,
    open: Cell<Option<usize>>,
    controllers_installed: Cell<bool>,
}

/// A menu bar whose drop-down panels are ordinary child widgets of the window.
pub(crate) struct InlineMenuBar {
    inner: Rc<Inner>,
}

impl InlineMenuBar {
    pub(crate) fn new(model: &gtk::gio::MenuModel) -> Self {
        install_css();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.add_css_class(MENUBAR_CSS_CLASS);
        row.set_halign(gtk::Align::Fill);
        row.set_hexpand(true);
        let bar = Self {
            inner: Rc::new(Inner {
                row,
                overlay: RefCell::new(None),
                buttons: RefCell::new(Vec::new()),
                panels: RefCell::new(Vec::new()),
                open: Cell::new(None),
                controllers_installed: Cell::new(false),
            }),
        };
        bar.set_model(model);
        bar
    }

    pub(crate) fn row(&self) -> &gtk::Box {
        &self.inner.row
    }

    /// Adds the menu panels to the window's overlay. Each panel is a direct
    /// overlay child so it only covers (and only picks events on) its own
    /// surface: clicks outside it reach the content underneath.
    pub(crate) fn attach_to(&self, overlay: &gtk::Overlay) {
        *self.inner.overlay.borrow_mut() = Some(overlay.clone());
        ACTIVE_MENUBAR.with(|weak| *weak.borrow_mut() = Rc::downgrade(&self.inner));
        for panel in self.inner.panels.borrow().iter() {
            if panel.parent().is_none() {
                overlay.add_overlay(panel);
            }
        }
        ensure_window_controllers(&self.inner);
    }

    pub(crate) fn height(&self) -> i32 {
        self.inner.row.height()
    }

    pub(crate) fn set_visible(&self, visible: bool) {
        if !visible {
            self.close();
        }
        self.inner.row.set_visible(visible);
    }

    /// Rebuilds the bar from a menu model (also used for live TAS/recent-files
    /// updates previously delivered through `GtkPopoverMenuBar::set_menu_model`).
    pub(crate) fn set_model(&self, model: &gtk::gio::MenuModel) {
        let inner = &self.inner;
        close_inner(inner);
        let overlay = inner.overlay.borrow().clone();
        for panel in inner.panels.borrow().iter() {
            if let Some(overlay) = overlay.as_ref() {
                overlay.remove_overlay(panel);
            }
        }
        inner.panels.borrow_mut().clear();
        for button in inner.buttons.borrow().iter() {
            inner.row.remove(button);
        }
        inner.buttons.borrow_mut().clear();

        let close: Rc<dyn Fn()> = {
            let weak = Rc::downgrade(inner);
            Rc::new(move || {
                if let Some(inner) = weak.upgrade() {
                    close_inner(&inner);
                }
            })
        };

        for i in 0..model.n_items() {
            let label = item_label(model, i).unwrap_or_default();
            let button = gtk::Button::with_mnemonic(&label);
            button.add_css_class("flat");
            button.add_css_class("ruzu-menubar-item");

            let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
            panel.add_css_class(PANEL_CSS_CLASS);
            panel.set_halign(gtk::Align::Start);
            panel.set_valign(gtk::Align::Start);
            panel.set_visible(false);
            let entries = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .max_content_height(500)
                .child(&entries)
                .build();
            panel.append(&scroll);
            match model.item_link(i, gtk::gio::MENU_LINK_SUBMENU) {
                Some(submenu) => append_model(&entries, &submenu, 0, &close, None),
                None => append_entry(&entries, model, i, 0, &close, None),
            }
            if let Some(overlay) = overlay.as_ref() {
                overlay.add_overlay(&panel);
            }
            inner.panels.borrow_mut().push(panel);

            {
                let weak = Rc::downgrade(inner);
                let index = i as usize;
                button.connect_clicked(move |_| {
                    if let Some(inner) = weak.upgrade() {
                        toggle_inner(&inner, index);
                    }
                });
            }
            inner.row.append(&button);
            inner.buttons.borrow_mut().push(button);
        }
        ensure_window_controllers(inner);
    }

    pub(crate) fn close(&self) {
        close_inner(&self.inner);
    }
}

fn close_inner(inner: &Rc<Inner>) {
    if let Some(index) = inner.open.get() {
        if std::env::var_os("RUZU_MENU_DEBUG").is_some() {
            eprintln!("inline_menu: close {index}");
        }
        let panel = inner.panels.borrow().get(index).cloned();
        if let Some(panel) = panel {
            panel.set_visible(false);
        }
    }
    inner.open.set(None);
    sync_render_suppression();
}

// Cancellation returns to the originating menu. Other closure paths (action
// activation, outside click, unmap) must not steal focus from their destination.
fn cancel_inner(inner: &Rc<Inner>) {
    let button = inner.open.get().and_then(|index| {
        inner.buttons.borrow().get(index).cloned()
    });
    close_inner(inner);
    if let Some(button) = button {
        button.grab_focus();
    }
}

fn open_inner(inner: &Rc<Inner>, index: usize) {
    ensure_window_controllers(inner);
    let buttons = inner.buttons.borrow().clone();
    let panels = inner.panels.borrow().clone();
    let (Some(button), Some(panel)) = (buttons.get(index), panels.get(index)) else {
        return;
    };
    for (i, other) in panels.iter().enumerate() {
        other.set_visible(i == index);
    }
    // Anchor the panel under its button, kept inside the window.
    let mut x = 0.0_f64;
    let mut y = inner.row.height() as f64;
    let bounds_target: gtk::Widget = inner
        .overlay
        .borrow()
        .clone()
        .map(gtk::Widget::from)
        .unwrap_or_else(|| inner.row.clone().upcast());
    if let Some(bounds) = button.compute_bounds(&bounds_target) {
        x = bounds.x() as f64;
        y = (bounds.y() + bounds.height()) as f64;
    }
    let target_width = bounds_target.width() as f64;
    if let Some(scroll) = panel.first_child().and_downcast::<gtk::ScrolledWindow>() {
        scroll.set_max_content_height((bounds_target.height() - y as i32 - 12).max(80));
    }
    let panel_width = panel.preferred_size().1.width() as f64;
    if target_width > 0.0 && panel_width > 0.0 && x + panel_width > target_width {
        x = (target_width - panel_width).max(0.0);
    }
    panel.set_margin_start(x.max(0.0) as i32);
    panel.set_margin_top(y.max(0.0) as i32);
    inner.open.set(Some(index));
    if std::env::var_os("RUZU_MENU_DEBUG").is_some() {
        let sens: String = panel
            .observe_children()
            .iter::<gtk::glib::Object>()
            .filter_map(Result::ok)
            .filter_map(|object| object.downcast::<gtk::Widget>().ok())
            .map(|widget| if widget.is_sensitive() { '1' } else { '0' })
            .collect();
        eprintln!(
            "inline_menu: open {index} items={} sens={} size={}x{} pos=({},{})",
            panels
                .get(index)
                .map_or(0, |panel| panel.observe_children().n_items()),
            sens,
            panel.width(),
            panel.height(),
            x,
            y,
        );
    }
    // Move keyboard/controller focus into the freshly opened panel.
    panel.child_focus(gtk::DirectionType::TabForward);
    sync_render_suppression();
}

fn toggle_inner(inner: &Rc<Inner>, index: usize) {
    if inner.open.get() == Some(index) {
        close_inner(inner);
    } else {
        open_inner(inner, index);
    }
}

/// Dismiss on Escape and on a click outside the bar and the open panel.
fn ensure_window_controllers(inner: &Rc<Inner>) {
    if inner.controllers_installed.get() {
        return;
    }
    let Some(root) = inner.row.root() else {
        return;
    };
    let Some(window) = root.downcast_ref::<gtk::Window>().cloned() else {
        return;
    };

    let key = gtk::EventControllerKey::new();
    {
        let weak = Rc::downgrade(inner);
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gtk::gdk::Key::Escape {
                if let Some(inner) = weak.upgrade() {
                    if inner.open.get().is_some() {
                        cancel_inner(&inner);
                        return gtk::glib::Propagation::Stop;
                    }
                }
            }
            gtk::glib::Propagation::Proceed
        });
    }
    key.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_window = window.downgrade();
    key.connect_key_pressed(move |_, key, _, _| {
        use super::controller_navigation::NavigationKey as Key;
        let navigation = match key {
            gtk::gdk::Key::Up => Key::Up,
            gtk::gdk::Key::Down => Key::Down,
            gtk::gdk::Key::Left => Key::Left,
            gtk::gdk::Key::Right => Key::Right,
            gtk::gdk::Key::F10 => Key::Menu,
            _ => return gtk::glib::Propagation::Proceed,
        };
        if weak_window
            .upgrade()
            .is_some_and(|window| navigate(&window, navigation))
        {
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    window.add_controller(key);

    let click = gtk::GestureClick::new();
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let weak = Rc::downgrade(inner);
        let bounds_target = window.downgrade();
        click.connect_pressed(move |gesture, _, x, y| {
            let Some(bounds_target) = bounds_target.upgrade() else {
                return;
            };
            let Some(inner) = weak.upgrade() else { return };
            let Some(index) = inner.open.get() else {
                return;
            };
            let point = gtk::graphene::Point::new(x as f32, y as f32);
            let inside = |widget: &gtk::Widget| {
                widget
                    .compute_bounds(&bounds_target)
                    .is_some_and(|bounds| bounds.contains_point(&point))
            };
            let on_row = inside(inner.row.upcast_ref());
            let on_panel = inner
                .panels
                .borrow()
                .get(index)
                .is_some_and(|panel| inside(panel.upcast_ref()));
            if on_row || on_panel {
                // Release the sequence so the widget under the pointer still
                // receives the click (a capturing gesture claims it otherwise).
                gesture.set_state(gtk::EventSequenceState::Denied);
                return;
            }
            close_inner(&inner);
        });
    }
    window.add_controller(click);
    let weak = Rc::downgrade(inner);
    window.connect_unmap(move |_| {
        if let Some(inner) = weak.upgrade() {
            close_inner(&inner);
        }
    });

    inner.controllers_installed.set(true);
}

fn item_label(model: &gtk::gio::MenuModel, index: i32) -> Option<String> {
    model
        .item_attribute_value(index, "label", None)
        .and_then(|value| value.get::<String>())
}

/// GTK menu labels mark mnemonics with `_`; plain labels render them literally.
fn strip_mnemonic(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '_' {
            if chars.peek() == Some(&'_') {
                chars.next();
                out.push('_');
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn separator() -> gtk::Separator {
    let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
    sep.add_css_class(SEPARATOR_CSS_CLASS);
    sep
}

fn append_model(
    container: &gtk::Box,
    model: &gtk::gio::MenuModel,
    depth: usize,
    close: &Rc<dyn Fn()>,
    local_actions: Option<(&str, &gtk::gio::ActionGroup)>,
) {
    for i in 0..model.n_items() {
        match model.item_link(i, gtk::gio::MENU_LINK_SECTION) {
            Some(section) => {
                if section.n_items() == 0 {
                    continue;
                }
                if container.first_child().is_some() {
                    container.append(&separator());
                }
                append_model(container, &section, depth, close, local_actions);
            }
            None => append_entry(container, model, i, depth, close, local_actions),
        }
    }
}

fn append_entry(
    container: &gtk::Box,
    model: &gtk::gio::MenuModel,
    index: i32,
    depth: usize,
    close: &Rc<dyn Fn()>,
    local_actions: Option<(&str, &gtk::gio::ActionGroup)>,
) {
    let label = item_label(model, index).unwrap_or_default();

    if let Some(submenu) = model.item_link(index, gtk::gio::MENU_LINK_SUBMENU) {
        // Nested submenus are rendered inline (a header plus indented items)
        // rather than as cascading native popups.
        let header = gtk::Label::new(Some(&strip_mnemonic(&label)));
        header.set_xalign(0.0);
        header.add_css_class(HEADER_CSS_CLASS);
        header.set_margin_start(8 + depth as i32 * 12);
        container.append(&header);
        append_model(container, &submenu, depth + 1, close, local_actions);
        return;
    }

    let text = gtk::Label::new(Some(&label));
    text.set_use_underline(true);
    text.set_xalign(0.0);
    text.set_halign(gtk::Align::Fill);
    let button = gtk::Button::builder().child(&text).build();
    // Actionable still owns sensitivity, targets and activation. Present the
    // current check/radio state when an app/window menu is opened, too.
    let name = model
        .item_attribute_value(index, "action", None)
        .and_then(|v| v.get::<String>());
    let target = model.item_attribute_value(index, "target", None);
    let original_label = label.clone();
    let local_actions = local_actions.map(|(name, group)| (name.to_owned(), group.clone()));
    let weak_text = text.downgrade();
    button.connect_map(move |button| {
        let Some(window) = button.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let Some((prefix, name)) = name.as_deref().and_then(|name| name.split_once('.')) else {
            return;
        };
        let group: Option<gtk::gio::ActionGroup> = match prefix {
            "app" => window.application().map(|app| app.upcast()),
            "win" => window
                .clone()
                .downcast::<gtk::ApplicationWindow>()
                .ok()
                .map(|window| window.upcast()),
            _ => local_actions
                .as_ref()
                .filter(|(name, _)| name == prefix)
                .map(|(_, group)| group.clone()),
        };
        let state = group.and_then(|group| group.action_state(name));
        let checked = state.as_ref().map(|state| {
            target.as_ref().map_or_else(
                || state.get::<bool>().unwrap_or(false),
                |target| target == state,
            )
        });
        if let Some(text) = weak_text.upgrade() {
            text.set_label(&match checked {
                Some(true) => format!("✓ {original_label}"),
                Some(false) => format!("  {original_label}"),
                None => original_label.clone(),
            });
        }
    });
    button.add_css_class("flat");
    button.add_css_class(ITEM_CSS_CLASS);
    button.set_halign(gtk::Align::Fill);
    button.set_hexpand(true);
    match model
        .item_attribute_value(index, "action", None)
        .and_then(|value| value.get::<String>())
    {
        Some(action) => {
            button.set_action_name(Some(action.as_str()));
            if let Some(target) = model.item_attribute_value(index, "target", None) {
                button.set_action_target_value(Some(&target));
            }
        }
        None => button.set_sensitive(false),
    }
    {
        let close = close.clone();
        let action_name = model
            .item_attribute_value(index, "action", None)
            .and_then(|value| value.get::<String>());
        button.connect_clicked(move |_| {
            if std::env::var_os("RUZU_MENU_DEBUG").is_some() {
                eprintln!("inline_menu: activate {action_name:?}");
            }
            // Close on the next cycle: GTK resolves the item's action after the
            // clicked handlers, and unparenting the panel first would make the
            // action (inserted on the panel) unresolvable.
            let close = close.clone();
            gtk::glib::idle_add_local_once(move || close());
        });
    }
    container.append(&button);
}

// Active in-window popup (context menu / choice list) and the windows that
// already carry its dismiss controllers.
thread_local! {
    static ACTIVE_POPUP: RefCell<Option<(gtk::Overlay, gtk::Box)>> = const { RefCell::new(None) };
    static POPUP_WINDOWS: RefCell<Vec<gtk::glib::WeakRef<gtk::Window>>> =
        const { RefCell::new(Vec::new()) };
}

/// Marks the overlay that hosts menu panels inside a given toplevel.
pub(crate) const OVERLAY_CSS_CLASS: &str = "ruzu-menu-overlay";

// Callback that hides the embedded render window while a menu is open: in X11
// a child window always paints above its parent, so an in-window menu would be
// covered by the game surface.
thread_local! {
    static RENDER_SUPPRESSOR: RefCell<Option<Rc<dyn Fn(bool)>>> = const { RefCell::new(None) };
    static RENDER_SUPPRESSED: Cell<bool> = const { Cell::new(false) };
    static ACTIVE_MENUBAR: RefCell<std::rc::Weak<Inner>> =
        RefCell::new(std::rc::Weak::<Inner>::new());
}

/// Registers the callback used to hide/show the embedded render window.
pub(crate) fn set_render_window_suppressor(suppressor: Rc<dyn Fn(bool)>) {
    RENDER_SUPPRESSOR.with(|slot| *slot.borrow_mut() = Some(suppressor));
}

fn any_menu_open() -> bool {
    let menubar_open = ACTIVE_MENUBAR
        .with(|weak| weak.borrow().upgrade())
        .is_some_and(|inner| inner.open.get().is_some());
    let popup_open = ACTIVE_POPUP.with(|slot| slot.borrow().is_some());
    menubar_open || popup_open
}

pub(crate) fn render_suppressed() -> bool {
    RENDER_SUPPRESSED.with(Cell::get)
}

/// Keeps the render window hidden exactly while at least one menu is open.
fn sync_render_suppression() {
    let want = any_menu_open();
    RENDER_SUPPRESSED.with(|state| {
        if state.get() == want {
            return;
        }
        state.set(want);
        let suppressor = RENDER_SUPPRESSOR.with(|slot| slot.borrow().clone());
        if let Some(suppressor) = suppressor {
            suppressor(want);
        }
    });
}

fn find_overlay(root: &gtk::Widget) -> Option<gtk::Overlay> {
    if let Some(overlay) = root.downcast_ref::<gtk::Overlay>() {
        if overlay.has_css_class(OVERLAY_CSS_CLASS) {
            return Some(overlay.clone());
        }
    }
    let mut child = root.first_child();
    while let Some(current) = child {
        if let Some(found) = find_overlay(&current) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

/// Returns (creating it if needed) the panel overlay of `anchor`'s own
/// toplevel. The panel must live in the same window as its control, otherwise
/// it would appear in another window and its coordinates would be wrong.
fn overlay_for(anchor: &gtk::Widget) -> Option<gtk::Overlay> {
    let root = anchor.root()?;
    let window = root.downcast::<gtk::Window>().ok()?;
    if let Some(overlay) = find_overlay(window.upcast_ref()) {
        return Some(overlay);
    }
    let overlay = gtk::Overlay::new();
    overlay.add_css_class(OVERLAY_CSS_CLASS);
    // Fill the toplevel, otherwise the overlay has no allocation and its
    // panels would be clipped away.
    overlay.set_hexpand(true);
    overlay.set_vexpand(true);
    if window.is::<gtk::Dialog>() {
        // Never restructure a dialog: moving its content into a new container
        // made some dialogs render empty. Dialog menus fall back to no panel.
        return None;
    }
    let child = window.child();
    window.set_child(gtk::Widget::NONE);
    overlay.set_child(child.as_ref());
    window.set_child(Some(&overlay));
    Some(overlay)
}

pub(crate) fn hide_context_menu() {
    let active = ACTIVE_POPUP.with(|slot| slot.borrow_mut().take());
    if let Some((overlay, panel)) = active {
        panel.set_visible(false);
        overlay.remove_overlay(&panel);
    }
    sync_render_suppression();
}

/// Presents a menu model as an in-window panel at `anchor`-relative `(x, y)`
/// instead of a native popover (see the module header for why).
pub(crate) fn show_context_menu(
    anchor: &gtk::Widget,
    group_name: &str,
    model: &gtk::gio::MenuModel,
    actions: &gtk::gio::ActionGroup,
    x: f64,
    y: f64,
) {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.insert_action_group(group_name, Some(actions));
    let close: Rc<dyn Fn()> = Rc::new(hide_context_menu);
    append_model(&panel, model, 0, &close, Some((group_name, actions)));
    show_content(anchor, &panel, x, y);
}

pub(crate) fn show_content(anchor: &gtk::Widget, panel: &gtk::Box, x: f64, y: f64) {
    hide_context_menu();
    let Some(overlay) = overlay_for(anchor) else {
        return;
    };
    install_css();
    panel.add_css_class(PANEL_CSS_CLASS);
    panel.set_halign(gtk::Align::Start);
    panel.set_valign(gtk::Align::Start);
    overlay.add_overlay(panel);
    panel.set_visible(true);
    // Position on the next cycle: a freshly created overlay has no allocation
    // yet, so `compute_point` would fail and the panel would land at (0,0).
    {
        let overlay = overlay.clone();
        let panel = panel.clone();
        let anchor = anchor.clone();
        gtk::glib::idle_add_local_once(move || {
            place_panel(&overlay, &panel, &anchor, x, y);
        });
    }
    if std::env::var_os("RUZU_MENU_DEBUG").is_some() {
        eprintln!(
            "inline_menu: popup items={}",
            panel.observe_children().n_items()
        );
    }
    ACTIVE_POPUP.with(|slot| *slot.borrow_mut() = Some((overlay.clone(), panel.clone())));
    ensure_popup_controllers(&overlay);
    panel.child_focus(gtk::DirectionType::TabForward);
    sync_render_suppression();
}

/// Fallback before GTK has allocated a freshly inserted overlay.
fn offset_to_root(widget: &gtk::Widget) -> (f64, f64) {
    let (mut x, mut y) = (0.0_f64, 0.0_f64);
    let mut current = widget.clone();
    while let Some(parent) = current.parent() {
        let allocation = current.allocation();
        x += allocation.x() as f64;
        y += allocation.y() as f64;
        current = parent;
    }
    (x, y)
}

/// Places `panel` under `anchor` inside `overlay`, clamped to the window.
fn place_panel(overlay: &gtk::Overlay, panel: &gtk::Box, anchor: &gtk::Widget, x: f64, y: f64) {
    // Account for GTK transforms when allocated; use accumulated allocations
    // only while a newly inserted overlay has not received its first layout.
    let overlay_offset = offset_to_root(overlay.upcast_ref());
    let anchor_offset = offset_to_root(anchor);
    let point = anchor.compute_point(overlay, &gtk::graphene::Point::new(x as f32, y as f32));
    let mut px = point
        .as_ref()
        .map_or(anchor_offset.0 - overlay_offset.0 + x, |p| p.x() as f64);
    let mut py = point
        .as_ref()
        .map_or(anchor_offset.1 - overlay_offset.1 + y, |p| p.y() as f64);
    let target_width = overlay.width() as f64;
    let target_height = overlay.height() as f64;
    let natural = panel.preferred_size().1;
    let panel_width = natural.width() as f64;
    let panel_height = natural.height() as f64;
    if target_width > 0.0 && px + panel_width > target_width {
        px = (target_width - panel_width).max(0.0);
    }
    if target_height > 0.0 && py + panel_height > target_height {
        py = (target_height - panel_height).max(0.0);
    }
    panel.set_margin_start(px.max(0.0) as i32);
    panel.set_margin_top(py.max(0.0) as i32);
    if std::env::var_os("RUZU_MENU_DEBUG").is_some() {
        eprintln!("inline_menu: place ({px},{py})");
    }
}

/// Dismiss the popup on Escape and on a click outside it, per toplevel.
fn ensure_popup_controllers(overlay: &gtk::Overlay) {
    let Some(root) = overlay.root() else { return };
    let Ok(window) = root.downcast::<gtk::Window>() else {
        return;
    };
    let already = POPUP_WINDOWS.with(|list| {
        let mut list = list.borrow_mut();
        list.retain(|weak| weak.upgrade().is_some());
        if list
            .iter()
            .any(|weak| weak.upgrade().as_ref() == Some(&window))
        {
            return true;
        }
        list.push(window.downgrade());
        false
    });
    if already {
        return;
    }

    let key = gtk::EventControllerKey::new();
    let weak_window = window.downgrade();
    key.connect_key_pressed(move |_, keyval, _, _| {
        use super::controller_navigation::NavigationKey as Key;
        let key = match keyval {
            gtk::gdk::Key::Escape => Key::Escape,
            gtk::gdk::Key::Up => Key::Up,
            gtk::gdk::Key::Down => Key::Down,
            _ => return gtk::glib::Propagation::Proceed,
        };
        if weak_window
            .upgrade()
            .is_some_and(|window| navigate(&window, key))
        {
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    window.add_controller(key);

    let click = gtk::GestureClick::new();
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let target = window.downgrade();
        click.connect_pressed(move |gesture, _, x, y| {
            let Some(target) = target.upgrade() else {
                return;
            };
            let point = gtk::graphene::Point::new(x as f32, y as f32);
            let inside = ACTIVE_POPUP.with(|slot| {
                slot.borrow().as_ref().is_some_and(|(_, panel)| {
                    panel
                        .compute_bounds(&target)
                        .is_some_and(|bounds| bounds.contains_point(&point))
                })
            });
            if inside {
                // Release the sequence so the panel button still gets the click.
                gesture.set_state(gtk::EventSequenceState::Denied);
                return;
            }
            hide_context_menu();
        });
    }
    window.add_controller(click);
    window.connect_unmap(|_| hide_context_menu());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an isolated X11 GTK process; run alone"]
    fn inline_menus_keep_actions_models_focus_and_surface() {
        std::env::set_var("RUZU_INLINE_MENUS", "1");
        gtk::init().unwrap();
        assert!(enabled());
        let app = gtk::Application::builder()
            .application_id("org.ruzu.InlineMenuTest")
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let invoked = Rc::new(Cell::new(0));
        let action = gtk::gio::SimpleAction::new("probe", None);
        let count = invoked.clone();
        action.connect_activate(move |_, _| count.set(count.get() + 1));
        app.add_action(&action);
        let section = gtk::gio::Menu::new();
        section.append(Some("First"), Some("app.probe"));
        let items = gtk::gio::Menu::new();
        items.append_section(None, &section);
        items.append(Some("Second"), Some("app.probe"));
        let menu = gtk::gio::Menu::new();
        menu.append_submenu(Some("_File"), &items);
        menu.append_submenu(Some("_Tools"), &items);
        let bar = InlineMenuBar::new(menu.upcast_ref());
        let overlay = gtk::Overlay::new();
        overlay.add_css_class(OVERLAY_CSS_CLASS);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(bar.row());
        let anchor = gtk::Button::with_label("Anchor");
        content.append(&anchor);
        overlay.set_child(Some(&content));
        let window = gtk::ApplicationWindow::builder()
            .application(&app)
            .default_width(600)
            .default_height(400)
            .child(&overlay)
            .build();
        bar.attach_to(&overlay);
        window.present();
        let settle = || {
            for _ in 0..20 {
                while gtk::glib::MainContext::default().pending() {
                    gtk::glib::MainContext::default().iteration(false);
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        settle();
        assert!(focus_bar(window.upcast_ref()));
        assert_eq!(gtk::prelude::GtkWindowExt::focus(&window),
            Some(bar.inner.buttons.borrow()[0].clone().upcast()));
        assert_eq!(bar.inner.open.get(), None);
        open_inner(&bar.inner, 0);
        settle();
        let panel = bar.inner.panels.borrow()[0].clone();
        assert_eq!(panel.native(), Some(window.clone().upcast()));
        let scroll = panel
            .first_child()
            .and_downcast::<gtk::ScrolledWindow>()
            .unwrap();
        let entries = scroll
            .child()
            .and_downcast::<gtk::Viewport>()
            .unwrap()
            .child()
            .unwrap();
        let first = entries.first_child().and_downcast::<gtk::Button>().unwrap();
        let first_label = first.child().and_downcast::<gtk::Label>().unwrap();
        assert_eq!(
            first_label.label(),
            "First",
            "sections must not be reordered after direct entries"
        );
        action.set_enabled(false);
        assert!(!first.is_sensitive());
        action.set_enabled(true);
        first.emit_clicked();
        settle();
        assert_eq!(invoked.get(), 1);
        assert_eq!(bar.inner.open.get(), None);
        assert!(navigate(
            window.upcast_ref(),
            super::super::controller_navigation::NavigationKey::Menu
        ));
        assert!(navigate(
            window.upcast_ref(),
            super::super::controller_navigation::NavigationKey::Escape
        ));
        let tools_button = bar.inner.buttons.borrow()[1].clone();
        tools_button.grab_focus();
        tools_button.emit_clicked();
        settle();
        assert_eq!(bar.inner.open.get(), Some(1));
        assert!(navigate(
            window.upcast_ref(),
            super::super::controller_navigation::NavigationKey::Escape
        ));
        settle();
        assert_eq!(bar.inner.open.get(), None);
        assert_eq!(gtk::prelude::GtkWindowExt::focus(&window),
            Some(tools_button.upcast()), "cancel must return to the originating menu");
        assert_eq!(invoked.get(), 1, "cancel must not activate an entry");
        let local = gtk::gio::SimpleActionGroup::new();
        local.add_action(&action);
        let flag = gtk::gio::SimpleAction::new_stateful("flag", None, &true.to_variant());
        local.add_action(&flag);
        let context = gtk::gio::Menu::new();
        context.append(Some("Local"), Some("local.probe"));
        context.append(Some("Flag"), Some("local.flag"));
        show_context_menu(
            anchor.upcast_ref(),
            "local",
            context.upcast_ref(),
            local.upcast_ref(),
            0.0,
            0.0,
        );
        settle();
        let (_, panel) = ACTIVE_POPUP.with(|slot| slot.borrow().clone()).unwrap();
        assert_eq!(panel.native(), Some(window.clone().upcast()));
        let flag_label = panel
            .first_child()
            .unwrap()
            .next_sibling()
            .unwrap()
            .first_child()
            .and_downcast::<gtk::Label>()
            .unwrap();
        assert_eq!(flag_label.label(), "✓ Flag");
        panel
            .first_child()
            .and_downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        settle();
        assert_eq!(
            invoked.get(),
            2,
            "local action must resolve before panel removal"
        );
        assert!(ACTIVE_POPUP.with(|slot| slot.borrow().is_none()));
        // Original selection and sensitivity remain authoritative.
        let dropdown = gtk::DropDown::from_strings(&["One", "Two"]);
        let replacement = crate::configuration::shared_widget::popup_safe_dropdown(&dropdown);
        content.append(&replacement);
        let choice_button = replacement
            .first_child()
            .unwrap()
            .next_sibling()
            .and_downcast::<gtk::Button>()
            .unwrap();
        dropdown.set_selected(1);
        assert_eq!(choice_button.label().as_deref(), Some("Two"));
        dropdown.set_sensitive(false);
        assert!(!choice_button.is_sensitive());
        dropdown.set_sensitive(true);
        choice_button.emit_clicked();
        let scroll = choice_button
            .next_sibling()
            .and_downcast::<gtk::ScrolledWindow>()
            .unwrap();
        let list = scroll
            .child()
            .and_downcast::<gtk::Viewport>()
            .unwrap()
            .child()
            .unwrap();
        list.first_child()
            .and_downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        assert_eq!(dropdown.selected(), 0);
        assert!(!scroll.is_visible());
        // Assert this window's focus target, not desktop activation: another
        // application may remain active while this isolated display test runs.
        assert_eq!(gtk::prelude::GtkWindowExt::focus(&window), Some(choice_button.clone().upcast()));
        dropdown.set_model(Some(&gtk::StringList::new(&["New one", "New two"])));
        dropdown.set_selected(1);
        assert_eq!(choice_button.label().as_deref(), Some("New two"));
        let combo = gtk::ComboBoxText::new();
        combo.append_text("First profile");
        combo.append_text("Second profile");
        let replacement = crate::configuration::shared_widget::popup_safe_combo(&combo);
        content.append(&replacement);
        let button = replacement
            .first_child()
            .unwrap()
            .next_sibling()
            .and_downcast::<gtk::Button>()
            .unwrap();
        combo.set_active(Some(1));
        assert_eq!(button.label().as_deref(), Some("Second profile"));
        combo.set_sensitive(false);
        assert!(!button.is_sensitive());
        window.destroy();
        settle();
        assert!(!render_suppressed());
    }

    #[test]
    fn strip_mnemonic_handles_escaped_underscore() {
        assert_eq!(strip_mnemonic("_File"), "File");
        assert_eq!(strip_mnemonic("Load _Folder..."), "Load Folder...");
        assert_eq!(strip_mnemonic("Free__Demo"), "Free_Demo");
    }
}
#[test]
fn policy_keeps_normal_desktops_and_non_x11_backends_native() {
    assert!(!popup_policy(true, None, "GNOME", false));
    assert!(!popup_policy(false, Some("1"), "gamescope", true));
    assert!(popup_policy(true, None, "gamescope", false));
    assert!(popup_policy(true, None, "GNOME", true));
    assert!(popup_policy(true, Some("1"), "GNOME", false));
    assert!(!popup_policy(true, Some("0"), "gamescope", true));
}
