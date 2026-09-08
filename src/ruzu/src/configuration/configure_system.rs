// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/eden/src/yuzu/configuration/configure_system.cpp`
// (`ConfigureSystem`), whose widget tree lives in `configure_system.ui`.
//
// Two groups: "System" (language, region, time zone, custom RTC, RNG seed,
// device name) and "Core" (multicore, memory layout, speed limit).
//
// The custom-RTC and RNG-seed rows are gated on their leading check box, as
// upstream does in `ConfigureSystem::SetConfiguration`.

use gtk::prelude::*;

use std::cell::Cell;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::configure_dialog::Page;
use super::shared_translation as tr;
use super::shared_widget as w;

const CPU_CLOCKS: &[(common::settings_enums::CpuClock, &str)] = &[
    (common::settings_enums::CpuClock::Normal, "Normal"),
    (common::settings_enums::CpuClock::Boost, "Boost"),
    (common::settings_enums::CpuClock::Overclock, "Overclock"),
];

const GPU_CLOCKS: &[(common::settings_enums::GpuClock, &str)] = &[
    (common::settings_enums::GpuClock::Normal, "Normal"),
    (common::settings_enums::GpuClock::Boost, "Boost"),
    (common::settings_enums::GpuClock::Overclock, "Overclock"),
];

/// Seconds in the Switch epoch offset used to render the custom RTC field.
/// Upstream stores `custom_rtc` as a POSIX timestamp and shows it in a
/// `QDateTimeEdit`; GTK has no date/time widget, so the field is a plain entry
/// carrying the same "dd/MM/yyyy HH:mm" text upstream displays.
const RTC_FORMAT: &str = "%d/%m/%Y %H:%M";

/// Upstream `LOCALE_BLOCKLIST`. Each bit marks a language that is invalid for
/// the corresponding region index.
const LOCALE_BLOCKLIST: [u32; 7] = [
    0b0100011100001100000, // Japan
    0b0000001101001100100, // Americas
    0b0100110100001000010, // Europe
    0b0100110100001000010, // Australia
    0b0000000000000000000, // China
    0b0100111100001000000, // Korea
    0b0100111100001000000, // Taiwan
];

/// Build the System tab — upstream `ConfigureSystem`.
pub fn page(runtime_lock: bool) -> Page {
    let configuring_global = common::settings::is_configuring_global();
    let (scroller, column) = w::page();

    // --- "System" ---------------------------------------------------------
    let (system_group, system) = w::group("System");

    let cpu_clock_value = *common::settings::values().cpu_clock.get_value();
    let (cpu_clock_row, cpu_clock) = w::combo_row(
        "CPU Clocks:",
        &tr::labels(CPU_CLOCKS),
        tr::index_of(CPU_CLOCKS, &cpu_clock_value),
    );
    system.append(&cpu_clock_row);

    let gpu_clock_value = *common::settings::values().gpu_clock.get_value();
    let (gpu_clock_row, gpu_clock) = w::combo_row(
        "GPU Clocks:",
        &tr::labels(GPU_CLOCKS),
        tr::index_of(GPU_CLOCKS, &gpu_clock_value),
    );
    system.append(&gpu_clock_row);

    let language_index = tr::index_of(
        tr::LANGUAGE,
        common::settings::values().language_index.get_value(),
    );
    let (language_row, language) =
        w::combo_row("Language:", &tr::labels(tr::LANGUAGE), language_index);
    system.append(&language_row);

    let region_index = tr::index_of(
        tr::REGION,
        common::settings::values().region_index.get_value(),
    );
    let (region_row, region) = w::combo_row("Region:", &tr::labels(tr::REGION), region_index);
    system.append(&region_row);

    let time_zones = time_zone_labels();
    let time_zone_refs: Vec<&str> = time_zones.iter().map(String::as_str).collect();
    let time_zone_index = *common::settings::values().time_zone_index.get_value() as u32;
    let (time_zone_row, time_zone) = w::combo_row("Time Zone:", &time_zone_refs, time_zone_index);
    system.append(&time_zone_row);

    // Custom RTC: check box in the label column, entry in the control column,
    // mirroring `configure_system.ui`'s `custom_rtc` / `custom_rtc_edit` pair.
    let rtc_enabled = *common::settings::values().custom_rtc_enabled.get_value();
    let custom_rtc_check = gtk::CheckButton::with_label("Custom RTC Date:");
    custom_rtc_check.set_active(rtc_enabled);
    let rtc_offset_value = *common::settings::values().custom_rtc_offset.get_value();
    let custom_rtc_entry = gtk::Entry::new();
    custom_rtc_entry.set_text(&format_rtc(rtc_display_time(unix_time_seconds(), rtc_enabled, rtc_offset_value)));
    custom_rtc_entry.set_sensitive(rtc_enabled);
    let rtc_row = gated_row(&custom_rtc_check, &custom_rtc_entry);
    system.append(&rtc_row);

    // GtkSpinButton stores doubles, which cannot preserve every signed 64-bit
    // offset. Keep the exact decimal representation of upstream's s64 setting.
    let rtc_offset = gtk::Entry::new();
    rtc_offset.set_text(&rtc_offset_value.to_string());
    rtc_offset.set_sensitive(rtc_enabled);
    let rtc_offset_row = w::labeled_row(" ", &rtc_offset);
    system.append(&rtc_offset_row);

    // RNG seed, gated the same way.
    let seed_enabled = *common::settings::values().rng_seed_enabled.get_value();
    let rng_seed_check = gtk::CheckButton::with_label("RNG Seed");
    rng_seed_check.set_active(seed_enabled);
    let rng_seed_entry = w::create_hex_edit(*common::settings::values().rng_seed.get_value());
    rng_seed_entry.set_sensitive(seed_enabled);
    let seed_row = gated_row(&rng_seed_check, &rng_seed_entry);
    system.append(&seed_row);

    let device_name_value = common::settings::values().device_name.get_value().clone();
    let (device_name_row, device_name) = w::entry_row("Device Name", &device_name_value);
    device_name_row.set_visible(configuring_global);
    system.append(&device_name_row);

    let console_mode_value = *common::settings::values().use_docked_mode.get_value();
    let docked = gtk::CheckButton::with_label("Docked");
    let handheld = gtk::CheckButton::with_label("Handheld");
    handheld.set_group(Some(&docked));
    if console_mode_value == common::settings_enums::ConsoleMode::Handheld {
        handheld.set_active(true);
    } else {
        docked.set_active(true);
    }
    let console_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 24);
    console_buttons.append(&docked);
    console_buttons.append(&handheld);
    let console_mode_row = w::labeled_row("Console Mode:", &console_buttons);
    console_mode_row.set_visible(!configuring_global);
    system.append(&console_mode_row);

    let program_args_value = common::settings::values().program_args.get_value().clone();
    let (program_args_row, program_args) = w::entry_row("Homebrew Args:", &program_args_value);
    // Global arguments belong to ConfigureDebug upstream. Keeping a second
    // editable copy here overwrites Debug's newly applied text with stale data.
    program_args_row.set_visible(!configuring_global);
    system.append(&program_args_row);

    let invalid_locale = gtk::Label::new(None);
    invalid_locale.set_wrap(true);
    invalid_locale.set_xalign(0.0);
    system.append(&invalid_locale);
    connect_locale_validation(&language, &region, &invalid_locale);

    column.append(&system_group);

    // --- "Core" -----------------------------------------------------------
    let (core_group, core) = w::group("Core");

    let multicore = w::check_row(
        "Multicore CPU Emulation",
        *common::settings::values().use_multi_core.get_value(),
    );
    core.append(&multicore);

    let memory_index = tr::index_of(
        tr::MEMORY_LAYOUT,
        common::settings::values().memory_layout_mode.get_value(),
    );
    let (memory_row, memory) = w::combo_row(
        "Memory Layout",
        &tr::labels(tr::MEMORY_LAYOUT),
        memory_index,
    );
    core.append(&memory_row);

    // Speed limit: check box in the label column, spin box in the control one.
    let limit_enabled = *common::settings::values().use_speed_limit.get_value();
    let speed_check = gtk::CheckButton::with_label("Limit Speed Percent");
    speed_check.set_active(limit_enabled);
    let speed_spin = gtk::SpinButton::with_range(0.0, 9999.0, 1.0);
    speed_spin.set_value(*common::settings::values().speed_limit.get_value() as f64);
    speed_spin.set_hexpand(true);
    let speed_suffix = gtk::Label::new(Some("%"));
    let speed_control = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    speed_control.append(&speed_spin);
    speed_control.append(&speed_suffix);
    speed_control.set_sensitive(limit_enabled);
    let speed_row = gated_row(&speed_check, &speed_control);
    core.append(&speed_row);

    let (slow_speed_row, slow_speed) = w::spin_row(
        "Slow Speed:",
        *common::settings::values().slow_speed_limit.get_value() as f64,
        0.0,
        9999.0,
        1.0,
        "%",
    );
    core.append(&slow_speed_row);

    let (turbo_speed_row, turbo_speed) = w::spin_row(
        "Turbo Speed:",
        *common::settings::values().turbo_speed_limit.get_value() as f64,
        0.0,
        9999.0,
        1.0,
        "%",
    );
    core.append(&turbo_speed_row);

    let sync_core_speed = w::check_row(
        "Synchronize Core Speed",
        *common::settings::values().sync_core_speed.get_value(),
    );
    core.append(&sync_core_speed);

    column.append(&core_group);

    // Gate each dependent control on its check box, as upstream does.
    gate(&custom_rtc_check, &custom_rtc_entry);
    gate(&custom_rtc_check, &rtc_offset);
    let (rtc_time, rtc_offset_state, refresh_rtc) =
        connect_rtc_controls(&custom_rtc_check, &custom_rtc_entry, &rtc_offset);
    gate(&rng_seed_check, &rng_seed_entry);
    gate(&speed_check, &speed_control);

    // Line the control column up across both groups. The check-box rows would
    // otherwise sit ~20px left of the combo rows above them.
    let label_columns = w::align_label_columns(&[
        &cpu_clock_row,
        &gpu_clock_row,
        &language_row,
        &region_row,
        &time_zone_row,
        &rtc_row,
        &rtc_offset_row,
        &seed_row,
        &device_name_row,
        &console_mode_row,
        &program_args_row,
        &memory_row,
        &speed_row,
        &slow_speed_row,
        &turbo_speed_row,
    ]);

    // ConfigureSystem's generic Widget rules apply to both row sensitivity
    // and serialization, including the paired controls' enclosing rows.
    let cpu_clock_policy = w::SettingEditPolicy::new(
        &common::settings::values().cpu_clock,
        runtime_lock,
        configuring_global,
    );
    cpu_clock_row.set_sensitive(cpu_clock_policy.sensitive);
    let gpu_clock_policy = w::SettingEditPolicy::new(
        &common::settings::values().gpu_clock,
        runtime_lock,
        configuring_global,
    );
    gpu_clock_row.set_sensitive(gpu_clock_policy.sensitive);
    let language_index_policy = w::SettingEditPolicy::new(
        &common::settings::values().language_index,
        runtime_lock,
        configuring_global,
    );
    language_row.set_sensitive(language_index_policy.sensitive);
    let region_index_policy = w::SettingEditPolicy::new(
        &common::settings::values().region_index,
        runtime_lock,
        configuring_global,
    );
    region_row.set_sensitive(region_index_policy.sensitive);
    let time_zone_index_policy = w::SettingEditPolicy::new(
        &common::settings::values().time_zone_index,
        runtime_lock,
        configuring_global,
    );
    time_zone_row.set_sensitive(time_zone_index_policy.sensitive);
    let custom_rtc_enabled_policy = w::SettingEditPolicy::new(
        &common::settings::values().custom_rtc_enabled,
        runtime_lock,
        configuring_global,
    );
    custom_rtc_check.set_sensitive(custom_rtc_enabled_policy.sensitive);
    let custom_rtc_policy = w::SettingEditPolicy::new(
        &common::settings::values().custom_rtc,
        runtime_lock,
        configuring_global,
    );
    rtc_row.set_sensitive(custom_rtc_policy.sensitive);
    let custom_rtc_offset_policy = w::SettingEditPolicy::new(
        &common::settings::values().custom_rtc_offset,
        runtime_lock,
        configuring_global,
    );
    rtc_offset_row.set_sensitive(custom_rtc_offset_policy.sensitive);
    let rng_seed_enabled_policy = w::SettingEditPolicy::new(
        &common::settings::values().rng_seed_enabled,
        runtime_lock,
        configuring_global,
    );
    rng_seed_check.set_sensitive(rng_seed_enabled_policy.sensitive);
    let rng_seed_policy = w::SettingEditPolicy::new(
        &common::settings::values().rng_seed,
        runtime_lock,
        configuring_global,
    );
    seed_row.set_sensitive(rng_seed_policy.sensitive);
    let use_multi_core_policy = w::SettingEditPolicy::new(
        &common::settings::values().use_multi_core,
        runtime_lock,
        configuring_global,
    );
    multicore.set_sensitive(use_multi_core_policy.sensitive);
    let memory_layout_mode_policy = w::SettingEditPolicy::new(
        &common::settings::values().memory_layout_mode,
        runtime_lock,
        configuring_global,
    );
    memory_row.set_sensitive(memory_layout_mode_policy.sensitive);
    let use_speed_limit_policy = w::SettingEditPolicy::new(
        &common::settings::values().use_speed_limit,
        runtime_lock,
        configuring_global,
    );
    speed_check.set_sensitive(use_speed_limit_policy.sensitive);
    let speed_limit_policy = w::SettingEditPolicy::new(
        &common::settings::values().speed_limit,
        runtime_lock,
        configuring_global,
    );
    speed_row.set_sensitive(speed_limit_policy.sensitive);
    let slow_speed_limit_policy = w::SettingEditPolicy::new(
        &common::settings::values().slow_speed_limit,
        runtime_lock,
        configuring_global,
    );
    slow_speed_row.set_sensitive(slow_speed_limit_policy.sensitive);
    let turbo_speed_limit_policy = w::SettingEditPolicy::new(
        &common::settings::values().turbo_speed_limit,
        runtime_lock,
        configuring_global,
    );
    turbo_speed_row.set_sensitive(turbo_speed_limit_policy.sensitive);
    let sync_core_speed_policy = w::SettingEditPolicy::new(
        &common::settings::values().sync_core_speed,
        runtime_lock,
        configuring_global,
    );
    sync_core_speed.set_sensitive(sync_core_speed_policy.sensitive);
    let program_args_policy = w::SettingEditPolicy::new(
        &common::settings::values().program_args,
        runtime_lock,
        configuring_global,
    );
    program_args_row.set_sensitive(program_args_policy.sensitive);
    let use_docked_mode_policy = w::SettingEditPolicy::new(
        &common::settings::values().use_docked_mode,
        runtime_lock,
        configuring_global,
    );
    console_mode_row.set_sensitive(use_docked_mode_policy.sensitive);

    Page::new("System", scroller, move || {
        // Widgets hold only a weak reference to their size group, so it has to
        // stay owned for the page's lifetime or the columns drift apart again.
        let _keep_alive = &label_columns;

        let language_value = tr::value_at(tr::LANGUAGE, language.selected());
        let cpu_clock_value = tr::value_at(CPU_CLOCKS, cpu_clock.selected());
        let gpu_clock_value = tr::value_at(GPU_CLOCKS, gpu_clock.selected());
        let region_value = tr::value_at(tr::REGION, region.selected());
        let time_zone_value = time_zone.selected();
        let rtc_on = custom_rtc_check.is_active();
        let rtc_offset_value = rtc_offset_state.get();
        // Like QDateTimeEdit, retain seconds not shown in the minute-only text.
        // Invalid intermediate text does not replace the last valid date.
        let rtc_value = rtc_time.get();
        let seed_on = rng_seed_check.is_active();
        let seed_value = u32::from_str_radix(rng_seed_entry.text().trim(), 16).unwrap_or(0);
        let device = device_name.text().to_string();
        let multi = multicore.is_active();
        let memory_value = tr::value_at(tr::MEMORY_LAYOUT, memory.selected());
        let limit_on = speed_check.is_active();
        let limit_value = speed_spin.value() as u16;
        let slow_speed_value = slow_speed.value() as u16;
        let turbo_speed_value = turbo_speed.value() as u16;
        let synchronize_core = sync_core_speed.is_active();
        let args = program_args.text().to_string();
        let console_mode = if handheld.is_active() {
            common::settings_enums::ConsoleMode::Handheld
        } else {
            common::settings_enums::ConsoleMode::Docked
        };

        let mut values = common::settings::values_mut();
        cpu_clock_policy.apply(&mut values.cpu_clock, cpu_clock_value);
        gpu_clock_policy.apply(&mut values.gpu_clock, gpu_clock_value);
        language_index_policy.apply(&mut values.language_index, language_value);
        region_index_policy.apply(&mut values.region_index, region_value);
        if let Some(zone) = common::settings_enums::TimeZone::from_u32(time_zone_value) {
            time_zone_index_policy.apply(&mut values.time_zone_index, zone);
        }
        custom_rtc_enabled_policy.apply(&mut values.custom_rtc_enabled, rtc_on);
        custom_rtc_policy.apply(&mut values.custom_rtc, rtc_value);
        custom_rtc_offset_policy.apply(&mut values.custom_rtc_offset, rtc_offset_value);
        rng_seed_enabled_policy.apply(&mut values.rng_seed_enabled, seed_on);
        rng_seed_policy.apply(&mut values.rng_seed, seed_value);
        if configuring_global {
            values.device_name.set_value(device);
        }
        use_multi_core_policy.apply(&mut values.use_multi_core, multi);
        memory_layout_mode_policy.apply(&mut values.memory_layout_mode, memory_value);
        use_speed_limit_policy.apply(&mut values.use_speed_limit, limit_on);
        speed_limit_policy.apply(&mut values.speed_limit, limit_value);
        slow_speed_limit_policy.apply(&mut values.slow_speed_limit, slow_speed_value);
        turbo_speed_limit_policy.apply(&mut values.turbo_speed_limit, turbo_speed_value);
        sync_core_speed_policy.apply(&mut values.sync_core_speed, synchronize_core);
        if !configuring_global {
            program_args_policy.apply(&mut values.program_args, args);
        }
        if !configuring_global {
            use_docked_mode_policy.apply(&mut values.use_docked_mode, console_mode);
        }
        drop(values);
        refresh_rtc();
    })
}

/// `ConfigureSystem::UpdateRtcTime` plus its reciprocal date/offset update.
fn connect_rtc_controls(
    enabled: &gtk::CheckButton,
    date: &gtk::Entry,
    offset: &gtk::Entry,
) -> (Rc<Cell<i64>>, Rc<Cell<i64>>, Rc<dyn Fn()>) {
    let updating = Rc::new(Cell::new(false));
    let previous_time = Rc::new(Cell::new(0));
    let displayed_time = Rc::new(Cell::new(0));
    let offset_state = Rc::new(Cell::new(parse_rtc_offset(&offset.text()).unwrap_or(0)));

    // ConfigureSystem::UpdateRtcTime. The text field cannot store hidden
    // seconds as QDateTimeEdit does, so keep its full timestamp separately.
    let refresh: Rc<dyn Fn()> = Rc::new({
        let enabled = enabled.downgrade();
        let date = date.downgrade();
        let offset = offset.downgrade();
        let previous_time = Rc::clone(&previous_time);
        let displayed_time = Rc::clone(&displayed_time);
        let updating = Rc::clone(&updating);
        let offset_state = Rc::clone(&offset_state);
        move || {
            let (Some(enabled), Some(date), Some(offset)) =
                (enabled.upgrade(), date.upgrade(), offset.upgrade())
            else {
                return;
            };
            if updating.replace(true) {
                return;
            }
            if let Some(value) = parse_rtc_offset(&offset.text()) {
                offset_state.set(value);
                offset.remove_css_class("error");
            } else {
                offset.add_css_class("error");
            }
            let timestamp = rtc_display_time(
                unix_time_seconds(), enabled.is_active(), offset_state.get(),
            );
            previous_time.set(timestamp);
            let text = format_rtc(timestamp);
            displayed_time.set(parse_rtc(&text).unwrap_or(timestamp));
            offset.set_sensitive(enabled.is_active());
            date.set_text(&text);
            updating.set(false);
        }
    });

    offset.connect_changed({
        let refresh = Rc::clone(&refresh);
        move |_| refresh()
    });

    date.connect_changed({
        let enabled = enabled.downgrade();
        let offset = offset.downgrade();
        let displayed_time = Rc::clone(&displayed_time);
        let refresh = Rc::clone(&refresh);
        let updating = Rc::clone(&updating);
        let offset_state = Rc::clone(&offset_state);
        move |date| {
            let (Some(enabled), Some(offset)) = (enabled.upgrade(), offset.upgrade()) else {
                return;
            };
            if updating.get() || !enabled.is_active() {
                return;
            }
            if let Some(timestamp) = parse_rtc(&date.text()) {
                if let Some(new_offset) = rtc_edited_offset(
                    offset_state.get(), displayed_time.get(), timestamp,
                ) {
                    // Suppress the intermediate refresh while updating both fields.
                    updating.set(true);
                    offset.set_text(&new_offset.to_string());
                    updating.set(false);
                    refresh();
                }
            }
        }
    });

    enabled.connect_toggled({
        let refresh = Rc::clone(&refresh);
        move |_| refresh()
    });
    refresh();
    (previous_time, offset_state, refresh)
}

fn parse_rtc_offset(text: &str) -> Option<i64> {
    text.trim().parse().ok()
}

fn rtc_display_time(now: i64, enabled: bool, offset: i64) -> i64 {
    // Same integer behavior as the RTC resource; never overflow in the editor.
    if enabled { now.wrapping_add(offset) } else { now }
}

// ConfigureSystem's update_date_offset lambda: edit relative to the displayed
// date, not the wall clock at the time of the edit. Invalid/overflowing GTK text
// is ignored, whereas Qt's constrained date editor cannot produce it.
fn rtc_edited_offset(offset: i64, previous_display: i64, selected: i64) -> Option<i64> {
    offset.checked_add(selected.checked_sub(previous_display)?)
}

fn unix_time_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Upstream `IsValidLocale`.
fn is_valid_locale(region_index: u32, language_index: u32) -> bool {
    LOCALE_BLOCKLIST
        .get(region_index as usize)
        .is_some_and(|blocked| ((blocked >> language_index) & 1) == 0)
}

fn connect_locale_validation(
    language: &gtk::DropDown,
    region: &gtk::DropDown,
    warning: &gtk::Label,
) {
    let update = Rc::new({
        let language = language.clone();
        let region = region.clone();
        let warning = warning.clone();
        move || {
            let valid = is_valid_locale(region.selected(), language.selected());
            warning.set_visible(!valid);
            if valid {
                warning.set_text("");
                return;
            }

            let language_name = language
                .selected_item()
                .and_downcast::<gtk::StringObject>()
                .map(|item| item.string().to_string())
                .unwrap_or_default();
            let region_name = region
                .selected_item()
                .and_downcast::<gtk::StringObject>()
                .map(|item| item.string().to_string())
                .unwrap_or_default();
            warning.set_text(&crate::i18n::tr_args(
                "Warning: \"%1\" is not a valid language for region \"%2\"",
                &[language_name, region_name],
            ));
        }
    });

    language.connect_selected_notify({
        let update = Rc::clone(&update);
        move |_| update()
    });
    region.connect_selected_notify({
        let update = Rc::clone(&update);
        move |_| update()
    });
    update();
}

/// A row whose label column is a check box gating the control on its right —
/// the shape upstream uses for Custom RTC, RNG Seed, and Limit Speed Percent.
///
/// The check box's width is left to `shared_widget::align_label_columns`, which
/// matches it to the plain label rows; requesting a fixed width here would put
/// the control column at a different x than the rows above it.
fn gated_row(check: &gtk::CheckButton, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(check);
    let control = control.as_ref();
    control.set_hexpand(true);
    row.append(control);
    row
}

/// Enable `control` only while `check` is ticked.
fn gate(check: &gtk::CheckButton, control: &impl IsA<gtk::Widget>) {
    let control = control.as_ref().clone();
    check.connect_toggled(move |check| control.set_sensitive(check.is_active()));
}

/// Time-zone combo entries. Upstream renders `Auto` and `Default` with the
/// resolved zone in parentheses and the rest as their plain names.
fn time_zone_labels() -> Vec<String> {
    common::settings_enums::TimeZone::canonicalizations()
        .iter()
        .map(|(name, zone)| match zone {
            common::settings_enums::TimeZone::Auto => {
                format!("Auto ({})", common::time_zone::find_system_time_zone())
            }
            common::settings_enums::TimeZone::Default => {
                format!("Default ({})", common::time_zone::get_default_time_zone())
            }
            _ => name.to_string(),
        })
        .collect()
}

/// Render a POSIX timestamp the way upstream's `QDateTimeEdit` displays it.
fn format_rtc(timestamp: i64) -> String {
    #[cfg(unix)]
    unsafe {
        let raw = timestamp as libc::time_t;
        let mut local = std::mem::zeroed::<libc::tm>();
        if !libc::localtime_r(&raw, &mut local).is_null() {
            return format!(
                "{:02}/{:02}/{:04} {:02}:{:02}",
                local.tm_mday,
                local.tm_mon + 1,
                i64::from(local.tm_year) + 1900,
                local.tm_hour,
                local.tm_min
            );
        }
    }

    // Platform fallback when no local-time conversion is available.
    let days = timestamp.div_euclid(86_400);
    let secs_of_day = timestamp.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let _ = RTC_FORMAT;
    format!(
        "{:02}/{:02}/{:04} {:02}:{:02}",
        day,
        month,
        year,
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60
    )
}

/// Parse the "dd/MM/yyyy HH:mm" text back into a POSIX timestamp.
fn parse_rtc(text: &str) -> Option<i64> {
    let (date, time) = text.trim().split_once(' ')?;
    let mut date_parts = date.split('/');
    let day: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let year: i64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) {
        return None;
    }
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    if time_parts.next().is_some()
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(1..=days_in_month(year, month)).contains(&day)
    {
        return None;
    }
    #[cfg(unix)]
    unsafe {
        let mut local = std::mem::zeroed::<libc::tm>();
        local.tm_year = i32::try_from(year - 1900).ok()?;
        local.tm_mon = i32::try_from(month - 1).ok()?;
        local.tm_mday = i32::try_from(day).ok()?;
        local.tm_hour = i32::try_from(hour).ok()?;
        local.tm_min = i32::try_from(minute).ok()?;
        local.tm_isdst = -1;
        return Some(libc::mktime(&mut local) as i64);
    }

    #[cfg(not(unix))]
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if year.rem_euclid(4) == 0
            && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0) =>
        {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 → (year, month, day). Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// (year, month, day) → days since 1970-01-01. Hinnant's `days_from_civil`.
#[cfg(any(not(unix), test))]
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtc_offset_editor_preserves_exact_integer_values() {
        for offset in [i64::MIN, -4_000_000_000, 4_000_000_000, (1i64 << 53) + 1, i64::MAX] {
            assert_eq!(parse_rtc_offset(&offset.to_string()), Some(offset));
            assert!(!format_rtc(rtc_display_time(1_800_000_000, true, offset)).is_empty());
        }
        for text in ["", "-", "1.5", "9223372036854775808"] {
            assert_eq!(parse_rtc_offset(text), None);
        }
        assert_eq!(rtc_display_time(1, true, i64::MAX), i64::MIN);
        assert_eq!(rtc_display_time(1, false, i64::MAX), 1);
    }

    #[test]
    fn rtc_date_edit_is_relative_to_previous_display_not_elapsed_host_time() {
        let now = 1_800_000_017;
        let initial_offset = 3600;
        let previous = rtc_display_time(now, true, initial_offset);
        let shown = parse_rtc(&format_rtc(previous)).unwrap();
        let offset = rtc_edited_offset(initial_offset, shown, shown + 60).unwrap();
        assert_eq!(offset, 3660);
        // An edit after 90 seconds must still add exactly one minute.
        let refreshed = rtc_display_time(now + 90, true, offset);
        assert_eq!(refreshed, now + 90 + 3660);
        assert_eq!(rtc_edited_offset(offset, shown + 60, shown), Some(3600));
        // A no-op edit retains the hidden seconds rather than rounding the offset.
        assert_eq!(rtc_edited_offset(initial_offset, shown, shown), Some(initial_offset));
        assert_eq!(previous.rem_euclid(60), 17);
    }

    #[test]
    fn disabled_rtc_shows_current_time_without_discarding_offset() {
        assert_eq!(rtc_display_time(1000, false, -300), 1000);
        assert_eq!(rtc_display_time(1020, true, -300), 720);
        assert_eq!(rtc_edited_offset(i64::MAX, 0, 1), None);
    }

    #[test]
    fn system_runtime_metadata_matches_upstream_widget_rules() {
        let values = common::settings::Values::default();
        for startup_only in [
            values.language_index.setting.runtime_modifiable,
            values.region_index.setting.runtime_modifiable,
            values.time_zone_index.setting.runtime_modifiable,
            values.use_multi_core.setting.runtime_modifiable,
            values.memory_layout_mode.setting.runtime_modifiable,
            values.sync_core_speed.setting.runtime_modifiable,
            values.program_args.setting.runtime_modifiable,
        ] {
            assert!(!startup_only);
        }
        for runtime_editable in [
            values.cpu_clock.setting.runtime_modifiable,
            values.gpu_clock.setting.runtime_modifiable,
            values.custom_rtc_enabled.setting.runtime_modifiable,
            values.custom_rtc.setting.runtime_modifiable,
            values.custom_rtc_offset.setting.runtime_modifiable,
            values.rng_seed_enabled.setting.runtime_modifiable,
            values.rng_seed.setting.runtime_modifiable,
            values.use_speed_limit.setting.runtime_modifiable,
            values.speed_limit.setting.runtime_modifiable,
            values.slow_speed_limit.setting.runtime_modifiable,
            values.turbo_speed_limit.setting.runtime_modifiable,
            values.use_docked_mode.setting.runtime_modifiable,
            values.device_name.runtime_modifiable,
        ] {
            assert!(runtime_editable);
        }
    }

    #[test]
    fn active_session_rejects_startup_changes_but_allows_speed_changes() {
        let mut values = common::settings::Values::default();
        w::SettingEditPolicy::new(&values.use_multi_core, false, true)
            .apply(&mut values.use_multi_core, false);
        assert!(*values.use_multi_core.get_value());
        w::SettingEditPolicy::new(&values.program_args, false, true)
            .apply(&mut values.program_args, "--synthetic".to_string());
        assert!(values.program_args.get_value().is_empty());
        w::SettingEditPolicy::new(&values.speed_limit, false, true)
            .apply(&mut values.speed_limit, 75);
        assert_eq!(*values.speed_limit.get_value(), 75);
        values.speed_limit.set_global(false);
        values.speed_limit.set_value(120);
        w::SettingEditPolicy::new(&values.speed_limit, false, true)
            .apply(&mut values.speed_limit, 80);
        assert_eq!(*values.speed_limit.get_value(), 120);
        assert_eq!(*values.speed_limit.get_value_global(), 75);
    }

    #[test]
    fn civil_date_conversions_round_trip() {
        for timestamp in [0i64, 1_000_000_000, 1_785_000_000, -86_400] {
            let days = timestamp.div_euclid(86_400);
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "timestamp {timestamp}");
        }
    }

    #[test]
    fn rtc_uses_the_host_local_time_like_qdatetime() {
        let timestamp = 1_785_000_000;
        assert_eq!(parse_rtc(&format_rtc(timestamp)), Some(timestamp));
    }

    #[test]
    fn rtc_text_round_trips() {
        let text = "27/07/2026 14:06";
        let parsed = parse_rtc(text).expect("parses");
        assert_eq!(format_rtc(parsed), text);
    }

    #[test]
    fn malformed_rtc_text_is_rejected_rather_than_defaulted() {
        // Silently substituting a date would move the emulated clock without
        // the user noticing; upstream's QDateTimeEdit can't produce this state.
        assert_eq!(parse_rtc("not a date"), None);
        assert_eq!(parse_rtc("27-07-2026 14:06"), None);
        assert_eq!(parse_rtc("31/02/2026 14:06"), None);
        assert_eq!(parse_rtc("01/01/2026 24:00"), None);
        assert!(parse_rtc("29/02/2024 23:59").is_some());
    }

    #[test]
    fn time_zone_list_covers_every_enum_variant() {
        let labels = time_zone_labels();
        assert_eq!(
            labels.len(),
            common::settings_enums::TimeZone::canonicalizations().len()
        );
        assert_eq!(labels[1], "Default (GMT)");
        assert_eq!(
            labels[0],
            format!("Auto ({})", common::time_zone::find_system_time_zone())
        );
    }

    #[test]
    fn locale_validation_matches_upstream_blocklist() {
        assert!(is_valid_locale(0, 0));
        assert!(!is_valid_locale(0, 6));
        assert!(!is_valid_locale(2, 1));
        assert!(is_valid_locale(4, 18));
        assert!(!is_valid_locale(7, 0));
    }
}
