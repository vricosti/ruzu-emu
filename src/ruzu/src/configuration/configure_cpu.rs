// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/eden/src/yuzu/configuration/configure_cpu.cpp`
// (`ConfigureCpu`), whose widget tree lives in `configure_cpu.ui`.
//
// A single "General" group holding the accuracy combo plus the recommendation
// note. Upstream additionally reveals an "Unsafe CPU Optimization Settings"
// group when accuracy is `Unsafe`; that group's rows are the `cpuopt_unsafe_*`
// settings.

use gtk::prelude::*;

use common::settings_enums::CpuAccuracy;

use super::configure_dialog::Page;
use super::shared_translation as tr;
use super::shared_widget as w;

/// Build the CPU tab — upstream `ConfigureCpu`.
pub fn page() -> Page {
    let (scroller, column) = w::page();

    // --- "General" --------------------------------------------------------
    let (general_group, general) = w::group("General");

    let accuracy_value = *common::settings::values().cpu_accuracy.get_value();
    let (accuracy_row, accuracy) = w::combo_row(
        "Accuracy:",
        &tr::labels(tr::CPU_ACCURACY),
        tr::index_of(tr::CPU_ACCURACY, &accuracy_value),
    );
    general.append(&accuracy_row);

    let note = gtk::Label::new(Some("We recommend setting accuracy to \"Auto\"."));
    note.set_xalign(0.0);
    general.append(&note);

    // Eden's paired `use_custom_cpu_ticks` / `cpu_ticks` setting. Keep the
    // control compact instead of allowing GTK's form-row expansion to turn the
    // spin button into the oversized block visible in the reference Qt layout.
    let custom_ticks_enabled = *common::settings::values().use_custom_cpu_ticks.get_value();
    let custom_ticks = gtk::CheckButton::with_label("Custom CPU Ticks");
    custom_ticks.set_active(custom_ticks_enabled);
    custom_ticks.set_hexpand(true);
    let ticks = gtk::SpinButton::with_range(77.0, 65_535.0, 1.0);
    ticks.set_value(*common::settings::values().cpu_ticks.get_value() as f64);
    ticks.set_width_chars(10);
    ticks.set_hexpand(false);
    ticks.set_sensitive(custom_ticks_enabled);
    let ticks_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    ticks_row.append(&custom_ticks);
    ticks_row.append(&ticks);
    general.append(&ticks_row);
    {
        let ticks = ticks.clone();
        custom_ticks.connect_toggled(move |check| ticks.set_sensitive(check.is_active()));
    }

    column.append(&general_group);

    // Upstream compiles and reveals this group only with `HAS_NCE`. Ruzu's NCE
    // backend is available on Linux/AArch64 under the equivalent target cfg.
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    let backend = {
        let (backend_group, backend_content) = w::group("CPU Backend");
        let backend_value = *common::settings::values().cpu_backend.get_value();
        let (backend_row, backend) = w::combo_row(
            "Backend:",
            &tr::labels(tr::CPU_BACKEND),
            tr::index_of(tr::CPU_BACKEND, &backend_value),
        );
        backend_content.append(&backend_row);
        column.append(&backend_group);
        backend
    };

    // --- "Unsafe CPU Optimization Settings" -------------------------------
    // Upstream shows this group only while accuracy is `Unsafe`
    // (`ConfigureCpu::UpdateGroup`).
    let (unsafe_group, unsafe_content) = w::group("Unsafe CPU Optimization Settings");

    let unsafe_note = gtk::Label::new(Some("These settings reduce accuracy for speed."));
    unsafe_note.set_xalign(0.0);
    unsafe_content.append(&unsafe_note);

    let host_mmu = w::check_row(
        "Enable Host MMU Emulation (fastmem)",
        *common::settings::values().cpuopt_unsafe_host_mmu.get_value(),
    );
    let unfuse_fma = w::check_row(
        "Unfuse FMA (improve performance on CPUs without FMA)",
        *common::settings::values()
            .cpuopt_unsafe_unfuse_fma
            .get_value(),
    );
    let reduce_fp_error = w::check_row(
        "Faster FRSQRTE and FRECPE",
        *common::settings::values()
            .cpuopt_unsafe_reduce_fp_error
            .get_value(),
    );
    let ignore_standard_fpcr = w::check_row(
        "Faster ASIMD instructions (32 bits only)",
        *common::settings::values()
            .cpuopt_unsafe_ignore_standard_fpcr
            .get_value(),
    );
    let inaccurate_nan = w::check_row(
        "Inaccurate NaN handling",
        *common::settings::values()
            .cpuopt_unsafe_inaccurate_nan
            .get_value(),
    );
    let fastmem_check = w::check_row(
        "Disable address space checks",
        *common::settings::values()
            .cpuopt_unsafe_fastmem_check
            .get_value(),
    );
    let ignore_global_monitor = w::check_row(
        "Ignore global monitor",
        *common::settings::values()
            .cpuopt_unsafe_ignore_global_monitor
            .get_value(),
    );
    for check in [
        &host_mmu,
        &unfuse_fma,
        &reduce_fp_error,
        &ignore_standard_fpcr,
        &inaccurate_nan,
        &fastmem_check,
        &ignore_global_monitor,
    ] {
        unsafe_content.append(check);
    }

    unsafe_group.set_visible(accuracy_value == CpuAccuracy::Unsafe);
    column.append(&unsafe_group);

    // Upstream `ConfigureCpu::UpdateGroup`: reveal the unsafe group only for
    // `CpuAccuracy::Unsafe`.
    {
        let unsafe_group = unsafe_group.clone();
        accuracy.connect_selected_notify(move |combo| {
            let selected = tr::value_at(tr::CPU_ACCURACY, combo.selected());
            unsafe_group.set_visible(selected == CpuAccuracy::Unsafe);
        });
    }

    Page::new("CPU", scroller, move || {
        let accuracy_value = tr::value_at(tr::CPU_ACCURACY, accuracy.selected());
        let custom_ticks_enabled = custom_ticks.is_active();
        let ticks_value = ticks.value() as u32;
        let unfuse = unfuse_fma.is_active();
        let fp_error = reduce_fp_error.is_active();
        let fpcr = ignore_standard_fpcr.is_active();
        let nan = inaccurate_nan.is_active();
        let fastmem = fastmem_check.is_active();
        let monitor = ignore_global_monitor.is_active();

        let mut values = common::settings::values_mut();
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        values
            .cpu_backend
            .set_value(tr::value_at(tr::CPU_BACKEND, backend.selected()));
        values.cpu_accuracy.set_value(accuracy_value);
        values.use_custom_cpu_ticks.set_value(custom_ticks_enabled);
        values.cpu_ticks.set_value(ticks_value);
        values.cpuopt_unsafe_host_mmu.set_value(host_mmu.is_active());
        values.cpuopt_unsafe_unfuse_fma.set_value(unfuse);
        values.cpuopt_unsafe_reduce_fp_error.set_value(fp_error);
        values.cpuopt_unsafe_ignore_standard_fpcr.set_value(fpcr);
        values.cpuopt_unsafe_inaccurate_nan.set_value(nan);
        values.cpuopt_unsafe_fastmem_check.set_value(fastmem);
        values
            .cpuopt_unsafe_ignore_global_monitor
            .set_value(monitor);
    })
}
