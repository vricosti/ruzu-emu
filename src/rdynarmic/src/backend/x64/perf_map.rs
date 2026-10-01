// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Linux perf-map registration for generated x64 code.
//!
//! Port of Dynarmic `backend/x64/perf_map.{h,cpp}`.

#[cfg(all(target_os = "linux", not(target_os = "android")))]
mod imp {
    use std::fs::File;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    static FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();

    fn file() -> &'static Mutex<Option<File>> {
        FILE.get_or_init(|| Mutex::new(None))
    }

    fn open_file() -> Option<File> {
        let perf_dir = crate::debug_env_var_os!("PERF_BUILDID_DIR")?;
        let filename = PathBuf::from(perf_dir).join(format!("perf-{}.map", std::process::id()));
        File::create(filename).ok()
    }

    fn format_line(start: usize, end: usize, friendly_name: &str) -> String {
        format!(
            "{start:016x} {:016x} {friendly_name}\n",
            end.wrapping_sub(start)
        )
    }

    pub fn register(start: *const u8, end: *const u8, friendly_name: &str) {
        if start == end {
            return;
        }
        let mut guard = file().lock().unwrap();
        if guard.is_none() {
            *guard = open_file();
        }
        let Some(output) = guard.as_mut() else {
            return;
        };
        let line = format_line(start as usize, end as usize, friendly_name);
        let _ = output.write_all(line.as_bytes());
    }

    pub fn clear() {
        let mut guard = file().lock().unwrap();
        if guard.take().is_none() {
            return;
        }
        *guard = open_file();
    }

    #[cfg(test)]
    mod tests {
        use super::format_line;

        #[test]
        fn perf_map_line_matches_upstream_format() {
            assert_eq!(
                format_line(0x1234, 0x1278, "a64_0000000000004000_fpcr00000000"),
                "0000000000001234 0000000000000044 a64_0000000000004000_fpcr00000000\n"
            );
        }
    }
}

#[cfg(all(target_os = "linux", not(target_os = "android")))]
pub use imp::{clear, register};

#[cfg(not(all(target_os = "linux", not(target_os = "android"))))]
pub fn register(_start: *const u8, _end: *const u8, _friendly_name: &str) {}

#[cfg(not(all(target_os = "linux", not(target_os = "android"))))]
pub fn clear() {}
