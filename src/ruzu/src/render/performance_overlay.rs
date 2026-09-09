// SPDX-License-Identifier: GPL-3.0-or-later
// GTK counterpart of yuzu/render/performance_overlay.{h,cpp,ui}.

use gtk::prelude::*;
use ruzu_core::perf_stats::PerfStatsResults;
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

const NUM_FPS_SAMPLES: usize = 120;
const NUM_FRAMETIME_SAMPLES: usize = 300;

/// Mechanical separation of UpdateStats' sample storage from GTK widgets for
/// deterministic tests. Both remain owned by the upstream overlay module.
#[derive(Default)]
struct Samples {
    fps: VecDeque<f64>,
    frametime: VecDeque<f64>,
    x_pos: u64,
}

impl Samples {
    fn update(&mut self, results: &PerfStatsResults) {
        let fps = results.average_game_fps;
        if !fps.is_nan() && fps > 3.0 {
            self.fps.push_back(fps);
            self.x_pos += 1;
            if self.fps.len() > NUM_FPS_SAMPLES {
                self.fps.pop_front();
            }
        }
        let ft_ms = results.frametime * 1000.0;
        if !ft_ms.is_nan() && ft_ms <= 500.0 {
            self.frametime.push_back(ft_ms);
            if self.frametime.len() > NUM_FRAMETIME_SAMPLES {
                self.frametime.pop_front();
            }
        }
    }
}

fn recent_average(samples: &VecDeque<f64>) -> Option<f64> {
    if samples.len() < 2 {
        return None;
    }
    // Preserve upstream's exclusion of the first sample until the window fills.
    let count = 10.min(samples.len() - 1);
    Some(samples.iter().skip(samples.len() - count).sum::<f64>() / count as f64)
}

pub struct PerformanceOverlay {
    window: gtk::Window,
    samples: Rc<RefCell<Samples>>,
    fps: [gtk::Label; 4],
    frametime: [gtk::Label; 4],
    chart: gtk::DrawingArea,
}

impl PerformanceOverlay {
    pub fn new(parent: &gtk::ApplicationWindow, closed: impl Fn() + 'static) -> Self {
        static CSS: std::sync::Once = std::sync::Once::new();
        CSS.call_once(|| {
            let provider = gtk::CssProvider::new();
            provider.load_from_data("window.ruzu-performance-overlay { background: rgba(127,127,127,0.745); border-radius: 10px; color: white; } window.ruzu-performance-overlay label { color: white; }");
            gtk::style_context_add_provider_for_display(&gtk::prelude::WidgetExt::display(parent), &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        });
        // A separate transient surface stays above the native render child.
        // GTK/Wayland leave absolute placement to the compositor, unlike Qt's
        // resetPosition(main.pos + offset). Native window dragging is retained.
        let window = gtk::Window::builder()
            .transient_for(parent)
            .decorated(false)
            .resizable(false)
            .default_width(280)
            .default_height(260)
            .build();
        window.add_css_class("ruzu-performance-overlay");
        crate::hotkeys::install_secondary_window_shortcuts(&window, parent);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_margin_top(10);
        content.set_margin_bottom(10);
        content.set_margin_start(10);
        content.set_margin_end(10);
        let columns = gtk::Box::new(gtk::Orientation::Horizontal, 20);
        let make_column = |title: &str, initial: &str| {
            let column = gtk::Box::new(gtk::Orientation::Vertical, 2);
            column.append(&gtk::Label::new(Some(&crate::i18n::tr(title))));
            let labels = [initial, "Min: 0", "Max: 0", "Avg: 0"]
                .map(|text| gtk::Label::new(Some(&crate::i18n::tr(text))));
            for label in &labels {
                column.append(label);
            }
            columns.append(&column);
            labels
        };
        let frametime = make_column("Frametime", "0 ms");
        let fps = make_column("FPS", "0 fps");
        content.append(&columns);
        let chart = gtk::DrawingArea::builder()
            .content_width(260)
            .content_height(100)
            .build();
        let samples = Rc::new(RefCell::new(Samples::default()));
        chart.set_draw_func({
            let samples = Rc::clone(&samples);
            move |_, cr, width, height| {
                cr.set_source_rgb(0.0, 0.0, 0.0);
                let _ = cr.paint();
                let samples = samples.borrow();
                let maximum = samples.fps.iter().copied().fold(0.0, f64::max);
                if maximum <= 0.0 || !maximum.is_finite() {
                    return;
                }
                let min_x = (samples.x_pos as f64 - NUM_FPS_SAMPLES as f64).max(0.0);
                let max_x = (samples.x_pos as f64).max(10.0);
                let plot_width = (width - 32).max(1) as f64;
                let plot_height = (height - 10).max(1) as f64;
                cr.set_font_size(10.0);
                for tick in 0..3 {
                    let y = 5.0 + plot_height * tick as f64 / 2.0;
                    cr.set_source_rgb(0.2, 0.2, 0.2);
                    cr.set_line_width(1.0);
                    cr.move_to(30.0, y);
                    cr.line_to(width as f64, y);
                    let _ = cr.stroke();
                    cr.set_source_rgb(1.0, 1.0, 1.0);
                    cr.move_to(1.0, y.clamp(10.0, height as f64 - 2.0));
                    let _ = cr.show_text(&format!("{:.0}", maximum * (1.0 - tick as f64 / 2.0)));
                }
                cr.set_source_rgb(1.0, 0.0, 0.0);
                cr.set_line_width(2.0);
                for (i, fps) in samples.fps.iter().enumerate() {
                    let x = 30.0
                        + ((samples.x_pos - samples.fps.len() as u64) as f64 + i as f64 - min_x)
                            / (max_x - min_x)
                            * plot_width;
                    let y = 5.0 + plot_height * (1.0 - fps / maximum);
                    if i == 0 {
                        cr.move_to(x, y);
                    } else {
                        cr.line_to(x, y);
                    }
                }
                let _ = cr.stroke();
            }
        });
        content.append(&chart);
        let drag_handle = gtk::WindowHandle::new();
        drag_handle.set_child(Some(&content));
        window.set_child(Some(&drag_handle));
        window.connect_close_request(move |window| {
            window.set_visible(false);
            closed();
            gtk::glib::Propagation::Stop
        });
        Self {
            window,
            samples,
            fps,
            frametime,
            chart,
        }
    }

    pub fn set_visible(&self, visible: bool) {
        self.window.set_visible(visible);
    }

    pub fn update_stats(&self, results: &PerfStatsResults) {
        self.samples.borrow_mut().update(results);
        let samples = self.samples.borrow();
        for (labels, history, value, decimals, suffix) in [
            (
                &self.fps,
                &samples.fps,
                results.average_game_fps,
                0,
                "%1 fps",
            ),
            (
                &self.frametime,
                &samples.frametime,
                results.frametime * 1000.0,
                2,
                "%1 ms",
            ),
        ] {
            if value.is_nan() {
                continue;
            }
            let value = if decimals == 0 { value.round() } else { value };
            labels[0]
                .set_text(&crate::i18n::tr(suffix).replace("%1", &format!("{value:.decimals$}")));
            let stats_decimals = if decimals == 0 { 0 } else { 1 };
            let values = [
                history.iter().copied().reduce(f64::min),
                history.iter().copied().reduce(f64::max),
                recent_average(history),
            ];
            for ((label, value), format) in labels[1..]
                .iter()
                .zip(values)
                .zip(["Min: %1", "Max: %1", "Avg: %1"])
            {
                if let Some(value) = value {
                    label.set_text(
                        &crate::i18n::tr(format)
                            .replace("%1", &format!("{value:.stats_decimals$}")),
                    );
                }
            }
        }
        self.chart.queue_draw();
    }
}

impl Drop for PerformanceOverlay {
    fn drop(&mut self) {
        self.window.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires GTK display; run in an isolated process"]
    fn overlay_updates_labels_retains_nan_and_closes_without_destroying_history() {
        gtk::init().unwrap();
        let app = gtk::Application::builder()
            .application_id("org.ruzu.PerformanceOverlayTest")
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let parent = gtk::ApplicationWindow::builder().application(&app).build();
        let closed = Rc::new(std::cell::Cell::new(false));
        let overlay = PerformanceOverlay::new(&parent, {
            let closed = closed.clone();
            move || closed.set(true)
        });
        let mut stats = PerfStatsResults::default();
        stats.average_game_fps = 60.5;
        stats.frametime = 0.016;
        overlay.update_stats(&stats);
        assert_eq!(overlay.fps[0].text(), "61 fps");
        assert_eq!(overlay.frametime[0].text(), "16.00 ms");
        stats.average_game_fps = f64::NAN;
        stats.frametime = f64::NAN;
        overlay.update_stats(&stats);
        assert_eq!(overlay.fps[0].text(), "61 fps");
        assert_eq!(overlay.samples.borrow().fps.len(), 1);
        overlay.set_visible(true);
        assert!(overlay.window.is_visible());
        assert!(overlay.window.emit_by_name::<bool>("close-request", &[]));
        assert!(closed.get());
        assert!(!overlay.window.is_visible());
        overlay.set_visible(true);
        assert_eq!(overlay.samples.borrow().fps.len(), 1);
        drop(overlay);
        parent.destroy();
    }

    #[test]
    fn samples_preserve_upstream_thresholds_windows_and_average() {
        let mut samples = Samples::default();
        let mut stats = PerfStatsResults::default();
        stats.average_game_fps = 3.0;
        stats.frametime = 0.5;
        samples.update(&stats);
        assert!(samples.fps.is_empty());
        assert_eq!(samples.frametime.front(), Some(&500.0));
        stats.average_game_fps = f64::NAN;
        stats.frametime = f64::NAN;
        samples.update(&stats);
        assert_eq!(samples.frametime.len(), 1);
        for i in 0..310 {
            stats.average_game_fps = 10.0 + i as f64;
            stats.frametime = 0.01;
            samples.update(&stats);
        }
        assert_eq!(samples.fps.len(), 120);
        assert_eq!(samples.frametime.len(), 300);
        assert_eq!(samples.x_pos, 310);
        assert_eq!(samples.fps.front(), Some(&200.0));
        assert_eq!(recent_average(&VecDeque::from([10.0, 20.0])), Some(20.0));
        assert_eq!(recent_average(&VecDeque::from([10.0])), None);
        stats.average_game_fps = 0.0;
        stats.frametime = 0.501;
        samples.update(&stats);
        assert_eq!(samples.x_pos, 310);
        assert_eq!(samples.frametime.back(), Some(&10.0));
    }
}
