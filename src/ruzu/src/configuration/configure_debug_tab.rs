// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/configuration/configure_debug_tab.cpp`
// (`ConfigureDebugTab`), whose widget tree lives in `configure_debug_tab.ui`.
//
// Upstream `ConfigureDebugTab` is itself a `QTabWidget` nested inside the
// dialog's outer tab widget, holding two pages: "Debug" (`ConfigureDebug`) and
// "CPU" (`ConfigureCpuDebug`). That nesting is why the Debug screen shows two
// rows of tabs.
//
// `ConfigureDialog` resets this inner widget to page 0 whenever the outer tab
// changes (`debug_tab_tab->SetCurrentIndex(0)`).

use gtk::prelude::*;

use super::configure_cpu_debug;
use super::configure_debug;
use super::configure_dialog::Page;

/// Build the Debug tab — upstream `ConfigureDebugTab`.
pub fn page(runtime_lock: bool) -> Page {
    let notebook = gtk::Notebook::new();
    notebook.set_hexpand(true);
    notebook.set_vexpand(true);

    let debug = configure_debug::page(runtime_lock);
    let cpu_debug = configure_cpu_debug::page(runtime_lock);

    notebook.append_page(&debug.widget, Some(&gtk::Label::new(Some(&debug.title))));
    notebook.append_page(
        &cpu_debug.widget,
        Some(&gtk::Label::new(Some(&cpu_debug.title))),
    );

    Page::new("Debug", notebook, move || {
        // Upstream forwards `ApplyConfiguration` to both inner pages.
        (debug.apply)();
        (cpu_debug.apply)();
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run separately so GTK initialization is not shared with other test
    /// threads. This constructs widgets but never presents a window or saves
    /// configuration. Missing display support is an error, not a silent pass.
    #[test]
    #[ignore = "requires a GTK display; run this exact test in its own process"]
    fn cpu_optimization_widgets_follow_the_runtime_lock() {
        gtk::init().expect("a GTK display is required");
        fn collect_checks(widget: &gtk::Widget, checks: &mut Vec<gtk::CheckButton>) {
            if let Some(check) = widget.downcast_ref::<gtk::CheckButton>() {
                checks.push(check.clone());
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                collect_checks(&widget, checks);
                child = widget.next_sibling();
            }
        }
        for runtime_lock in [true, false] {
            let page = page(runtime_lock);
            let notebook = page.widget.downcast_ref::<gtk::Notebook>().unwrap();
            let cpu_page = notebook.nth_page(Some(1)).unwrap();
            let mut checks = Vec::new();
            collect_checks(&cpu_page, &mut checks);
            assert_eq!(checks.len(), 12);
            for check in checks {
                assert_eq!(check.is_sensitive(), runtime_lock, "{:?}", check.label());
            }
            let debug_page = notebook.nth_page(Some(0)).unwrap();
            let mut checks = Vec::new();
            collect_checks(&debug_page, &mut checks);
            for label in [
                "Show Log in Console",
                "Enable FS Access Log",
                "Enable Graphics Debugging",
                "Enable Renderdoc Hotkey",
                "Enable Shader Feedback",
                "Enable Nsight Aftermath",
                "Disable Loop safety checks",
                "Disable Buffer Reorder",
                "Dump Game Shaders",
                "Disable Macro JIT",
                "Dump Maxwell Macros",
                "Disable Macro HLE",
            ] {
                let check = checks
                    .iter()
                    .find(|check| check.label().as_deref() == Some(label))
                    .unwrap_or_else(|| panic!("missing debug row: {label}"));
                assert_eq!(check.is_sensitive(), runtime_lock, "{label}");
            }
            // These dedicated upstream debug controls remain editable live.
            for label in [
                "Enable GDB Stub",
                "Enable Extended Logging**",
                "Kiosk (Quest) Mode",
                "Enable Debug Asserts",
                "Enable Verbose Reporting Services**",
                "Dump Audio Commands To Console**",
            ] {
                let check = checks
                    .iter()
                    .find(|check| check.label().as_deref() == Some(label))
                    .unwrap();
                assert!(check.is_sensitive(), "{label}");
            }
        }
    }
}
