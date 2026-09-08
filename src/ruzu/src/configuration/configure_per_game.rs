// SPDX-FileCopyrightText: Copyright 2020 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! GTK counterpart of Eden `yuzu/configuration/configure_per_game.{h,cpp,ui}`.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use frontend_common::config::{BaseConfig, ConfigType};
use gtk::prelude::*;
use gtk::{gdk, glib};

use super::configure_dialog::Page;
use super::{
    configure_applets, configure_audio, configure_cpu, configure_graphics,
    configure_graphics_advanced, configure_graphics_extensions, configure_input_per_game,
    configure_network, configure_per_game_addons, configure_system, qt_config,
};

// The Eden Properties captures are 928x790 including the Linux window frame.
// Mutter adds 28x66 pixels around this GTK client on the same reference
// desktop, so these client dimensions reproduce the upstream geometry.
const DEFAULT_WIDTH: i32 = 900;
const DEFAULT_HEIGHT: i32 = 724;
const INFO_PANEL_WIDTH: i32 = 280;
const ICON_SIZE: i32 = 256;

const PER_GAME_CATEGORIES: [common::settings_enums::Category; 16] = [
    common::settings_enums::Category::Controls,
    common::settings_enums::Category::Core,
    common::settings_enums::Category::Cpu,
    common::settings_enums::Category::CpuDebug,
    common::settings_enums::Category::CpuUnsafe,
    common::settings_enums::Category::Linux,
    common::settings_enums::Category::Renderer,
    common::settings_enums::Category::RendererAdvanced,
    common::settings_enums::Category::RendererHacks,
    common::settings_enums::Category::RendererExtensions,
    common::settings_enums::Category::RendererDebug,
    common::settings_enums::Category::Audio,
    common::settings_enums::Category::System,
    common::settings_enums::Category::SystemAudio,
    common::settings_enums::Category::Network,
    common::settings_enums::Category::LibraryApplet,
];

/// Metadata displayed by the permanent left-side Info panel.
#[derive(Clone)]
pub struct GameProperties {
    pub name: String,
    pub developer: String,
    pub version: String,
    pub title_id: u64,
    pub format: String,
    pub size: String,
    pub filename: String,
    pub path: PathBuf,
    pub icon: Option<gdk::Texture>,
}

type SettingState = BTreeMap<(u32, String), (bool, String)>;

/// Upstream `ConfigurePerGame`.
pub struct ConfigurePerGame {
    window: gtk::Window,
    pages: Vec<Page>,
    config: RefCell<BaseConfig>,
    config_path: PathBuf,
    finalized: Cell<bool>,
}

impl ConfigurePerGame {
    pub fn new(
        parent: Option<&gtk::Window>,
        properties: GameProperties,
        hid_core: Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>,
        runtime_lock: bool,
    ) -> Rc<Self> {
        install_properties_css();
        common::settings::set_configuring_global(false);

        let config_path = custom_config_path(properties.title_id, &properties.path);
        let mut config = BaseConfig::new(ConfigType::PerGameConfig);
        config.initialize(&config_path);
        qt_config::load_per_game_control_values(&config_path);

        let advanced_graphics = configure_graphics_advanced::page(runtime_lock);
        let graphics =
            configure_graphics::page(advanced_graphics.expose_compute_option, runtime_lock);
        let pages = vec![
            configure_per_game_addons::page(properties.title_id, &properties.path),
            configure_system::page(runtime_lock),
            configure_cpu::page(runtime_lock),
            graphics,
            advanced_graphics.page,
            configure_graphics_extensions::page(runtime_lock),
            configure_audio::page(runtime_lock),
            configure_input_per_game::page(hid_core),
            configure_network::page(),
            configure_applets::page(runtime_lock),
        ];

        let window = gtk::Window::builder()
            .title("Properties")
            .modal(true)
            .default_width(DEFAULT_WIDTH)
            .default_height(DEFAULT_HEIGHT)
            .build();
        window.add_css_class("ruzu-properties");
        if let Some(parent) = parent {
            window.set_transient_for(Some(parent));
        }

        let notebook = gtk::Notebook::new();
        notebook.add_css_class("ruzu-properties-tabs");
        notebook.set_hexpand(true);
        notebook.set_vexpand(true);
        notebook.set_scrollable(true);
        for page in &pages {
            notebook.append_page(&page.widget, Some(&gtk::Label::new(Some(&page.title))));
        }

        let body = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        body.set_margin_top(8);
        body.set_margin_start(2);
        body.set_margin_end(2);
        body.append(&info_panel(&properties));
        body.append(&notebook);

        let status = gtk::Label::new(Some(
            "Some settings are only available when a game is not running.",
        ));
        status.set_xalign(0.0);
        status.set_hexpand(true);
        let cancel = dialog_button("window-close-symbolic", "Cancel", "ruzu-properties-cancel");
        let ok = dialog_button("emblem-ok-symbolic", "OK", "ruzu-properties-ok");

        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        footer.set_margin_top(8);
        footer.set_margin_bottom(8);
        footer.set_margin_start(2);
        footer.set_margin_end(8);
        footer.append(&status);
        footer.append(&cancel);
        footer.append(&ok);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&body);
        root.append(&footer);
        window.set_child(Some(&root));

        let this = Rc::new(Self {
            window,
            pages,
            config: RefCell::new(config),
            config_path,
            finalized: Cell::new(false),
        });

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
                dialog.window.close();
            }
        ));
        this.window.connect_close_request(glib::clone!(
            #[weak(rename_to = dialog)]
            this,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_| {
                dialog.restore_global_configuration();
                glib::Propagation::Proceed
            }
        ));

        this
    }

    pub fn present(&self) {
        crate::i18n::translate_widget_tree(&self.window);
        self.window.present();
        // Upstream gives the dialog itself `Qt::ClickFocus`; no read-only Info
        // field receives the orange keyboard-focus ring on first presentation.
        let window = self.window.clone();
        glib::idle_add_local_once(move || {
            gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>)
        });
    }

    pub fn connect_closed(&self, callback: impl Fn() + 'static) {
        // `gtk_window_close()` tears down the native surface while a Rust
        // wrapper can remain alive. Waiting for `Widget::destroy` therefore
        // leaves `GameListView::property_dialog` pointing at an unusable
        // window, and a second Properties action tries to present it again.
        // Upstream's modal QDialog is discarded as soon as it closes, so drop
        // the frontend owner from the close request itself.
        self.window.connect_close_request(move |_| {
            callback();
            glib::Propagation::Proceed
        });
    }

    fn apply_configuration(&self) {
        if self.finalized.get() {
            return;
        }

        let setting_state = prepare_custom_settings();
        for page in &self.pages {
            (page.apply)();
        }
        {
            use common::settings_enums::ConsoleMode;
            use common::settings_input::ControllerType;

            let mut values = common::settings::values_mut();
            if common::settings::is_docked_mode(&values)
                && values.players.get_value()[0].controller_type == ControllerType::Handheld
            {
                values.use_docked_mode.set_value(ConsoleMode::Handheld);
                values.use_docked_mode.set_global(true);
            }
        }
        preserve_global_selections(&setting_state);

        let result = (|| -> std::io::Result<()> {
            let mut config = self.config.borrow_mut();
            config.save_values();
            config.write_to_ini()?;
            qt_config::save_per_game_control_values(&self.config_path)
        })();
        if let Err(error) = result {
            log::error!("Failed to save per-game configuration: {error}");
            crate::gtk_compat::show_warning(
                Some(&self.window),
                "Properties",
                "The custom game configuration could not be saved.",
            );
            return;
        }

        self.restore_global_configuration();
    }

    fn restore_global_configuration(&self) {
        if self.finalized.replace(true) {
            return;
        }
        {
            let mut values = common::settings::values_mut();
            common::settings::restore_global_state(&mut values, false);
            values.players.set_global(true);
        }
        common::settings::set_configuring_global(true);
    }
}

fn install_properties_css() {
    use std::sync::Once;

    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Some(display) = gdk::Display::default() else {
            return;
        };
        let provider = gtk::CssProvider::new();
        provider.load_from_data(
            "notebook.ruzu-properties-tabs > header tab {\
                 min-width: 0;\
                 padding-left: 7px;\
                 padding-right: 7px;\
             }\
             window.ruzu-properties frame {\
                 border-radius: 0;\
             }\
             window.ruzu-properties entry,\
             window.ruzu-properties spinbutton,\
             window.ruzu-properties dropdown > button {\
                 min-height: 20px;\
                 padding-top: 0;\
                 padding-bottom: 0;\
                 border-radius: 2px;\
             }\
             window.ruzu-properties .ruzu-properties-table-header {\
                 background-color: shade(@theme_bg_color, 0.98);\
                 border-bottom: 1px solid @borders;\
                 padding: 2px;\
             }\
             window.ruzu-properties .ruzu-properties-cancel image {\
                 color: #b02020;\
             }\
             window.ruzu-properties .ruzu-properties-ok image {\
                 color: #3b7f3b;\
             }",
        );
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
}

fn dialog_button(icon_name: &str, label: &str, css_class: &str) -> gtk::Button {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    content.append(&gtk::Image::from_icon_name(icon_name));
    content.append(&gtk::Label::new(Some(label)));
    let button = gtk::Button::new();
    button.add_css_class(css_class);
    button.set_child(Some(&content));
    button
}

fn custom_config_path(title_id: u64, game_path: &Path) -> PathBuf {
    let filename = if title_id == 0 {
        game_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("game")
            .to_string()
    } else {
        format!("{title_id:016X}")
    };
    common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::ConfigDir)
        .join("custom")
        .join(format!("{filename}.ini"))
}

fn prepare_custom_settings() -> SettingState {
    let mut state = SettingState::new();
    let mut values = common::settings::values_mut();
    for category in PER_GAME_CATEGORIES {
        values.for_each_setting_in_category_mut(category, |setting| {
            if !setting.switchable() {
                return;
            }
            let key = (category as u32, setting.label().to_string());
            let was_global = setting.using_global();
            let global_value = setting.to_string_global();
            state.insert(key, (was_global, global_value.clone()));
            if was_global {
                setting.set_global(false);
                setting.load_string(&global_value);
            }
        });
    }
    state
}

fn preserve_global_selections(state: &SettingState) {
    let mut values = common::settings::values_mut();
    for category in PER_GAME_CATEGORIES {
        values.for_each_setting_in_category_mut(category, |setting| {
            if !setting.switchable() {
                return;
            }
            let key = (category as u32, setting.label().to_string());
            let Some((was_global, global_value)) = state.get(&key) else {
                return;
            };
            if *was_global && setting.to_string_repr() == *global_value {
                setting.set_global(true);
            }
        });
    }
}

fn info_panel(properties: &GameProperties) -> gtk::Frame {
    let frame = gtk::Frame::new(Some("Info"));
    frame.set_width_request(INFO_PANEL_WIDTH);
    frame.set_hexpand(false);
    // Upstream's group box fills the dialog body while its inner vertical
    // spacer absorbs height changes. The icon and metadata grid remain fixed.
    frame.set_vexpand(true);
    frame.set_halign(gtk::Align::Start);
    frame.set_valign(gtk::Align::Fill);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_margin_top(8);
    content.set_margin_bottom(8);
    content.set_margin_start(8);
    content.set_margin_end(8);

    let picture = gtk::Picture::new();
    picture.set_size_request(ICON_SIZE, ICON_SIZE);
    picture.set_hexpand(false);
    picture.set_vexpand(false);
    picture.set_halign(gtk::Align::Start);
    picture.set_valign(gtk::Align::Start);
    picture.set_can_shrink(false);
    picture.set_keep_aspect_ratio(false);
    picture.set_paintable(properties.icon.as_ref());
    content.append(&picture);

    let grid = gtk::Grid::new();
    grid.set_row_spacing(6);
    grid.set_column_spacing(6);
    grid.set_hexpand(false);
    grid.set_halign(gtk::Align::Start);
    let title_id = format!("{:016X}", properties.title_id);
    for (row, (label, value)) in [
        ("Name", properties.name.as_str()),
        ("Developer", properties.developer.as_str()),
        ("Version", properties.version.as_str()),
        ("Title ID", title_id.as_str()),
        ("Format", properties.format.as_str()),
        ("Size", properties.size.as_str()),
        ("Filename", properties.filename.as_str()),
    ]
    .into_iter()
    .enumerate()
    {
        let caption = gtk::Label::new(Some(label));
        caption.set_xalign(0.0);
        caption.set_width_chars(9);
        let entry = gtk::Entry::new();
        entry.set_text(value);
        entry.set_editable(false);
        entry.set_width_chars(20);
        entry.set_max_width_chars(20);
        entry.set_hexpand(false);
        grid.attach(&caption, 0, row as i32, 1, 1);
        grid.attach(&entry, 1, row as i32, 1, 1);
    }
    content.append(&grid);
    frame.set_child(Some(&content));
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_config_uses_title_id_or_filename_like_upstream() {
        let title = custom_config_path(0x0100_1234_5678_9000, Path::new("ignored.nsp"));
        assert!(title.ends_with("custom/0100123456789000.ini"));

        let homebrew = custom_config_path(0, Path::new("/games/sample.nro"));
        assert!(homebrew.ends_with("custom/sample.nro.ini"));
    }
}
