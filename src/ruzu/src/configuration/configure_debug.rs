// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/configuration/configure_debug.cpp`
// (`ConfigureDebug`), whose widget tree lives in `configure_debug.ui`.
//
// Layout: a "Debugger" group beside a "Logging" group on the first row, a
// "Homebrew" group below them, then three side-by-side columns — "Graphics",
// "Advanced" and "Debugging" — and finally the reset note.
//
// The `**`-suffixed labels mark session-only settings: their save=false
// metadata excludes them from configuration persistence. Apply does not reset
// them; a fresh process starts with their defaults.

use gtk::prelude::*;

use super::configure_dialog::Page;
use super::shared_widget as w;

/// Build the Debug sub-tab — upstream `ConfigureDebug`.
pub fn page(runtime_lock: bool) -> Page {
    let (scroller, column) = w::page();

    // --- Row 1: "Debugger" | "Logging" ------------------------------------
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    let (debugger_group, debugger) = w::group("Debugger");
    let gdb_stub = w::check_row(
        "Enable GDB Stub",
        *common::settings::values().use_gdbstub.get_value(),
    );
    debugger.append(&gdb_stub);
    let gdb_port = gtk::SpinButton::with_range(1024.0, 65535.0, 1.0);
    gdb_port.set_value(*common::settings::values().gdbstub_port.get_value() as f64);
    gdb_port.set_sensitive(gdb_stub.is_active());
    debugger.append(&w::labeled_row("Port:", &gdb_port));
    {
        let gdb_port = gdb_port.clone();
        gdb_stub.connect_toggled(move |check| gdb_port.set_sensitive(check.is_active()));
    }
    top.append(&debugger_group);

    let (logging_group, logging) = w::group("Logging");
    logging_group.set_hexpand(true);
    let log_filter_value = common::settings::values().log_filter.get_value().clone();
    let (log_filter_row, log_filter) = w::entry_row("Global Log Filter", &log_filter_value);
    logging.append(&log_filter_row);
    let flush_line = w::check_row("Flush log output on each line", *common::settings::values().log_flush_line.get_value());
    let censor_username = w::check_row("Censor username in logs", *common::settings::values().censor_username.get_value());
    logging.append(&flush_line);
    logging.append(&censor_username);
    let (gpu_log_row, gpu_log_level) = w::combo_row(
        "GPU Logging/Level",
        &["Off", "Errors", "Standard", "Verbose", "All"],
        *common::settings::values().gpu_log_level.get_value() as u32,
    );
    gpu_log_level.set_sensitive(runtime_lock);
    gpu_log_level.set_tooltip_text(Some("Detail level for GPU logs. Off disables logging entirely."));
    logging.append(&gpu_log_row);
    let show_console = w::check_row(
        "Show Log in Console",
        crate::uisettings::with(|v| *v.show_console.get_value()),
    );
    show_console.set_sensitive(runtime_lock);
    logging.append(&show_console);
    let extended_logging = w::check_row(
        "Enable Extended Logging**",
        *common::settings::values().extended_logging.get_value(),
    );
    logging.append(&extended_logging);
    let open_log_location = gtk::Button::with_label("Open Log Location");
    logging.append(&open_log_location);
    top.append(&logging_group);

    column.append(&top);

    // --- "Homebrew" -------------------------------------------------------
    let (homebrew_group, homebrew) = w::group("Homebrew");
    let program_args_value = common::settings::values().program_args.get_value().clone();
    let (args_row, program_args) = w::entry_row("Arguments String", &program_args_value);
    program_args.set_sensitive(runtime_lock);
    homebrew.append(&args_row);
    column.append(&homebrew_group);

    // --- Row 3: "Graphics" | "Advanced" | "Debugging" ----------------------
    let columns = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    let (graphics_group, graphics) = w::group("Graphics");
    graphics_group.set_hexpand(true);
    let renderer_debug = w::check_row(
        "Enable Graphics Debugging",
        *common::settings::values().renderer_debug.get_value(),
    );
    let renderdoc_hotkey = w::check_row(
        "Enable Renderdoc Hotkey",
        *common::settings::values()
            .enable_renderdoc_hotkey
            .get_value(),
    );
    let shader_feedback = w::check_row(
        "Enable Shader Feedback",
        *common::settings::values()
            .renderer_shader_feedback
            .get_value(),
    );
    let nsight_aftermath = w::check_row(
        "Enable Nsight Aftermath",
        *common::settings::values()
            .enable_nsight_aftermath
            .get_value(),
    );
    let disable_loop_safety = w::check_row(
        "Disable Loop safety checks",
        *common::settings::values()
            .disable_shader_loop_safety_checks
            .get_value(),
    );
    let disable_buffer_reorder = w::check_row(
        "Disable Buffer Reorder",
        *common::settings::values()
            .disable_buffer_reorder
            .get_value(),
    );
    let dump_shaders = w::check_row(
        "Dump Game Shaders",
        *common::settings::values().dump_guest_shaders.get_value(),
    );
    let disable_macro_jit = w::check_row(
        "Disable Macro JIT",
        *common::settings::values().disable_macro_jit.get_value(),
    );
    let dump_macros = w::check_row(
        "Dump Maxwell Macros",
        *common::settings::values().dump_macros.get_value(),
    );
    let disable_macro_hle = w::check_row(
        "Disable Macro HLE",
        *common::settings::values().disable_macro_hle.get_value(),
    );
    let gpu_log_shader_dumps = w::check_row(
        "Dump SPIR-V Shaders",
        *common::settings::values().gpu_log_shader_dumps.get_value(),
    );
    // Upstream's tooltip still says LogDir/shaders, but DumpSpirvShader writes
    // directly into DumpDir in both implementations.
    gpu_log_shader_dumps.set_tooltip_text(Some("Dump compiled SPIR-V binaries (.spv) to the configured dump directory."));
    for check in [
        &renderer_debug,
        &renderdoc_hotkey,
        &shader_feedback,
        &nsight_aftermath,
        &disable_loop_safety,
        &disable_buffer_reorder,
        &dump_shaders,
        &disable_macro_jit,
        &dump_macros,
        &disable_macro_hle,
        &gpu_log_shader_dumps,
    ] {
        check.set_sensitive(runtime_lock);
        graphics.append(check);
    }
    columns.append(&graphics_group);

    let (advanced_group, advanced) = w::group("Advanced");
    advanced_group.set_hexpand(true);
    let quest_flag = w::check_row(
        "Kiosk (Quest) Mode",
        *common::settings::values().quest_flag.get_value(),
    );
    let cpu_debug_mode = w::check_row(
        "Enable CPU Debugging",
        *common::settings::values().cpu_debug_mode.get_value(),
    );
    let use_dev_keys = w::check_row(
        "Use dev.keys",
        *common::settings::values().use_dev_keys.get_value(),
    );
    let debug_asserts = w::check_row(
        "Enable Debug Asserts",
        *common::settings::values().use_debug_asserts.get_value(),
    );
    let vulkan_check = w::check_row(
        "Perform Startup Vulkan Check",
        *common::settings::values().perform_vulkan_check.get_value(),
    );
    // Match ConfigureDebug without YUZU_USE_QT_WEB_ENGINE: checked and disabled,
    // then persist disable_web_applet=true when applying the page.
    let web_applet = w::check_row("Web applet not compiled", true);
    web_applet.set_sensitive(false);
    let all_controllers = w::check_row(
        "Enable All Controller Types",
        *common::settings::values()
            .enable_all_controllers
            .get_value(),
    );
    let auto_stub = w::check_row(
        "Enable Auto-Stub",
        *common::settings::values().use_auto_stub.get_value(),
    );
    for check in [
        &quest_flag,
        &use_dev_keys,
        &cpu_debug_mode,
        &debug_asserts,
        &vulkan_check,
        &web_applet,
        &all_controllers,
        &auto_stub,
    ] {
        advanced.append(check);
    }
    columns.append(&advanced_group);

    let (battery_row, serial_battery) = w::entry_row(
        "Battery Serial:",
        &common::settings::values().serial_battery.get_value().to_string(),
    );
    let (unit_row, serial_unit) = w::entry_row(
        "Unit Serial:",
        &common::settings::values().serial_unit.get_value().to_string(),
    );
    advanced.append(&battery_row);
    advanced.append(&unit_row);

    let (debugging_group, debugging) = w::group("Debugging");
    debugging_group.set_hexpand(true);
    let fs_access_log = w::check_row(
        "Enable FS Access Log",
        *common::settings::values().enable_fs_access_log.get_value(),
    );
    fs_access_log.set_sensitive(runtime_lock);
    let reporting_services = w::check_row(
        "Enable Verbose Reporting Services**",
        *common::settings::values().reporting_services.get_value(),
    );
    let dump_audio_commands = w::check_row(
        "Dump Audio Commands To Console**",
        *common::settings::values().dump_audio_commands.get_value(),
    );
    for check in [&fs_access_log, &reporting_services, &dump_audio_commands] {
        debugging.append(check);
    }
    columns.append(&debugging_group);

    column.append(&columns);

    let note = gtk::Label::new(Some("**This will be reset automatically when ruzu closes."));
    note.set_xalign(0.0);
    column.append(&note);

    // Upstream opens the log directory in the platform file manager.
    open_log_location.connect_clicked(|_| {
        let path = common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::LogDir);
        // QUrl::fromLocalFile upstream; use the same platform path adapter as
        // other folder actions, not a hand-built file:// URL.
        if let Err(err) = crate::util::game::open_folder(&path) {
            log::warn!("Failed to open log location: {err}");
        }
    });

    Page::new("Debug", scroller, move || {
        let gdb = gdb_stub.is_active();
        let port = gdb_port.value() as u16;
        let filter = log_filter.text().to_string();
        let extended = extended_logging.is_active();
        let args = program_args.text().to_string();
        let console = show_console.is_active();

        crate::uisettings::with_mut(|v| v.show_console.set_value(console));

        let mut values = common::settings::values_mut();
        values.use_gdbstub.set_value(gdb);
        values.gdbstub_port.set_value(port);
        values.log_filter.set_value(filter);
        values.log_flush_line.set_value(flush_line.is_active());
        values.censor_username.set_value(censor_username.is_active());
        values.gpu_log_level.set_value(
            common::settings_enums::GpuLogLevel::from_u32(gpu_log_level.selected())
                .unwrap_or(common::settings_enums::GpuLogLevel::Off),
        );
        values.gpu_log_shader_dumps.set_value(gpu_log_shader_dumps.is_active());
        values.extended_logging.set_value(extended);
        values.program_args.set_value(args);

        values.renderer_debug.set_value(renderer_debug.is_active());
        values
            .enable_renderdoc_hotkey
            .set_value(renderdoc_hotkey.is_active());
        values
            .renderer_shader_feedback
            .set_value(shader_feedback.is_active());
        values
            .enable_nsight_aftermath
            .set_value(nsight_aftermath.is_active());
        values
            .disable_shader_loop_safety_checks
            .set_value(disable_loop_safety.is_active());
        values
            .disable_buffer_reorder
            .set_value(disable_buffer_reorder.is_active());
        values
            .dump_guest_shaders
            .set_value(dump_shaders.is_active());
        values
            .disable_macro_jit
            .set_value(disable_macro_jit.is_active());
        values.dump_macros.set_value(dump_macros.is_active());
        values
            .disable_macro_hle
            .set_value(disable_macro_hle.is_active());

        values.quest_flag.set_value(quest_flag.is_active());
        values.disable_web_applet.set_value(web_applet.is_active());
        values.use_dev_keys.set_value(use_dev_keys.is_active());
        values.cpu_debug_mode.set_value(cpu_debug_mode.is_active());
        values
            .use_debug_asserts
            .set_value(debug_asserts.is_active());
        values
            .perform_vulkan_check
            .set_value(vulkan_check.is_active());
        values
            .enable_all_controllers
            .set_value(all_controllers.is_active());
        values.use_auto_stub.set_value(auto_stub.is_active());
        // QString::toUInt uses base 10 and returns zero for invalid/overflowing input.
        values.serial_battery.set_value(serial_battery.text().trim().parse().unwrap_or(0));
        values.serial_unit.set_value(serial_unit.text().trim().parse().unwrap_or(0));

        values
            .enable_fs_access_log
            .set_value(fs_access_log.is_active());
        values
            .reporting_services
            .set_value(reporting_services.is_active());
        values
            .dump_audio_commands
            .set_value(dump_audio_commands.is_active());
        // ReloadKeys reads settings itself; release the settings write guard
        // before taking the key-manager lock, in upstream ApplyConfiguration order.
        drop(values);
        let mut filter = common::logging::filter::Filter::default();
        filter.parse_filter_string(&common::settings::values().log_filter.get_value());
        common::logging::backend::set_global_filter(&filter);
        crate::debugger::console::toggle_console();
        ruzu_core::crypto::key_manager::KeyManager::instance()
            .lock()
            .unwrap()
            .reload_keys();
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display and isolated process for global key-manager state"]
    fn applying_dev_keys_switch_reloads_the_selected_synthetic_keys() {
        use common::fs::path_util::{set_ruzu_path, RuzuPath};
        use ruzu_core::crypto::key_manager::{KeyManager, S128KeyType};

        gtk::init().expect("a GTK display is required");
        let directory = tempfile::tempdir().unwrap();
        set_ruzu_path(RuzuPath::KeysDir, directory.path());
        std::env::remove_var("RUZU_LOG_FILTER");
        std::env::remove_var("RUST_LOG");
        common::settings::values_mut().log_filter.set_value("*:Warning".to_owned());
        common::logging::backend::initialize_with_config(Some(directory.path().join("log")), "*:Warning", false);
        log::info!("before_apply_hidden");
        // Artificial bytes only: these fixtures do not contain usable console keys.
        std::fs::write(directory.path().join("prod.keys"),
            "master_key_00 = 11111111111111111111111111111111\n").unwrap();
        std::fs::write(directory.path().join("dev.keys"),
            "master_key_00 = 22222222222222222222222222222222\n").unwrap();
        common::settings::values_mut().use_dev_keys.set_value(false);
        let manager = KeyManager::instance();
        assert_eq!(manager.lock().unwrap().get_key_128(S128KeyType::Master, 0, 0), [0x11; 16]);

        fn find_switch(widget: &gtk::Widget, label: &str) -> Option<gtk::CheckButton> {
            if let Some(check) = widget.downcast_ref::<gtk::CheckButton>() {
                if check.label().as_deref() == Some(label) {
                    return Some(check.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(check) = find_switch(&widget, label) { return Some(check); }
                child = widget.next_sibling();
            }
            None
        }
        fn find_gpu_level(widget: &gtk::Widget) -> Option<gtk::DropDown> {
            if let Some(combo) = widget.downcast_ref::<gtk::DropDown>() { return Some(combo.clone()); }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(combo) = find_gpu_level(&widget) { return Some(combo); }
                child = widget.next_sibling();
            }
            None
        }
        {
            let mut values = common::settings::values_mut();
            values.gpu_log_level.set_value(common::settings_enums::GpuLogLevel::Standard);
            values.gpu_log_shader_dumps.set_value(true);
            values.serial_battery.set_value(12345);
            values.serial_unit.set_value(98765);
            values.disable_web_applet.set_value(false);
            values.program_args.set_value("--before".into());
        }
        let locked_page = page(false);
        for label in ["Disable Macro JIT", "Disable Macro HLE", "Dump Maxwell Macros"] {
            assert!(!find_switch(&locked_page.widget, label).unwrap().is_sensitive());
        }
        assert!(!find_switch(&locked_page.widget, "Enable FS Access Log").unwrap().is_sensitive());
        assert!(!find_gpu_level(&locked_page.widget).unwrap().is_sensitive());
        assert!(!find_switch(&locked_page.widget, "Dump SPIR-V Shaders").unwrap().is_sensitive());
        let page = page(true);
        let system_page = super::super::configure_system::page(true);
        let macro_jit = find_switch(&page.widget, "Disable Macro JIT").unwrap();
        let macro_hle = find_switch(&page.widget, "Disable Macro HLE").unwrap();
        let macro_dump = find_switch(&page.widget, "Dump Maxwell Macros").unwrap();
        let guest_shaders = find_switch(&page.widget, "Dump Game Shaders").unwrap();
        assert!(!guest_shaders.is_active());
        assert!(!find_switch(&locked_page.widget, "Dump Game Shaders").unwrap().is_sensitive());
        assert!(!macro_jit.is_active() && !macro_hle.is_active() && !macro_dump.is_active());
        let gpu_level = find_gpu_level(&page.widget).unwrap();
        let gpu_dumps = find_switch(&page.widget, "Dump SPIR-V Shaders").unwrap();
        let flush_line = find_switch(&page.widget, "Flush log output on each line").unwrap();
        let censor_username = find_switch(&page.widget, "Censor username in logs").unwrap();
        let auto_stub = find_switch(&page.widget, "Enable Auto-Stub").unwrap();
        let quest_flag = find_switch(&page.widget, "Kiosk (Quest) Mode").unwrap();
        let all_controllers = find_switch(&page.widget, "Enable All Controller Types").unwrap();
        assert!(!all_controllers.is_active());
        let web_applet = find_switch(&page.widget, "Web applet not compiled").unwrap();
        assert!(!quest_flag.is_active());
        assert!(web_applet.is_active());
        assert!(!web_applet.is_sensitive());
        assert!(find_switch(&locked_page.widget, "Kiosk (Quest) Mode").unwrap().is_sensitive());
        let fs_access_log = find_switch(&page.widget, "Enable FS Access Log").unwrap();
        assert!(!fs_access_log.is_active());
        assert!(fs_access_log.is_sensitive());
        assert!(!auto_stub.is_active());
        assert!(!flush_line.is_active());
        assert!(censor_username.is_active());
        assert_eq!(gpu_level.selected(), 2);
        assert!(gpu_dumps.is_active());
        assert!(gpu_level.is_sensitive() && gpu_dumps.is_sensitive());
        fn find_entry(widget: &gtk::Widget, text: &str) -> Option<gtk::Entry> {
            if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
                if entry.text() == text { return Some(entry.clone()); }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(entry) = find_entry(&widget, text) { return Some(entry); }
                child = widget.next_sibling();
            }
            None
        }
        find_entry(&page.widget, "*:Warning").expect("global log filter entry").set_text("*:Info");
        let args_text = "--label \"café au lait\" --path=/tmp/example";
        find_entry(&page.widget, "--before").unwrap().set_text(args_text);
        let battery = find_entry(&page.widget, "12345").expect("battery serial entry");
        let unit = find_entry(&page.widget, "98765").expect("unit serial entry");
        assert!(find_entry(&locked_page.widget, "12345").unwrap().is_sensitive());
        assert!(find_entry(&locked_page.widget, "98765").unwrap().is_sensitive());
        let check = find_switch(&page.widget, "Use dev.keys").expect("Use dev.keys row");
        assert!(!check.is_active());
        for (enabled, expected) in [(true, 0x22), (false, 0x11)] {
            check.set_active(enabled);
            (page.apply)();
            assert_eq!(*common::settings::values().use_dev_keys.get_value(), enabled);
            assert_eq!(manager.lock().unwrap().get_key_128(S128KeyType::Master, 0, 0), [expected; 16]);
        }
        let serial_cases = [("0", 0), ("4294967295", u32::MAX), ("4294967296", 0), ("-1", 0), (" +42 ", 42)];
        fn find_port(widget: &gtk::Widget) -> Option<gtk::SpinButton> {
            if let Some(spin) = widget.downcast_ref::<gtk::SpinButton>() {
                return Some(spin.clone());
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(spin) = find_port(&widget) {
                    return Some(spin);
                }
                child = widget.next_sibling();
            }
            None
        }
        let gdb = find_switch(&page.widget, "Enable GDB Stub").unwrap();
        let port = find_port(&page.widget).unwrap();
        assert_eq!(port.adjustment().lower(), 1024.0);
        assert_eq!(port.adjustment().upper(), 65535.0);
        for (enabled, number) in [(true, 1024), (false, 65535), (true, 6543)] {
            gdb.set_active(enabled);
            assert_eq!(port.is_sensitive(), enabled);
            port.set_value(f64::from(number));
            (page.apply)();
            let values = common::settings::values();
            assert_eq!(*values.use_gdbstub.get_value(), enabled);
            assert_eq!(*values.gdbstub_port.get_value(), number as u16);
        }
        for index in 0..5 {
            macro_jit.set_active(index % 2 == 0);
            macro_hle.set_active(index % 2 != 0);
            macro_dump.set_active(index % 2 == 0);
            guest_shaders.set_active(index % 2 != 0);
            gpu_level.set_selected(index);
            gpu_dumps.set_active(index % 2 == 0);
            flush_line.set_active(index % 2 == 0);
            censor_username.set_active(index % 2 != 0);
            auto_stub.set_active(index % 2 != 0);
            quest_flag.set_active(index % 2 != 0);
            all_controllers.set_active(index % 2 != 0);
            fs_access_log.set_active(index % 2 != 0);
            battery.set_text(serial_cases[index as usize].0);
            unit.set_text(serial_cases[4 - index as usize].0);
            (page.apply)();
            let values = common::settings::values();
            assert_eq!(*values.disable_macro_jit.get_value(), index % 2 == 0);
            assert_eq!(*values.disable_macro_hle.get_value(), index % 2 != 0);
            assert_eq!(*values.dump_macros.get_value(), index % 2 == 0);
            assert_eq!(*values.dump_guest_shaders.get_value(), index % 2 != 0);
            assert_eq!(*values.gpu_log_level.get_value() as u32, index);
            assert_eq!(*values.gpu_log_shader_dumps.get_value(), index % 2 == 0);
            assert_eq!(*values.log_flush_line.get_value(), index % 2 == 0);
            assert_eq!(*values.censor_username.get_value(), index % 2 != 0);
            assert_eq!(*values.use_auto_stub.get_value(), index % 2 != 0);
            assert_eq!(*values.quest_flag.get_value(), index % 2 != 0);
            assert_eq!(*values.enable_all_controllers.get_value(), index % 2 != 0);
            assert!(*values.disable_web_applet.get_value());
            assert_eq!(*values.enable_fs_access_log.get_value(), index % 2 != 0);
            assert_eq!(*values.serial_battery.get_value(), serial_cases[index as usize].1);
            assert_eq!(*values.serial_unit.get_value(), serial_cases[4 - index as usize].1);
        }
        // ConfigureDialog applies General/Debug before System. The System page
        // was constructed with --before and must not overwrite the new text.
        (system_page.apply)();
        assert_eq!(common::settings::values().program_args.get_value(), args_text);
        log::info!("after_apply_visible");
        common::logging::backend::stop();
        let log = std::fs::read_to_string(directory.path().join("log/ruzu_log.txt")).unwrap();
        assert!(!log.contains("before_apply_hidden"));
        assert!(log.contains("after_apply_visible"));
    }
}
