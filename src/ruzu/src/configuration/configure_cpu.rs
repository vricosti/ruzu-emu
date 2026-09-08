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

use common::settings_enums::{CpuAccuracy, CpuBackend};

use super::configure_dialog::Page;
use super::shared_translation as tr;
use super::shared_widget as w;

// ConfigureCpu::UpdateGroup: unsafe optimizations apply only to Dynarmic.
fn update_group(accuracy: CpuAccuracy, backend: CpuBackend) -> bool {
    accuracy == CpuAccuracy::Unsafe && backend == CpuBackend::Dynarmic
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_group_requires_unsafe_dynarmic() {
        assert!(update_group(CpuAccuracy::Unsafe, CpuBackend::Dynarmic));
        assert!(!update_group(CpuAccuracy::Unsafe, CpuBackend::Nce));
        assert!(!update_group(CpuAccuracy::Auto, CpuBackend::Dynarmic));
        assert!(!update_group(CpuAccuracy::Debugging, CpuBackend::Dynarmic));
    }

    #[test]
    fn runtime_accuracy_is_locked_but_custom_ticks_remain_editable() {
        let mut values = common::settings::Values::default();
        let accuracy = w::SettingEditPolicy::new(&values.cpu_accuracy, false, true);
        assert!(!accuracy.sensitive);
        accuracy.apply(&mut values.cpu_accuracy, CpuAccuracy::Unsafe);
        assert_eq!(*values.cpu_accuracy.get_value(), CpuAccuracy::Auto);
        let enabled = w::SettingEditPolicy::new(&values.use_custom_cpu_ticks, false, true);
        let ticks = w::SettingEditPolicy::new(&values.cpu_ticks, false, true);
        assert!(enabled.sensitive && ticks.sensitive);
        enabled.apply(&mut values.use_custom_cpu_ticks, true);
        ticks.apply(&mut values.cpu_ticks, 12345);
        assert!(*values.use_custom_cpu_ticks.get_value());
        assert_eq!(*values.cpu_ticks.get_value(), 12345);
    }

    #[test]
    fn all_unsafe_options_reject_runtime_writes_and_allow_idle_writes() {
        let mut values = common::settings::Values::default();
        for setting in [
            &mut values.cpuopt_unsafe_host_mmu,
            &mut values.cpuopt_unsafe_unfuse_fma,
            &mut values.cpuopt_unsafe_reduce_fp_error,
            &mut values.cpuopt_unsafe_ignore_standard_fpcr,
            &mut values.cpuopt_unsafe_inaccurate_nan,
            &mut values.cpuopt_unsafe_fastmem_check,
            &mut values.cpuopt_unsafe_ignore_global_monitor,
        ] {
            let initial = *setting.get_value();
            let running = w::SettingEditPolicy::new(setting, false, true);
            assert!(!running.sensitive);
            running.apply(setting, !initial);
            assert_eq!(*setting.get_value(), initial);
            let idle = w::SettingEditPolicy::new(setting, true, true);
            assert!(idle.sensitive);
            idle.apply(setting, !initial);
            assert_eq!(*setting.get_value(), !initial);
        }
        assert!(!values.cpu_backend.setting.runtime_modifiable);
    }
}

/// Build the CPU tab — upstream `ConfigureCpu`.
pub fn page(runtime_lock: bool) -> Page {
    let configuring_global = common::settings::is_configuring_global();
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

    let backend_value = *common::settings::values().cpu_backend.get_value();
    unsafe_group.set_visible(update_group(accuracy_value, backend_value));
    column.append(&unsafe_group);

    // Reevaluate both selectors, as in ConfigureCpu::UpdateGroup.
    {
        let unsafe_group = unsafe_group.clone();
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        let backend = backend.clone();
        accuracy.connect_selected_notify(move |combo| {
            let selected = tr::value_at(tr::CPU_ACCURACY, combo.selected());
            #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
            let backend_value = tr::value_at(tr::CPU_BACKEND, backend.selected());
            unsafe_group.set_visible(update_group(selected, backend_value));
        });
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        let unsafe_group = unsafe_group.clone();
        let accuracy = accuracy.clone();
        backend.connect_selected_notify(move |combo| {
            unsafe_group.set_visible(update_group(
                tr::value_at(tr::CPU_ACCURACY, accuracy.selected()),
                tr::value_at(tr::CPU_BACKEND, combo.selected()),
            ));
        });
    }

    // ConfigurationShared::Widget gates both sensitivity and ApplyConfiguration.
    let accuracy_policy = w::SettingEditPolicy::new(
        &common::settings::values().cpu_accuracy,
        runtime_lock,
        configuring_global,
    );
    accuracy_row.set_sensitive(accuracy_policy.sensitive);
    let custom_ticks_policy = w::SettingEditPolicy::new(
        &common::settings::values().use_custom_cpu_ticks,
        runtime_lock,
        configuring_global,
    );
    custom_ticks.set_sensitive(custom_ticks_policy.sensitive);
    let ticks_policy = w::SettingEditPolicy::new(
        &common::settings::values().cpu_ticks,
        runtime_lock,
        configuring_global,
    );
    ticks_row.set_sensitive(ticks_policy.sensitive);
    let host_mmu_policy = w::SettingEditPolicy::new(
        &common::settings::values().cpuopt_unsafe_host_mmu,
        runtime_lock,
        configuring_global,
    );
    host_mmu.set_sensitive(host_mmu_policy.sensitive);
    let unfuse_policy = w::SettingEditPolicy::new(
        &common::settings::values().cpuopt_unsafe_unfuse_fma,
        runtime_lock,
        configuring_global,
    );
    unfuse_fma.set_sensitive(unfuse_policy.sensitive);
    let fp_error_policy = w::SettingEditPolicy::new(
        &common::settings::values().cpuopt_unsafe_reduce_fp_error,
        runtime_lock,
        configuring_global,
    );
    reduce_fp_error.set_sensitive(fp_error_policy.sensitive);
    let fpcr_policy = w::SettingEditPolicy::new(
        &common::settings::values().cpuopt_unsafe_ignore_standard_fpcr,
        runtime_lock,
        configuring_global,
    );
    ignore_standard_fpcr.set_sensitive(fpcr_policy.sensitive);
    let nan_policy = w::SettingEditPolicy::new(
        &common::settings::values().cpuopt_unsafe_inaccurate_nan,
        runtime_lock,
        configuring_global,
    );
    inaccurate_nan.set_sensitive(nan_policy.sensitive);
    let fastmem_policy = w::SettingEditPolicy::new(
        &common::settings::values().cpuopt_unsafe_fastmem_check,
        runtime_lock,
        configuring_global,
    );
    fastmem_check.set_sensitive(fastmem_policy.sensitive);
    let monitor_policy = w::SettingEditPolicy::new(
        &common::settings::values().cpuopt_unsafe_ignore_global_monitor,
        runtime_lock,
        configuring_global,
    );
    ignore_global_monitor.set_sensitive(monitor_policy.sensitive);
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    let backend_policy = {
        let policy = w::SettingEditPolicy::new(
            &common::settings::values().cpu_backend,
            runtime_lock,
            configuring_global,
        );
        backend.set_sensitive(policy.sensitive);
        policy
    };

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
        let host_mmu_value = host_mmu.is_active();
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        let backend_value = tr::value_at(tr::CPU_BACKEND, backend.selected());

        let mut values = common::settings::values_mut();
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        backend_policy.apply(&mut values.cpu_backend, backend_value);
        accuracy_policy.apply(&mut values.cpu_accuracy, accuracy_value);
        custom_ticks_policy.apply(&mut values.use_custom_cpu_ticks, custom_ticks_enabled);
        ticks_policy.apply(&mut values.cpu_ticks, ticks_value);
        host_mmu_policy.apply(&mut values.cpuopt_unsafe_host_mmu, host_mmu_value);
        unfuse_policy.apply(&mut values.cpuopt_unsafe_unfuse_fma, unfuse);
        fp_error_policy.apply(&mut values.cpuopt_unsafe_reduce_fp_error, fp_error);
        fpcr_policy.apply(&mut values.cpuopt_unsafe_ignore_standard_fpcr, fpcr);
        nan_policy.apply(&mut values.cpuopt_unsafe_inaccurate_nan, nan);
        fastmem_policy.apply(&mut values.cpuopt_unsafe_fastmem_check, fastmem);
        monitor_policy.apply(&mut values.cpuopt_unsafe_ignore_global_monitor, monitor);
    })
}
