// SPDX-License-Identifier: GPL-3.0-or-later
//! Counterpart of common/assert.{h,cpp}'s failure handlers.
//! Callers log the assertion first. This does not replace Rust safety assertions.

pub fn assert_fail_soft_impl() {
    let enabled = *crate::settings::values().use_debug_asserts.get_value();
    if !enabled {
        return;
    }
    crate::logging::backend::stop();
    #[cfg(target_env = "msvc")]
    unsafe {
        #[link(name = "kernel32")]
        extern "system" { fn DebugBreak(); }
        DebugBreak();
    }
    #[cfg(all(not(target_env = "msvc"), target_arch = "x86_64"))]
    unsafe { core::arch::asm!("int3"); }
    #[cfg(all(not(target_env = "msvc"), target_arch = "aarch64"))]
    unsafe { core::arch::asm!("brk #0"); }
    #[cfg(not(any(target_env = "msvc", target_arch = "x86_64", target_arch = "aarch64")))]
    std::process::exit(1);
}

pub fn assert_fatal_impl() -> ! {
    crate::logging::backend::stop();
    std::process::abort();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn failure_policy_respects_runtime_setting() {
        const CHILD: &str = "RUZU_TEST_ASSERT_FAILURE_POLICY";
        if let Ok(mode) = std::env::var(CHILD) {
            // Expected breakpoint/abort children must not generate core dumps.
            let limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) }, 0);
            crate::settings::values_mut().use_debug_asserts.set_value(mode == "enabled");
            if mode == "fatal" { assert_fatal_impl(); }
            assert_fail_soft_impl();
            return;
        }
        for mode in ["disabled", "enabled", "fatal"] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "assert::tests::failure_policy_respects_runtime_setting"])
                .env(CHILD, mode).status().unwrap();
            match mode {
                "disabled" => assert!(status.success()),
                "fatal" => assert_eq!(status.signal(), Some(libc::SIGABRT)),
                _ => {
                    if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
                        assert_eq!(status.signal(), Some(libc::SIGTRAP));
                    } else { assert_eq!(status.code(), Some(1)); }
                }
            }
        }
    }
}
