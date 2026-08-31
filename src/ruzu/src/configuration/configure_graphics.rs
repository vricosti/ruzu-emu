// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/eden/src/yuzu/configuration/configure_graphics.cpp`
// (`ConfigureGraphics`), whose widget tree lives in `configure_graphics.ui`.
//
// Two groups: "API Settings" (Eden's combined backend + Vulkan device) and
// "Graphics Settings" (the render options).
//
// Upstream shows the Vulkan device row only for Vulkan
// (`ConfigureGraphics::UpdateAPILayout`). The VSync combo is likewise rebuilt
// per backend by `PopulateVSyncModeSelection`, because each backend supports a
// different subset of the modes.

use std::cell::RefCell;
use std::rc::Rc;

use ash::vk;
use gtk::prelude::*;

use common::settings_enums::{RendererBackend, VSyncMode};

use super::configure_dialog::Page;
use super::shared_translation as tr;
use super::shared_widget as w;

/// Build the Graphics tab — upstream `ConfigureGraphics`.
pub fn page(expose_compute_option: impl Fn() + 'static, runtime_lock: bool) -> Page {
    let (scroller, column) = w::page();

    // --- "API Settings" ---------------------------------------------------
    let (api_group, api) = w::group("API Settings");

    let backend_value = *common::settings::values().renderer_backend.get_value();
    let (backend_row, backend) = w::combo_row(
        "API:",
        &tr::labels(tr::GRAPHICS_API),
        tr::index_of(tr::GRAPHICS_API, &backend_value),
    );
    api.append(&backend_row);

    // Vulkan physical-device picker, populated from the upstream-owned
    // `VkDeviceInfo::Record` counterpart.
    let mut device_records = Vec::new();
    crate::vk_device_info::populate_records(&mut device_records);
    for record in &device_records {
        if record.has_broken_compute {
            expose_compute_option();
        }
    }
    let device_records = Rc::new(device_records);
    let mut device_labels: Vec<String> = device_records
        .iter()
        .map(|record| record.name.clone())
        .collect();
    if device_labels.is_empty() {
        device_labels.push("Device 0".to_string());
    }
    let device_label_refs: Vec<&str> = device_labels.iter().map(String::as_str).collect();
    let selected_device = *common::settings::values().vulkan_device.get_value();
    let (device_row, device) = w::combo_row("Device:", &device_label_refs, selected_device);
    api.append(&device_row);

    apply_api_layout(backend_value, &device_row);
    let configuring_global = common::settings::is_configuring_global();
    let api_uses_global = common::settings::values().renderer_backend.using_global();
    device_row.set_sensitive(vulkan_device_sensitive(
        configuring_global,
        api_uses_global,
        runtime_lock,
    ));

    column.append(&api_group);

    // --- "Graphics Settings" ----------------------------------------------
    let (settings_group, settings) = w::group("Graphics Settings");

    let async_gpu = w::check_row(
        "Use asynchronous GPU emulation",
        *common::settings::values()
            .use_asynchronous_gpu_emulation
            .get_value(),
    );

    let initial_vsync_mode =
        setting_to_present_mode(*common::settings::values().vsync_mode.get_value());
    let initial_present_modes =
        present_modes_for(backend_value, selected_device as usize, &device_records);
    let initial_vsync_labels = present_mode_labels(&initial_present_modes, backend_value);
    let initial_vsync_refs: Vec<&str> = initial_vsync_labels.iter().map(String::as_str).collect();
    let initial_vsync_index = initial_present_modes
        .iter()
        .position(|mode| *mode == initial_vsync_mode)
        .unwrap_or(0) as u32;
    let (vsync_row, vsync) = w::combo_row("VSync Mode:", &initial_vsync_refs, initial_vsync_index);
    vsync.set_sensitive(backend_value != RendererBackend::Null);
    let vsync_modes = Rc::new(RefCell::new(initial_present_modes));

    let fullscreen_value = *common::settings::values().fullscreen_mode.get_value();
    let (fullscreen_row, fullscreen) = w::combo_row(
        "Fullscreen Mode:",
        &tr::labels(tr::FULLSCREEN_MODE),
        tr::index_of(tr::FULLSCREEN_MODE, &fullscreen_value),
    );

    let aspect_value = *common::settings::values().aspect_ratio.get_value();
    let (aspect_row, aspect) = w::combo_row(
        "Aspect Ratio:",
        &tr::labels(tr::ASPECT_RATIO),
        tr::index_of(tr::ASPECT_RATIO, &aspect_value),
    );

    let resolution_value = *common::settings::values().resolution_setup.get_value();
    let (resolution_row, resolution) = w::combo_row(
        "Resolution:",
        &tr::labels(tr::RESOLUTION_SETUP),
        tr::index_of(tr::RESOLUTION_SETUP, &resolution_value),
    );

    let filter_value = *common::settings::values().scaling_filter.get_value();
    let (filter_row, filter) = w::combo_row(
        "Window Adapting Filter:",
        &tr::labels(tr::SCALING_FILTER),
        tr::index_of(tr::SCALING_FILTER, &filter_value),
    );

    let aa_value = *common::settings::values().anti_aliasing.get_value();
    let (aa_row, aa) = w::combo_row(
        "Anti-Aliasing Method:",
        &tr::labels(tr::ANTI_ALIASING),
        tr::index_of(tr::ANTI_ALIASING, &aa_value),
    );

    let sharpness_value = *common::settings::values().fsr_sharpening_slider.get_value();
    // Eden keeps the raw 0..=200 setting on the slider, reverses its visual
    // direction, and presents `(200 - raw) * 0.5` as the percentage.
    let (sharpness_row, sharpness, _) =
        w::reversed_percent_slider_row("FSR Sharpness:", sharpness_value as f64, 0.0, 200.0, 0.5);

    let bg_color = gtk::ColorButton::with_rgba(&background_rgba());
    bg_color.set_halign(gtk::Align::Start);
    let bg_row = w::labeled_row("Background Color:", &bg_color);

    // `ConfigureGraphics::Setup` inserts the Renderer-category widgets in
    // setting-id order. Keep the resulting Eden order from the Properties
    // dialog rather than the unrelated declaration/construction order here.
    settings.append(&resolution_row);
    settings.append(&vsync_row);
    settings.append(&filter_row);
    settings.append(&sharpness_row);
    settings.append(&aspect_row);
    settings.append(&aa_row);
    settings.append(&async_gpu);
    settings.append(&fullscreen_row);
    settings.append(&bg_row);

    column.append(&settings_group);

    // Reveal the Vulkan device row only when the selected API is Vulkan.
    {
        let device_row = device_row.clone();
        let device = device.clone();
        let vsync = vsync.clone();
        let vsync_modes = Rc::clone(&vsync_modes);
        let device_records = Rc::clone(&device_records);
        backend.connect_selected_notify(move |combo| {
            let selected = tr::value_at(tr::GRAPHICS_API, combo.selected());
            apply_api_layout(selected, &device_row);
            device_row.set_sensitive(vulkan_device_sensitive(
                configuring_global,
                api_uses_global,
                runtime_lock,
            ));
            repopulate_vsync(
                &vsync,
                &vsync_modes,
                selected,
                device.selected() as usize,
                &device_records,
            );
        });
    }

    {
        let backend = backend.clone();
        let vsync = vsync.clone();
        let vsync_modes = Rc::clone(&vsync_modes);
        let device_records = Rc::clone(&device_records);
        device.connect_selected_notify(move |device| {
            let selected_backend = tr::value_at(tr::GRAPHICS_API, backend.selected());
            repopulate_vsync(
                &vsync,
                &vsync_modes,
                selected_backend,
                device.selected() as usize,
                &device_records,
            );
        });
    }

    Page::new("Graphics", scroller, move || {
        let backend_value = tr::value_at(tr::GRAPHICS_API, backend.selected());
        let device_index = device.selected();
        let async_value = async_gpu.is_active();
        let vsync_value = vsync_modes
            .borrow()
            .get(vsync.selected() as usize)
            .copied()
            .map(present_mode_to_setting);
        let fullscreen_value = tr::value_at(tr::FULLSCREEN_MODE, fullscreen.selected());
        let aspect_value = tr::value_at(tr::ASPECT_RATIO, aspect.selected());
        let resolution_value = tr::value_at(tr::RESOLUTION_SETUP, resolution.selected());
        let filter_value = tr::value_at(tr::SCALING_FILTER, filter.selected());
        let aa_value = tr::value_at(tr::ANTI_ALIASING, aa.selected());
        let sharpness_value = sharpness.value() as i32;
        let rgba = bg_color.rgba();

        let mut values = common::settings::values_mut();
        values.renderer_backend.set_value(backend_value);
        // Upstream `ConfigureGraphics::ApplyConfiguration` only publishes the
        // physical-device combobox while Vulkan is the selected backend. The
        // hidden row must not overwrite a stored Vulkan device when applying
        // an OpenGL or Null configuration.
        if updates_vulkan_device(backend_value) {
            values.vulkan_device.set_value(device_index);
        }
        values.use_asynchronous_gpu_emulation.set_value(async_value);
        if backend_value != RendererBackend::Null {
            if let Some(mode) = vsync_value {
                values.vsync_mode.set_value(mode);
            }
        }
        values.fullscreen_mode.set_value(fullscreen_value);
        values.aspect_ratio.set_value(aspect_value);
        values.resolution_setup.set_value(resolution_value);
        values.scaling_filter.set_value(filter_value);
        values.anti_aliasing.set_value(aa_value);
        values.fsr_sharpening_slider.set_value(sharpness_value);
        values.bg_red.set_value((rgba.red() * 255.0).round() as u8);
        values
            .bg_green
            .set_value((rgba.green() * 255.0).round() as u8);
        values
            .bg_blue
            .set_value((rgba.blue() * 255.0).round() as u8);
    })
}

/// Show the Vulkan device row only for Vulkan — upstream
/// `ConfigureGraphics::UpdateAPILayout`.
fn apply_api_layout(backend: RendererBackend, device_row: &gtk::Box) {
    device_row.set_visible(backend == RendererBackend::Vulkan);
}

/// Upstream `ConfigureGraphics::UpdateAPILayout` disables the physical-device
/// row only when a per-game configuration inherits the global renderer API.
/// Global configuration owns the value even though its setting reports that it
/// is using the global slot.
fn vulkan_device_sensitive(
    configuring_global: bool,
    api_uses_global: bool,
    runtime_lock: bool,
) -> bool {
    (configuring_global || !api_uses_global) && runtime_lock
}

/// The backend cases which enter the Vulkan-device branch of upstream
/// `ConfigureGraphics::ApplyConfiguration`.
fn updates_vulkan_device(backend: RendererBackend) -> bool {
    backend == RendererBackend::Vulkan
}

const DEFAULT_PRESENT_MODES: &[vk::PresentModeKHR] =
    &[vk::PresentModeKHR::IMMEDIATE, vk::PresentModeKHR::FIFO];

fn setting_to_present_mode(mode: VSyncMode) -> vk::PresentModeKHR {
    match mode {
        VSyncMode::Immediate => vk::PresentModeKHR::IMMEDIATE,
        VSyncMode::Mailbox => vk::PresentModeKHR::MAILBOX,
        VSyncMode::Fifo => vk::PresentModeKHR::FIFO,
        VSyncMode::FifoRelaxed => vk::PresentModeKHR::FIFO_RELAXED,
    }
}

fn present_mode_to_setting(mode: vk::PresentModeKHR) -> VSyncMode {
    match mode {
        vk::PresentModeKHR::IMMEDIATE => VSyncMode::Immediate,
        vk::PresentModeKHR::MAILBOX => VSyncMode::Mailbox,
        vk::PresentModeKHR::FIFO_RELAXED => VSyncMode::FifoRelaxed,
        _ => VSyncMode::Fifo,
    }
}

fn present_modes_for(
    backend: RendererBackend,
    device: usize,
    records: &[crate::vk_device_info::Record],
) -> Vec<vk::PresentModeKHR> {
    if backend == RendererBackend::Vulkan {
        if let Some(record) = records.get(device) {
            if !record.vsync_support.is_empty() {
                return record.vsync_support.clone();
            }
        }
    }
    DEFAULT_PRESENT_MODES.to_vec()
}

fn translate_present_mode(
    mode: vk::PresentModeKHR,
    backend: RendererBackend,
) -> Option<&'static str> {
    match mode {
        vk::PresentModeKHR::IMMEDIATE
            if matches!(
                backend,
                RendererBackend::OpenGlGlsl
                    | RendererBackend::OpenGlGlasm
                    | RendererBackend::OpenGlSpirV
            ) =>
        {
            Some("Off")
        }
        vk::PresentModeKHR::IMMEDIATE => Some("Immediate (VSync Off)"),
        vk::PresentModeKHR::MAILBOX => Some("Mailbox (Recommended)"),
        vk::PresentModeKHR::FIFO
            if matches!(
                backend,
                RendererBackend::OpenGlGlsl
                    | RendererBackend::OpenGlGlasm
                    | RendererBackend::OpenGlSpirV
            ) =>
        {
            Some("On")
        }
        vk::PresentModeKHR::FIFO => Some("FIFO (VSync On)"),
        vk::PresentModeKHR::FIFO_RELAXED => Some("FIFO Relaxed"),
        _ => None,
    }
}

fn present_mode_labels(modes: &[vk::PresentModeKHR], backend: RendererBackend) -> Vec<String> {
    modes
        .iter()
        .filter_map(|mode| translate_present_mode(*mode, backend))
        .map(crate::i18n::tr)
        .collect()
}

fn repopulate_vsync(
    dropdown: &gtk::DropDown,
    current_modes: &RefCell<Vec<vk::PresentModeKHR>>,
    backend: RendererBackend,
    device: usize,
    records: &[crate::vk_device_info::Record],
) {
    if backend == RendererBackend::Null {
        dropdown.set_sensitive(false);
        return;
    }
    dropdown.set_sensitive(true);

    let selected_mode = current_modes
        .borrow()
        .get(dropdown.selected() as usize)
        .copied()
        .unwrap_or(vk::PresentModeKHR::FIFO);
    let modes = present_modes_for(backend, device, records);
    let labels = present_mode_labels(&modes, backend);
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    dropdown.set_model(Some(&gtk::StringList::new(&label_refs)));
    dropdown.set_selected(
        modes
            .iter()
            .position(|mode| *mode == selected_mode)
            .unwrap_or(0) as u32,
    );
    *current_modes.borrow_mut() = modes;
}

/// The configured background colour as a GDK colour.
fn background_rgba() -> gtk::gdk::RGBA {
    let values = common::settings::values();
    gtk::gdk::RGBA::new(
        *values.bg_red.get_value() as f32 / 255.0,
        *values.bg_green.get_value() as f32 / 255.0,
        *values.bg_blue.get_value() as f32 / 255.0,
        1.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vsync_setting_present_mode_round_trip_matches_upstream() {
        for setting in [
            VSyncMode::Immediate,
            VSyncMode::Mailbox,
            VSyncMode::Fifo,
            VSyncMode::FifoRelaxed,
        ] {
            assert_eq!(
                present_mode_to_setting(setting_to_present_mode(setting)),
                setting
            );
        }
    }

    #[test]
    fn opengl_uses_the_two_default_present_modes_and_short_labels() {
        let modes = present_modes_for(RendererBackend::OpenGlGlsl, 0, &[]);
        assert_eq!(modes, DEFAULT_PRESENT_MODES);
        assert_eq!(
            present_mode_labels(&modes, RendererBackend::OpenGlGlsl),
            ["Off", "On"]
        );
    }

    #[test]
    fn graphics_api_rows_preserve_eden_and_expose_metal_only_on_macos() {
        let mut expected_labels = vec!["Vulkan"];
        let mut expected_values = vec![1];
        #[cfg(target_os = "macos")]
        {
            expected_labels.push("Metal");
            expected_values.push(5);
        }
        expected_labels.extend([
            "OpenGL GLSL",
            "OpenGL GLASM (Assembly Shaders, NVIDIA Only)",
            "OpenGL SPIR-V (Experimental, AMD/Mesa Only)",
            "Null",
        ]);
        expected_values.extend([0, 3, 4, 2]);
        assert_eq!(tr::labels(tr::GRAPHICS_API), expected_labels);
        assert_eq!(
            tr::GRAPHICS_API
                .iter()
                .map(|(value, _)| *value as u32)
                .collect::<Vec<_>>(),
            expected_values
        );
    }

    #[test]
    fn vulkan_uses_the_selected_devices_present_modes() {
        let records = [crate::vk_device_info::Record {
            name: "test".to_string(),
            vsync_support: vec![vk::PresentModeKHR::MAILBOX, vk::PresentModeKHR::FIFO],
            has_broken_compute: false,
        }];
        assert_eq!(
            present_modes_for(RendererBackend::Vulkan, 0, &records),
            records[0].vsync_support
        );
    }

    #[test]
    fn vulkan_device_sensitivity_matches_upstream_global_inheritance_rule() {
        assert!(vulkan_device_sensitive(true, true, true));
        assert!(vulkan_device_sensitive(true, false, true));
        assert!(!vulkan_device_sensitive(false, true, true));
        assert!(vulkan_device_sensitive(false, false, true));
        assert!(!vulkan_device_sensitive(true, false, false));
        assert!(!vulkan_device_sensitive(false, false, false));
    }

    #[test]
    fn hidden_vulkan_device_is_only_applied_for_the_vulkan_backend() {
        assert!(updates_vulkan_device(RendererBackend::Vulkan));
        for backend in [
            RendererBackend::OpenGlGlsl,
            RendererBackend::OpenGlGlasm,
            RendererBackend::OpenGlSpirV,
            RendererBackend::Metal,
            RendererBackend::Null,
        ] {
            assert!(!updates_vulkan_device(backend));
        }
    }
}
