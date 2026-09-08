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
// The `**`-suffixed labels are upstream's marker for settings that
// `ConfigureDebug::ApplyConfiguration` resets on exit, explained by the trailing
// note.

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
    // Upstream ships without the web applet compiled in, so this row is a
    // permanently-disabled placeholder reading "Web applet not compiled".
    let web_applet = w::check_row("Web applet not compiled", false);
    web_applet.set_sensitive(false);
    let all_controllers = w::check_row(
        "Enable All Controller Types",
        *common::settings::values()
            .enable_all_controllers
            .get_value(),
    );
    let auto_stub = w::check_row(
        "Enable Auto-Stub**",
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
        let launcher = gtk::gio::AppInfo::launch_default_for_uri(
            &format!("file://{}", path.display()),
            gtk::gio::AppLaunchContext::NONE,
        );
        if let Err(err) = launcher {
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
        common::logging::backend::set_color_console_backend_enabled(console);
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
        }
        let locked_page = page(false);
        assert!(!find_gpu_level(&locked_page.widget).unwrap().is_sensitive());
        assert!(!find_switch(&locked_page.widget, "Dump SPIR-V Shaders").unwrap().is_sensitive());
        let page = page(true);
        let gpu_level = find_gpu_level(&page.widget).unwrap();
        let gpu_dumps = find_switch(&page.widget, "Dump SPIR-V Shaders").unwrap();
        assert_eq!(gpu_level.selected(), 2);
        assert!(gpu_dumps.is_active());
        assert!(gpu_level.is_sensitive() && gpu_dumps.is_sensitive());
        fn find_filter(widget: &gtk::Widget) -> Option<gtk::Entry> {
            if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
                if entry.text() == "*:Warning" { return Some(entry.clone()); }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(entry) = find_filter(&widget) { return Some(entry); }
                child = widget.next_sibling();
            }
            None
        }
        find_filter(&page.widget).expect("global log filter entry").set_text("*:Info");
        let check = find_switch(&page.widget, "Use dev.keys").expect("Use dev.keys row");
        assert!(!check.is_active());
        for (enabled, expected) in [(true, 0x22), (false, 0x11)] {
            check.set_active(enabled);
            (page.apply)();
            assert_eq!(*common::settings::values().use_dev_keys.get_value(), enabled);
            assert_eq!(manager.lock().unwrap().get_key_128(S128KeyType::Master, 0, 0), [expected; 16]);
        }
        for index in 0..5 {
            gpu_level.set_selected(index);
            gpu_dumps.set_active(index % 2 == 0);
            (page.apply)();
            let values = common::settings::values();
            assert_eq!(*values.gpu_log_level.get_value() as u32, index);
            assert_eq!(*values.gpu_log_shader_dumps.get_value(), index % 2 == 0);
        }
        log::info!("after_apply_visible");
        common::logging::backend::stop();
        let log = std::fs::read_to_string(directory.path().join("log/ruzu_log.txt")).unwrap();
        assert!(!log.contains("before_apply_hidden"));
        assert!(log.contains("after_apply_visible"));
    }
}
