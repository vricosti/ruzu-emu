// SPDX-License-Identifier: GPL-3.0-or-later
// Counterpart of yuzu/debugger/console.{h,cpp}.

/// Apply the frontend console preference. Rust's logger uses std::io, not
/// C FILE streams: AllocConsole initializes the Win32 standard handles used
/// by std::io, so the upstream freopen_s calls are not needed here.
pub fn toggle_console() {
    let shown = crate::uisettings::with(|values| *values.show_console.get_value());
    #[cfg(all(target_os = "windows", not(debug_assertions)))]
    {
        use std::cell::Cell;
        use windows_sys::Win32::System::Console::{
            AllocConsole, FreeConsole, SetConsoleOutputCP, SetStdHandle,
            STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        };
        thread_local! {
            static CONSOLE_SHOWN: Cell<bool> = const { Cell::new(false) };
        }
        if CONSOLE_SHOWN.with(|previous| previous.replace(shown)) == shown {
            return;
        }
        // Serialize detachment with Rust's stderr writer. After detachment,
        // null handles give GUI stdio its normal disconnected behavior instead
        // of leaving a stale console handle (eprintln! panics on write errors).
        let stderr = std::io::stderr();
        let _stderr = stderr.lock();
        unsafe {
            if shown {
                if AllocConsole() != 0 {
                    SetConsoleOutputCP(65001);
                    common::logging::backend::set_color_console_backend_enabled(true);
                }
            } else if FreeConsole() != 0 {
                common::logging::backend::set_color_console_backend_enabled(false);
                SetStdHandle(STD_INPUT_HANDLE, std::ptr::null_mut());
                SetStdHandle(STD_OUTPUT_HANDLE, std::ptr::null_mut());
                SetStdHandle(STD_ERROR_HANDLE, std::ptr::null_mut());
            }
        }
    }
    #[cfg(not(all(target_os = "windows", not(debug_assertions))))]
    common::logging::backend::set_color_console_backend_enabled(shown);
}

#[cfg(all(test, target_os = "windows", not(debug_assertions)))]
mod tests {
    #[test]
    #[ignore = "detaches the test runner console; run alone on interactive Windows in release"]
    fn console_can_be_reopened_and_detached_stderr_remains_writable() {
        use std::io::Write;
        use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleWindow};
        unsafe { FreeConsole(); }
        for _ in 0..2 {
            crate::uisettings::with_mut(|values| values.show_console.set_value(true));
            super::toggle_console();
            let window = unsafe { GetConsoleWindow() };
            assert!(!window.is_null());
            super::toggle_console();
            assert_eq!(unsafe { GetConsoleWindow() }, window);
            writeln!(std::io::stderr(), "Console visibility regression").unwrap();
            crate::uisettings::with_mut(|values| values.show_console.set_value(false));
            super::toggle_console();
            assert!(unsafe { GetConsoleWindow() }.is_null());
            writeln!(std::io::stderr(), "Detached console regression").unwrap();
            super::toggle_console();
        }
    }
}
