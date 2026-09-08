// SPDX-License-Identifier: GPL-3.0-or-later

//! Counterpart of `yuzu/startup_checks.{h,cpp}`.
//!
//! The driver probe runs before GTK/configuration initialization in a fresh
//! process. `Command` owns the child environment and handles rather than
//! temporarily changing the parent's environment around fork/CreateProcess.
//! Ruzu does not use Eden's additional Windows parent/child GUI relaunch.

use std::io;
use std::process::Command;

const STARTUP_CHECK_ENV_VAR: &str = "RUZU_DO_STARTUP_CHECKS";
const ENV_VAR_ENABLED_TEXT: &str = "ON";

/// `CheckVulkan`: ordinary loader/API errors are reported but are not crashes.
/// The parent detects abnormal process termination, as in upstream.
fn check_vulkan() {
    use video_core::vulkan_common::{vulkan_instance, vulkan_library};
    let result = vulkan_library::open_library().and_then(|entry| {
        vulkan_instance::create_instance(
            entry,
            ash::vk::API_VERSION_1_1,
            vulkan_instance::WindowSystemType::Headless,
            false,
        )
    });
    if let Err(error) = result {
        eprintln!("Failed to initialize Vulkan: {error}");
    }
}

/// `CheckEnvVars`: the probe must exit before opening any frontend windows.
pub fn check_env_vars() -> bool {
    if std::env::var_os(STARTUP_CHECK_ENV_VAR).as_deref()
        != Some(std::ffi::OsStr::new(ENV_VAR_ENABLED_TEXT))
    {
        return false;
    }
    check_vulkan();
    true
}

/// `StartupChecks`: true means that the probe child terminated unsuccessfully.
pub fn startup_checks(perform_vulkan_check: bool) -> io::Result<bool> {
    if !perform_vulkan_check {
        return Ok(false);
    }
    let mut command = Command::new(std::env::current_exe()?);
    run_startup_checks(&mut command, perform_vulkan_check)
}

// Mechanical command-injection seam for subprocess regressions. No driver is
// touched by those tests, and the production child receives no game arguments.
fn run_startup_checks(command: &mut Command, enabled: bool) -> io::Result<bool> {
    if !enabled {
        return Ok(false);
    }
    let mut child = spawn_child(command)?;
    Ok(!child.wait()?.success())
}

fn spawn_child(command: &mut Command) -> io::Result<std::process::Child> {
    command.env(STARTUP_CHECK_ENV_VAR, ENV_VAR_ENABLED_TEXT);
    command.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_probe_child() {
        let Ok(action) = std::env::var("RUZU_TEST_STARTUP_PROBE_ACTION") else {
            return;
        };
        assert_eq!(std::env::var(STARTUP_CHECK_ENV_VAR).unwrap(), "ON");
        match action.as_str() {
            "success" => (),
            "failure" => std::process::exit(7),
            #[cfg(unix)]
            "signal" => unsafe {
                // SIGKILL cannot dump core or be caught by an inherited handler.
                libc::raise(libc::SIGKILL);
                unreachable!();
            },
            _ => panic!("unknown probe action"),
        }
    }

    #[test]
    fn startup_probe_honors_disable_and_process_results() {
        let directory = tempfile::tempdir().unwrap();
        let mut missing = Command::new(directory.path().join("missing-probe"));
        assert!(!run_startup_checks(&mut missing, false).unwrap());
        assert!(run_startup_checks(&mut missing, true).is_err());
        let parent_marker = std::env::var_os(STARTUP_CHECK_ENV_VAR);
        let cases = [
            ("success", false),
            ("failure", true),
            #[cfg(unix)]
            ("signal", true),
        ];
        for (action, broken) in cases {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", "startup_checks::tests::startup_probe_child"])
                .env("RUZU_TEST_STARTUP_PROBE_ACTION", action);
            assert_eq!(run_startup_checks(&mut command, true).unwrap(), broken);
            assert_eq!(std::env::var_os(STARTUP_CHECK_ENV_VAR), parent_marker);
        }
    }
}
