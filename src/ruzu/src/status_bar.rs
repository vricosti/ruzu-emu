// SPDX-License-Identifier: GPL-3.0-or-later
//
// Bottom status bar — counterpart of the permanent status widgets upstream
// `GMainWindow` builds in `main.cpp` (`renderer_status_button`,
// `gpu_accuracy_button`, `dock_status_button`, `filter_status_button`,
// `aa_status_button`, and `volume_button`), together with the
// `UpdateAPIText` / `UpdateGPUAccuracyButton` / `UpdateFilterText` /
// `UpdateAAText` / `UpdateDockedButton` / `UpdateVolumeUI` refreshers and the
// `OnToggle*` click handlers.
//
// Each button shows a `Settings` value and *writes it back* when clicked,
// cycling through the same sequence upstream does. The colours come from
// upstream's own stylesheet — see [`css`] below.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};

use common::settings;
use common::settings_enums::{
    AntiAliasing, ConsoleMode, GpuAccuracy, RendererBackend, ScalingFilter,
};
use input_common::drivers::tas_input::{TasState, PLAYER_NUMBER};
use ruzu_core::perf_stats::PerfStatsResults;

/// The status bar and handles to the value buttons so they can be refreshed.
pub struct StatusBar {
    root: gtk::Box,
    renderer: gtk::Button,
    accuracy: gtk::Button,
    dock: gtk::Button,
    filter: gtk::Button,
    aa: gtk::Button,
    volume: gtk::Button,
    tas_state: gtk::Label,
    shader_building: gtk::Label,
    res_scale: gtk::Label,
    game_fps: gtk::Label,
    frame_time: gtk::Label,
    /// Invoked only by upstream `OnToggleGpuAccuracy`, whose left-click path
    /// calls `system->ApplySettings()` after changing the setting. The context
    /// menu deliberately does not invoke it because Eden's direct menu action
    /// only updates the value and button text.
    on_gpu_accuracy_changed: RefCell<Option<Box<dyn Fn()>>>,
    /// Mirrors `MainWindow::emulation_running` for the renderer button. Eden
    /// disables that complete button (left click and context menu) while a
    /// title is active because the graphics API cannot be changed live.
    emulation_running: Cell<bool>,
}

type StatusMenuAction = (String, Box<dyn Fn(&StatusBar) -> bool>);

impl StatusBar {
    pub fn new() -> Rc<Self> {
        install_css();

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        root.add_css_class("ruzu-statusbar");
        root.set_margin_start(4);
        root.set_margin_end(4);

        // Left-aligned status buttons. Upstream inserts each with
        // `insertPermanentWidget(0, …)`, so the *last* inserted ends up
        // leftmost: renderer, accuracy, dock, filter, AA, volume.
        let renderer = status_button(class::RENDERER);
        let accuracy = status_button(class::GPU);
        let dock = status_button(class::DOCKING);
        let filter = status_button(class::TOGGLABLE);
        let aa = status_button(class::TOGGLABLE);
        let volume = status_button(class::TOGGLABLE);
        for b in [&renderer, &accuracy, &dock, &filter, &aa, &volume] {
            root.append(b);
        }

        // Right side: message label (upstream `message_label`, stretch), then
        // the performance labels updated by `GMainWindow::UpdateStatusBar`.
        let message = gtk::Label::new(None);
        message.set_hexpand(true);
        root.append(&message);

        let tas_state = performance_label("Current TAS playback or recording state");
        let shader_building = performance_label("The amount of shaders currently being built");
        let res_scale = performance_label("The current selected resolution scaling multiplier.");
        let game_fps =
            performance_label("How many frames per second the game is currently displaying.");
        let frame_time = performance_label(
            "Time taken to emulate a Switch frame, excluding frame limiting and v-sync.",
        );
        for label in [
            &tas_state,
            &shader_building,
            &res_scale,
            &game_fps,
            &frame_time,
        ] {
            root.append(label);
        }

        let bar = Rc::new(Self {
            root,
            renderer,
            accuracy,
            dock,
            filter,
            aa,
            volume,
            tas_state,
            shader_building,
            res_scale,
            game_fps,
            frame_time,
            on_gpu_accuracy_changed: RefCell::new(None),
            emulation_running: Cell::new(false),
        });

        bar.connect_actions();
        bar.refresh();
        bar
    }

    /// Register upstream `OnToggleGpuAccuracy`'s live-apply callback.
    pub fn connect_gpu_accuracy_changed(&self, f: impl Fn() + 'static) {
        *self.on_gpu_accuracy_changed.borrow_mut() = Some(Box::new(f));
    }

    /// The widget to place at the bottom of the window.
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// Wire each button to the upstream `OnToggle*` behaviour.
    fn connect_actions(self: &Rc<Self>) {
        macro_rules! on_click {
            ($button:expr, $handler:ident) => {{
                let bar = Rc::clone(self);
                $button.connect_clicked(move |_| {
                    log::debug!("status bar: {} clicked", stringify!($handler));
                    bar.$handler();
                    bar.finish_setting_change();
                });
            }};
        }

        on_click!(self.renderer, on_toggle_graphics_api);
        on_click!(self.dock, on_toggle_docked_mode);
        on_click!(self.filter, on_toggle_adapting_filter);
        on_click!(self.aa, on_toggle_anti_aliasing);
        on_click!(self.volume, on_toggle_mute);

        {
            let bar = Rc::clone(self);
            self.accuracy.connect_clicked(move |_| {
                log::debug!("status bar: on_toggle_gpu_accuracy clicked");
                bar.on_toggle_gpu_accuracy();
                bar.finish_setting_change();
                if let Some(callback) = bar.on_gpu_accuracy_changed.borrow().as_ref() {
                    callback();
                }
            });
        }

        self.install_context_menu(&self.renderer, StatusBar::renderer_context_actions);
        self.install_context_menu(&self.accuracy, StatusBar::accuracy_context_actions);
        self.install_context_menu(&self.dock, StatusBar::dock_context_actions);
        self.install_context_menu(&self.filter, StatusBar::filter_context_actions);
        self.install_context_menu(&self.aa, StatusBar::aa_context_actions);
        self.install_context_menu(&self.volume, StatusBar::volume_context_actions);
    }

    fn finish_setting_change(&self) {
        self.refresh();
    }

    /// Keep the renderer selector locked exactly when Eden disables
    /// `renderer_status_button` in `BootGame`/`OnEmulationStopped`.
    pub fn set_emulation_running(&self, running: bool) {
        self.emulation_running.set(running);
        self.renderer.set_sensitive(!running);
    }

    /// GTK counterpart of `Qt::CustomContextMenu` on each status button.
    fn install_context_menu(
        self: &Rc<Self>,
        button: &gtk::Button,
        actions: fn(&StatusBar) -> Vec<StatusMenuAction>,
    ) {
        let gesture = gtk::GestureClick::new();
        gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
        let bar = Rc::downgrade(self);
        let anchor = button.downgrade();
        gesture.connect_pressed(move |gesture, _, x, y| {
            let (Some(bar), Some(anchor)) = (bar.upgrade(), anchor.upgrade()) else {
                return;
            };
            bar.show_context_menu(anchor.upcast_ref(), x, y, actions(&bar));
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });
        button.add_controller(gesture);
    }

    fn show_context_menu(
        self: &Rc<Self>,
        anchor: &gtk::Widget,
        x: f64,
        y: f64,
        actions: Vec<StatusMenuAction>,
    ) {
        let menu = gio::Menu::new();
        let action_group = gio::SimpleActionGroup::new();
        for (index, (label, callback)) in actions.into_iter().enumerate() {
            let name = format!("choice-{index}");
            let action = gio::SimpleAction::new(&name, None);
            let bar = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                let Some(bar) = bar.upgrade() else { return };
                if callback(&bar) {
                    bar.finish_setting_change();
                }
            });
            action_group.add_action(&action);
            menu.append(
                Some(&crate::i18n::tr(&label)),
                Some(&format!("status.{name}")),
            );
        }

        let popover = gtk::PopoverMenu::from_model(Option::<&gio::Menu>::None);
        popover.add_css_class("ruzu-context-menu");
        popover.set_has_arrow(false);
        // Eden's QMenu automatically flips above the status-bar button when
        // opening below would place it under the window/screen edge. GTK's
        // default Popover placement prefers the bottom, so select the matching
        // status-bar direction explicitly.
        popover.set_position(gtk::PositionType::Top);
        popover.insert_action_group("status", Some(&action_group));
        popover.set_parent(anchor);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.set_menu_model(Some(&menu));
        popover.connect_closed(|popover| {
            let popover = popover.clone();
            glib::idle_add_local_once(move || popover.unparent());
        });
        popover.popup();
    }

    fn renderer_context_actions(&self) -> Vec<StatusMenuAction> {
        renderer_context_choices(self.emulation_running.get())
            .into_iter()
            .map(|(backend, label)| {
                (
                    label.to_string(),
                    Box::new(move |_: &StatusBar| {
                        settings::values_mut().renderer_backend.set_value(backend);
                        true
                    }) as Box<dyn Fn(&StatusBar) -> bool>,
                )
            })
            .collect()
    }

    fn accuracy_context_actions(&self) -> Vec<StatusMenuAction> {
        crate::configuration::shared_translation::STATUS_GPU_ACCURACY
            .iter()
            .map(|(accuracy, label)| {
                let accuracy = *accuracy;
                (
                    (*label).to_string(),
                    Box::new(move |_: &StatusBar| {
                        settings::values_mut().gpu_accuracy.set_value(accuracy);
                        true
                    }) as Box<dyn Fn(&StatusBar) -> bool>,
                )
            })
            .collect()
    }

    fn dock_context_actions(&self) -> Vec<StatusMenuAction> {
        crate::configuration::shared_translation::STATUS_CONSOLE_MODE
            .iter()
            .map(|(mode, label)| {
                let mode = *mode;
                (
                    (*label).to_string(),
                    Box::new(move |bar: &StatusBar| {
                        if *settings::values().use_docked_mode.get_value() == mode {
                            return false;
                        }
                        bar.on_toggle_docked_mode();
                        true
                    }) as Box<dyn Fn(&StatusBar) -> bool>,
                )
            })
            .collect()
    }

    fn filter_context_actions(&self) -> Vec<StatusMenuAction> {
        crate::configuration::shared_translation::STATUS_SCALING_FILTER
            .iter()
            .map(|(filter, label)| {
                let filter = *filter;
                (
                    (*label).to_string(),
                    Box::new(move |_: &StatusBar| {
                        settings::values_mut().scaling_filter.set_value(filter);
                        true
                    }) as Box<dyn Fn(&StatusBar) -> bool>,
                )
            })
            .collect()
    }

    fn aa_context_actions(&self) -> Vec<StatusMenuAction> {
        crate::configuration::shared_translation::STATUS_ANTI_ALIASING
            .iter()
            .map(|(anti_aliasing, label)| {
                let anti_aliasing = *anti_aliasing;
                (
                    (*label).to_string(),
                    Box::new(move |_: &StatusBar| {
                        settings::values_mut()
                            .anti_aliasing
                            .set_value(anti_aliasing);
                        true
                    }) as Box<dyn Fn(&StatusBar) -> bool>,
                )
            })
            .collect()
    }

    fn volume_context_actions(&self) -> Vec<StatusMenuAction> {
        let muted = *settings::values().audio_muted.get_value();
        vec![
            (
                if muted { "Unmute" } else { "Mute" }.to_string(),
                Box::new(|bar: &StatusBar| {
                    bar.on_toggle_mute();
                    true
                }),
            ),
            (
                "Reset Volume".to_string(),
                Box::new(|_: &StatusBar| {
                    settings::values_mut().volume.set_value(100);
                    true
                }),
            ),
        ]
    }

    /// Upstream `GMainWindow::OnToggleGraphicsAPI`.
    fn on_toggle_graphics_api(&self) {
        let mut values = settings::values_mut();
        let api = next_graphics_api(*values.renderer_backend.get_value());
        values.renderer_backend.set_value(api);
    }

    /// Upstream `GMainWindow::OnToggleGpuAccuracy`: High ⇄ Low.
    fn on_toggle_gpu_accuracy(&self) {
        let mut values = settings::values_mut();
        let accuracy = match *values.gpu_accuracy.get_value() {
            GpuAccuracy::High => GpuAccuracy::Low,
            GpuAccuracy::Low => GpuAccuracy::High,
        };
        values.gpu_accuracy.set_value(accuracy);
    }

    /// Upstream `GMainWindow::OnToggleDockedMode`.
    ///
    /// Upstream additionally disconnects a handheld controller and warns, which
    /// needs `HIDCore`; that is not reachable from the launcher yet, so only the
    /// console-mode flip is performed here.
    fn on_toggle_docked_mode(&self) {
        let mut values = settings::values_mut();
        let mode = match *values.use_docked_mode.get_value() {
            ConsoleMode::Docked => ConsoleMode::Handheld,
            ConsoleMode::Handheld => ConsoleMode::Docked,
        };
        values.use_docked_mode.set_value(mode);
    }

    /// Upstream `GMainWindow::OnToggleAdaptingFilter`: advance one step,
    /// wrapping past `EnumMetadata<ScalingFilter>::GetLast()`.
    fn on_toggle_adapting_filter(&self) {
        let mut values = settings::values_mut();
        let next = next_wrapping(
            *values.scaling_filter.get_value() as u32,
            ScalingFilter::SgsrEdge as u32,
        );
        if let Some(filter) = ScalingFilter::from_u32(next) {
            values.scaling_filter.set_value(filter);
        }
    }

    /// Upstream's `aa_status_button` click handler: advance one step, wrapping
    /// past `EnumMetadata<AntiAliasing>::GetLast()`.
    fn on_toggle_anti_aliasing(&self) {
        let mut values = settings::values_mut();
        let next = next_wrapping(
            *values.anti_aliasing.get_value() as u32,
            AntiAliasing::Smaa as u32,
        );
        if let Some(aa) = AntiAliasing::from_u32(next) {
            values.anti_aliasing.set_value(aa);
        }
    }

    /// Upstream exposes mute on the volume button's context menu; a plain click
    /// opens a volume slider popup. Without that popup ported yet, the click
    /// toggles mute, which is the one action the button's own checked state
    /// already reflects (`UpdateVolumeUI` shows "VOLUME: MUTE" when muted).
    fn on_toggle_mute(&self) {
        let mut values = settings::values_mut();
        let muted = *values.audio_muted.get_value();
        values.audio_muted.set_value(!muted);
    }

    /// Re-read the settings and update every label and colour state — upstream
    /// `UpdateStatusButtons` plus the individual `Update*` refreshers.
    pub fn refresh(&self) {
        let values = settings::values();

        // `UpdateAPIText`: the fused backend value names the OpenGL shader API.
        let backend = *values.renderer_backend.get_value();
        let renderer = match backend {
            RendererBackend::OpenGlGlsl => "OPENGL GLSL".to_string(),
            RendererBackend::OpenGlGlasm => "OPENGL GLASM".to_string(),
            RendererBackend::OpenGlSpirV => "OPENGL SPIRV".to_string(),
            RendererBackend::Vulkan => "VULKAN".to_string(),
            RendererBackend::Metal => "METAL".to_string(),
            RendererBackend::Null => "NULL".to_string(),
        };
        self.renderer.set_label(&renderer);
        // `renderer_status_button->setChecked(api == Vulkan)` — checked renders
        // orange, unchecked blue. Metal follows the accelerated native API
        // presentation rather than the OpenGL presentation.
        set_checked(
            &self.renderer,
            matches!(backend, RendererBackend::Vulkan | RendererBackend::Metal),
        );

        // `UpdateGPUAccuracyButton`.
        let accuracy = *values.gpu_accuracy.get_value();
        let accuracy_label = crate::configuration::shared_translation::GPU_ACCURACY
            .iter()
            .find_map(|(value, label)| (*value == accuracy).then_some(*label))
            .unwrap_or("Unknown")
            .to_uppercase();
        self.accuracy.set_label(&accuracy_label);
        set_checked(&self.accuracy, accuracy == GpuAccuracy::High);

        // `UpdateDockedButton`.
        let console_mode = *values.use_docked_mode.get_value();
        self.dock.set_label(match console_mode {
            ConsoleMode::Docked => "DOCKED",
            ConsoleMode::Handheld => "HANDHELD",
        });
        set_checked(&self.dock, console_mode == ConsoleMode::Docked);

        // `UpdateFilterText` uses the short status-bar map from
        // `qt_common/config/shared_translation.h`, not the longer settings-row
        // descriptions used by the configuration dialog.
        self.filter
            .set_label(match *values.scaling_filter.get_value() {
                ScalingFilter::NearestNeighbor => "NEAREST",
                ScalingFilter::Bilinear => "BILINEAR",
                ScalingFilter::Bicubic => "BICUBIC",
                ScalingFilter::ZeroTangent => "ZERO-TANGENT",
                ScalingFilter::BSpline => "B-SPLINE",
                ScalingFilter::Mitchell => "MITCHELL",
                ScalingFilter::Spline1 => "SPLINE-1",
                ScalingFilter::Gaussian => "GAUSSIAN",
                ScalingFilter::Lanczos => "LANCZOS",
                ScalingFilter::ScaleForce => "SCALEFORCE",
                ScalingFilter::Fsr => "FSR",
                ScalingFilter::Area => "AREA",
                ScalingFilter::Mmpx => "MMPX",
                ScalingFilter::Sgsr => "SGSR",
                ScalingFilter::SgsrEdge => "SGSR EDGEDIR",
            });
        // Upstream keeps the filter button permanently checked.
        set_checked(&self.filter, true);

        // `UpdateAAText`.
        self.aa.set_label(match *values.anti_aliasing.get_value() {
            AntiAliasing::None => "NO AA",
            AntiAliasing::Fxaa => "FXAA",
            AntiAliasing::Smaa => "SMAA",
        });
        set_checked(&self.aa, true);

        // `UpdateVolumeUI`.
        let muted = *values.audio_muted.get_value();
        if muted {
            self.volume.set_label(&crate::i18n::tr("VOLUME: MUTE"));
        } else {
            self.volume.set_label(&crate::i18n::tr_args(
                "VOLUME: %1%",
                &[values.volume.get_value().to_string()],
            ));
        }
        set_checked(&self.volume, !muted);
    }

    /// Update the permanent performance labels from the latest engine sample.
    ///
    /// This is the GTK counterpart of `GMainWindow::UpdateStatusBar`.
    pub fn update_performance(
        &self,
        results: Option<PerfStatsResults>,
        shaders_building: Option<i32>,
    ) {
        let Some(results) = results else {
            for label in [
                &self.shader_building,
                &self.res_scale,
                &self.game_fps,
                &self.frame_time,
            ] {
                label.set_visible(false);
            }
            return;
        };

        if let Some(count) = shaders_building.filter(|count| *count > 0) {
            self.shader_building
                .set_label(&format_shaders_building(count));
            self.shader_building.set_visible(true);
        } else {
            self.shader_building.set_visible(false);
        }

        let values = settings::values();
        self.res_scale
            .set_label(&format_resolution_scale(values.resolution_info.up_factor));
        self.game_fps.set_label(&format_game_fps(
            results.average_game_fps,
            !*values.use_speed_limit.get_value(),
        ));
        self.frame_time
            .set_label(&format_frame_time(results.frametime));

        for label in [&self.res_scale, &self.game_fps, &self.frame_time] {
            label.set_visible(true);
        }
    }

    /// Update upstream's `tas_label` from `GMainWindow::GetTasStateDescription`.
    pub fn update_tas(&self, status: Option<(TasState, usize, [usize; PLAYER_NUMBER])>) {
        if !*settings::values().tas_enable.get_value() {
            self.tas_state.set_visible(false);
            return;
        }
        let Some((state, current_frame, frame_counts)) = status else {
            self.tas_state.set_visible(false);
            return;
        };

        self.tas_state
            .set_label(&format_tas_state(state, current_frame, frame_counts));
        self.tas_state.set_visible(true);
    }
}

fn renderer_context_choices(running: bool) -> Vec<(RendererBackend, &'static str)> {
    if running {
        return Vec::new();
    }
    crate::configuration::shared_translation::STATUS_RENDERER_BACKEND
        .iter()
        .copied()
        .filter(|(backend, _)| *backend != RendererBackend::Null)
        .collect()
}

/// The exact switch in upstream `GMainWindow::OnToggleGraphicsAPI`.
fn next_graphics_api(api: RendererBackend) -> RendererBackend {
    match api {
        RendererBackend::Vulkan => RendererBackend::OpenGlGlsl,
        RendererBackend::Metal => RendererBackend::Vulkan,
        RendererBackend::OpenGlGlsl => RendererBackend::OpenGlGlsl,
        RendererBackend::OpenGlSpirV => RendererBackend::OpenGlGlasm,
        RendererBackend::OpenGlGlasm => RendererBackend::Null,
        RendererBackend::Null => RendererBackend::Vulkan,
    }
}

fn create_tas_frames_string(frames: [usize; PLAYER_NUMBER]) -> String {
    let Some(last_player) = frames.iter().rposition(|frames| *frames != 0) else {
        return String::new();
    };
    frames[..=last_player]
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_tas_state(
    state: TasState,
    current_frame: usize,
    frames: [usize; PLAYER_NUMBER],
) -> String {
    let frame_counts = create_tas_frames_string(frames);
    match state {
        TasState::Running => crate::i18n::tr_args(
            "TAS state: Running %1/%2",
            &[current_frame.to_string(), frame_counts],
        ),
        TasState::Recording => {
            crate::i18n::tr_args("TAS state: Recording %1", &[frames[0].to_string()])
        }
        TasState::Stopped => crate::i18n::tr_args(
            "TAS state: Idle %1/%2",
            &[current_frame.to_string(), frame_counts],
        ),
    }
}

fn format_resolution_scale(up_factor: f32) -> String {
    let scale = if up_factor.fract().abs() < f32::EPSILON {
        format!("{up_factor:.0}")
    } else {
        format!("{up_factor:.2}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    };
    crate::i18n::tr_args("Scale: %1x", &[scale])
}

fn format_shaders_building(count: i32) -> String {
    let suffix = if count == 1 { "shader" } else { "shaders" };
    format!(
        "{} {count} {}",
        crate::i18n::tr("Building:"),
        crate::i18n::tr(suffix)
    )
}

fn format_game_fps(average_game_fps: f64, unlocked: bool) -> String {
    let fps = format!("{:.0}", average_game_fps.round());
    crate::i18n::tr_args(
        if unlocked {
            "Game: %1 FPS (Unlocked)"
        } else {
            "Game: %1 FPS"
        },
        &[fps],
    )
}

fn format_frame_time(frametime_seconds: f64) -> String {
    crate::i18n::tr_args(
        "Frame: %1 ms",
        &[format!("{:.2}", frametime_seconds * 1000.0)],
    )
}

/// Next enum discriminant, wrapping back to 0 after `last`.
///
/// Mirrors upstream's `static_cast<Enum>(static_cast<u32>(value) + 1)` followed
/// by a comparison with `EnumMetadata<Enum>::GetLast()`.
fn next_wrapping(current: u32, last: u32) -> u32 {
    let next = current + 1;
    if next > last {
        0
    } else {
        next
    }
}

/// CSS classes standing in for upstream's `objectName`s, which is how its
/// stylesheet targets each button.
mod class {
    pub const TOGGLABLE: &str = "ruzu-status-togglable";
    pub const RENDERER: &str = "ruzu-status-renderer";
    pub const GPU: &str = "ruzu-status-gpu";
    pub const DOCKING: &str = "ruzu-status-docking";
}

/// Qt's `:checked` pseudo-state, which the stylesheet colours against.
const CHECKED_CLASS: &str = "ruzu-status-checked";

fn set_checked(button: &gtk::Button, checked: bool) {
    if checked {
        button.add_css_class(CHECKED_CLASS);
    } else {
        button.remove_css_class(CHECKED_CLASS);
    }
}

/// A flat status-bar button, matching yuzu's `QPushButton` status widgets
/// (borderless, compact).
fn status_button(class: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.add_css_class(class);
    button.set_has_frame(false);
    // Upstream assigns Qt::NoFocus to every status-bar button. The render
    // surface owns keyboard input while emulation is active.
    button.set_can_focus(false);
    button.set_focus_on_click(false);
    button
}

fn performance_label(tooltip: &str) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_tooltip_text(Some(&crate::i18n::tr(tooltip)));
    label.set_margin_start(4);
    label.set_margin_end(4);
    label.set_visible(false);
    label
}

/// Install the status-bar styling once.
///
/// The colours are upstream's, from
/// `zuyu/dist/qt_themes/default/style.qss` — the stylesheet yuzu's default
/// theme loads. Qt's `:checked` / `:!checked` pseudo-states become the
/// [`CHECKED_CLASS`] marker here:
///
/// ```qss
/// QPushButton#RendererStatusBarButton:checked  { color: #e85c00; }  /* Vulkan */
/// QPushButton#RendererStatusBarButton:!checked { color: #0066ff; }  /* OpenGL */
/// QPushButton#GPUStatusBarButton:checked       { color: #b06020; }
/// QPushButton#GPUStatusBarButton:!checked      { color: #109010; }
/// QPushButton#TogglableStatusBarButton         { color: #959595; }
/// QPushButton#TogglableStatusBarButton:checked { color: #000000; }
/// QPushButton#DockingStatusBarButton           { color: #000000; }
/// ```
///
/// Note the docking button has no `:checked` rule upstream — it is always
/// rendered in the base colour, whichever console mode is active.
fn install_css() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else {
            return;
        };
        let provider = gtk::CssProvider::new();
        provider.load_from_data(&format!(
            ".ruzu-statusbar {{ padding: 0 2px; min-height: 0; }}\
             .ruzu-statusbar button {{ padding: 2px 6px; min-height: 0; min-width: 0;\
                 border: 1px solid transparent; box-shadow: none; background: none;\
                 font-size: 11px; }}\
             .ruzu-statusbar button:hover {{ border: 1px solid #76797C; }}\
             .ruzu-statusbar label {{ font-size: 11px; }}\
             .{togglable} {{ color: #959595; }}\
             .{togglable}.{checked} {{ color: #000000; }}\
             .{renderer} {{ color: #0066ff; }}\
             .{renderer}.{checked} {{ color: #e85c00; }}\
             .{gpu} {{ color: #109010; }}\
             .{gpu}.{checked} {{ color: #b06020; }}\
             .{docking} {{ color: #000000; }}",
            togglable = class::TOGGLABLE,
            renderer = class::RENDERER,
            gpu = class::GPU,
            docking = class::DOCKING,
            checked = CHECKED_CLASS,
        ));
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphics_api_toggle_matches_upstream_switch() {
        assert_eq!(
            next_graphics_api(RendererBackend::Vulkan),
            RendererBackend::OpenGlGlsl
        );
        assert_eq!(
            next_graphics_api(RendererBackend::Metal),
            RendererBackend::Vulkan
        );
        assert_eq!(
            next_graphics_api(RendererBackend::OpenGlGlsl),
            RendererBackend::OpenGlGlsl
        );
        assert_eq!(
            next_graphics_api(RendererBackend::OpenGlSpirV),
            RendererBackend::OpenGlGlasm
        );
        assert_eq!(
            next_graphics_api(RendererBackend::OpenGlGlasm),
            RendererBackend::Null
        );
        assert_eq!(
            next_graphics_api(RendererBackend::Null),
            RendererBackend::Vulkan
        );
    }

    #[test]
    fn renderer_context_menu_preserves_runtime_lock_and_adds_metal_on_macos() {
        let mut expected = vec![
            (RendererBackend::OpenGlGlsl, "OpenGL GLSL"),
            (RendererBackend::Vulkan, "Vulkan"),
        ];
        #[cfg(target_os = "macos")]
        expected.push((RendererBackend::Metal, "Metal"));
        expected.extend([
            (RendererBackend::OpenGlGlasm, "OpenGL GLASM"),
            (RendererBackend::OpenGlSpirV, "OpenGL SPIRV"),
        ]);
        assert_eq!(renderer_context_choices(false), expected);
        assert!(renderer_context_choices(true).is_empty());
    }

    #[test]
    fn filter_cycle_wraps_past_the_last_real_value() {
        let last = ScalingFilter::SgsrEdge as u32;
        assert_eq!(next_wrapping(last, last), 0);
        assert_eq!(
            ScalingFilter::from_u32(next_wrapping(last, last)),
            Some(ScalingFilter::NearestNeighbor)
        );
    }

    #[test]
    fn filter_cycle_advances_one_step() {
        let last = ScalingFilter::SgsrEdge as u32;
        assert_eq!(
            ScalingFilter::from_u32(next_wrapping(ScalingFilter::NearestNeighbor as u32, last)),
            Some(ScalingFilter::Bilinear)
        );
    }

    #[test]
    fn anti_aliasing_cycle_wraps_to_none() {
        let last = AntiAliasing::Smaa as u32;
        assert_eq!(
            AntiAliasing::from_u32(next_wrapping(AntiAliasing::Smaa as u32, last)),
            Some(AntiAliasing::None)
        );
    }

    #[test]
    fn performance_text_matches_upstream_status_bar() {
        assert_eq!(format_shaders_building(1), "Building: 1 shader");
        assert_eq!(format_shaders_building(3), "Building: 3 shaders");
        assert_eq!(format_resolution_scale(1.0), "Scale: 1x");
        assert_eq!(format_resolution_scale(1.5), "Scale: 1.5x");
        assert_eq!(format_game_fps(59.4, false), "Game: 59 FPS");
        assert_eq!(format_game_fps(59.5, true), "Game: 60 FPS (Unlocked)");
        assert_eq!(format_frame_time(1.0 / 60.0), "Frame: 16.67 ms");
    }

    #[test]
    fn tas_frame_counts_preserve_empty_player_slots() {
        let mut frames = [0usize; PLAYER_NUMBER];
        frames[1] = 120;
        frames[3] = 40;

        assert_eq!(create_tas_frames_string(frames), "0, 120, 0, 40");
        assert_eq!(
            format_tas_state(TasState::Running, 12, frames),
            "TAS state: Running 12/0, 120, 0, 40"
        );
    }
}
