//! Counterpart of Eden's core/reporter.{h,cpp}.
//!
//! Reporter class for saving telemetry/crash/error reports as JSON files.
//! Reports are written to the log directory under type-specific subdirectories.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use common::fs as common_fs;
use common::settings;

/// Play report type enum, matching upstream Reporter::PlayReportType.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayReportType {
    Old = 0,
    Old2 = 1,
    New = 2,
    System = 3,
}

/// Reporter for generating and saving various report types.
///
/// Corresponds to the C++ `Reporter` class. Reports are saved as JSON files
/// in the log directory, organized by report type.
///
/// Upstream holds a `System&` reference; here the reporter is standalone and
/// reads `Settings::values.reporting_services` on each call, matching upstream
/// `IsReportingEnabled()` which reads from the global `Settings::values`.
pub struct Reporter {
    _private: (),
}

// --- Private helper functions (matching anonymous namespace in C++) ---

fn get_timestamp() -> String {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    timestamp_at(now).unwrap_or_else(|| "unknown-time".to_owned())
}

// GetTimestamp's local-time conversion, split only to test calendar boundaries.
// Reentrant libc APIs avoid the shared std::localtime buffer used in C++.
fn timestamp_at(timestamp: libc::time_t) -> Option<String> {
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    #[cfg(unix)]
    let valid = unsafe { !libc::localtime_r(&timestamp, &mut local).is_null() };
    #[cfg(windows)]
    let valid = unsafe { libc::localtime_s(&mut local, &timestamp) == 0 };
    #[cfg(not(any(unix, windows)))]
    let valid = false;
    valid.then(|| {
        format!(
            "{:04}-{:02}-{:02}T{:02}-{:02}-{:02}",
            local.tm_year + 1900,
            local.tm_mon + 1,
            local.tm_mday,
            local.tm_hour,
            local.tm_min,
            local.tm_sec,
        )
    })
}

fn get_path(report_type: &str, title_id: u64, timestamp: &str) -> PathBuf {
    common_fs::path_util::get_ruzu_path(common_fs::path_util::RuzuPath::LogDir)
        .join(report_type)
        .join(format!("{:016X}_{}.json", title_id, timestamp))
}

fn save_to_file(json: &serde_json::Value, filename: &PathBuf) {
    if let Some(parent) = filename.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            log::error!(
                "Failed to create path for '{}' to save report: {}",
                filename.display(),
                e
            );
            return;
        }
    }

    match fs::File::create(filename) {
        Ok(mut file) => {
            let json_str = serde_json::to_string_pretty(json).unwrap_or_default();
            if let Err(e) = file.write_all(json_str.as_bytes()) {
                log::error!("Failed to write report to '{}': {}", filename.display(), e);
            }
        }
        Err(e) => {
            log::error!(
                "Failed to create report file '{}': {}",
                filename.display(),
                e
            );
        }
    }
}

fn get_ruzu_version_data() -> serde_json::Value {
    serde_json::json!({
        "scm_rev": env!("CARGO_PKG_VERSION"),
        "scm_branch": "unknown",
        "scm_desc": "ruzu",
        "build_name": "ruzu",
        "build_date": "unknown",
        "build_fullname": "ruzu",
        "build_version": env!("CARGO_PKG_VERSION"),
    })
}

fn get_report_common_data(
    title_id: u64,
    result_raw: u32,
    timestamp: &str,
    user_id: Option<u128>,
) -> serde_json::Value {
    let mut out = serde_json::json!({
        "title_id": format!("{:016X}", title_id),
        "result_raw": format!("{:08X}", result_raw),
        "result_module": format!("{:08X}", result_raw & 0x1FF),
        "result_description": format!("{:08X}", (result_raw >> 9) & 0x1FFF),
        "timestamp": timestamp,
    });

    if let Some(uid) = user_id {
        let high = (uid >> 64) as u64;
        let low = uid as u64;
        out["user_id"] = serde_json::Value::String(format!("{:016X}{:016X}", high, low));
    }

    out
}

fn get_processor_state_data(
    architecture: &str,
    entry_point: u64,
    sp: u64,
    pc: u64,
    pstate: u64,
    registers: &[u64; 31],
    backtrace: Option<&[u64; 32]>,
) -> serde_json::Value {
    let mut out = serde_json::json!({
        "entry_point": format!("{:016X}", entry_point),
        "sp": format!("{:016X}", sp),
        "pc": format!("{:016X}", pc),
        "pstate": format!("{:016X}", pstate),
        "architecture": architecture,
    });

    let mut registers_out = BTreeMap::new();
    for (i, reg) in registers.iter().enumerate() {
        registers_out.insert(format!("X{:02}", i), format!("{:016X}", reg));
    }
    out["registers"] = serde_json::to_value(registers_out).unwrap_or_default();

    if let Some(bt) = backtrace {
        let backtrace_out: Vec<String> = bt.iter().map(|e| format!("{:016X}", e)).collect();
        out["backtrace"] = serde_json::to_value(backtrace_out).unwrap_or_default();
    }

    out
}

fn get_full_data_auto(timestamp: &str, title_id: u64) -> serde_json::Value {
    serde_json::json!({
        "yuzu_version": get_ruzu_version_data(),
        "report_common": get_report_common_data(title_id, 0, timestamp, None),
    })
}

impl Reporter {
    /// Create a new Reporter.
    /// Upstream takes a `System&`; here we just clear the FS access log on construction.
    pub fn new() -> Self {
        let reporter = Self { _private: () };
        reporter.clear_fs_access_log();
        reporter
    }

    /// Save a crash report from the fatal service.
    #[allow(clippy::too_many_arguments)]
    pub fn save_crash_report(
        &self,
        title_id: u64,
        result: u32,
        set_flags: u64,
        entry_point: u64,
        sp: u64,
        pc: u64,
        pstate: u64,
        afsr0: u64,
        afsr1: u64,
        esr: u64,
        far: u64,
        registers: &[u64; 31],
        backtrace: &[u64; 32],
        backtrace_size: u32,
        arch: &str,
        unk10: u32,
    ) {
        if !self.is_reporting_enabled() {
            return;
        }

        let timestamp = get_timestamp();
        let mut out = serde_json::json!({});

        out["yuzu_version"] = get_ruzu_version_data();
        out["report_common"] = get_report_common_data(title_id, result, &timestamp, None);

        let mut proc_out = get_processor_state_data(
            arch,
            entry_point,
            sp,
            pc,
            pstate,
            registers,
            Some(backtrace),
        );
        proc_out["set_flags"] = serde_json::Value::String(format!("{:016X}", set_flags));
        proc_out["afsr0"] = serde_json::Value::String(format!("{:016X}", afsr0));
        proc_out["afsr1"] = serde_json::Value::String(format!("{:016X}", afsr1));
        proc_out["esr"] = serde_json::Value::String(format!("{:016X}", esr));
        proc_out["far"] = serde_json::Value::String(format!("{:016X}", far));
        proc_out["backtrace_size"] = serde_json::Value::String(format!("{:08X}", backtrace_size));
        proc_out["unknown_10"] = serde_json::Value::String(format!("{:08X}", unk10));

        out["processor_state"] = proc_out;

        save_to_file(&out, &get_path("crash_report", title_id, &timestamp));
    }

    /// Save a report for svcBreak.
    pub fn save_svc_break_report(
        &self,
        title_id: u64,
        break_type: u32,
        signal_debugger: bool,
        info1: u64,
        info2: u64,
        resolved_buffer: Option<&[u8]>,
    ) {
        if !self.is_reporting_enabled() {
            return;
        }

        let timestamp = get_timestamp();
        let mut out = get_full_data_auto(&timestamp, title_id);

        let mut break_out = serde_json::json!({
            "type": format!("{:08X}", break_type),
            "signal_debugger": format!("{}", signal_debugger),
            "info1": format!("{:016X}", info1),
            "info2": format!("{:016X}", info2),
        });

        if let Some(buf) = resolved_buffer {
            break_out["debug_buffer"] = serde_json::Value::String(hex::encode(buf));
        }

        out["svc_break"] = break_out;

        save_to_file(&out, &get_path("svc_break_report", title_id, &timestamp));
    }

    /// Save a report for an unimplemented applet.
    #[allow(clippy::too_many_arguments)]
    pub fn save_unimplemented_applet_report(
        &self,
        title_id: u64,
        applet_id: u32,
        common_args_version: u32,
        library_version: u32,
        theme_color: u32,
        startup_sound: bool,
        system_tick: u64,
        normal_channel: &[Vec<u8>],
        interactive_channel: &[Vec<u8>],
    ) {
        if !self.is_reporting_enabled() {
            return;
        }

        let timestamp = get_timestamp();
        let mut out = get_full_data_auto(&timestamp, title_id);

        out["applet_common_args"] = serde_json::json!({
            "applet_id": format!("{:02X}", applet_id),
            "common_args_version": format!("{:08X}", common_args_version),
            "library_version": format!("{:08X}", library_version),
            "theme_color": format!("{:08X}", theme_color),
            "startup_sound": format!("{}", startup_sound),
            "system_tick": format!("{:016X}", system_tick),
        });

        let normal_out: Vec<String> = normal_channel.iter().map(|d| hex::encode(d)).collect();
        let interactive_out: Vec<String> =
            interactive_channel.iter().map(|d| hex::encode(d)).collect();

        out["applet_normal_data"] = serde_json::to_value(normal_out).unwrap_or_default();
        out["applet_interactive_data"] = serde_json::to_value(interactive_out).unwrap_or_default();

        save_to_file(
            &out,
            &get_path("unimpl_applet_report", title_id, &timestamp),
        );
    }

    /// Save a play report.
    pub fn save_play_report(
        &self,
        report_type: PlayReportType,
        title_id: u64,
        data: &[&[u8]],
        process_id: Option<u64>,
        user_id: Option<u128>,
    ) {
        if !self.is_reporting_enabled() {
            return;
        }

        let timestamp = get_timestamp();
        let mut out = serde_json::json!({});

        out["yuzu_version"] = get_ruzu_version_data();
        out["report_common"] = get_report_common_data(title_id, 0, &timestamp, user_id);

        let data_out: Vec<String> = data.iter().map(|d| hex::encode(d)).collect();

        if let Some(pid) = process_id {
            out["play_report_process_id"] = serde_json::Value::String(format!("{:016X}", pid));
        }

        out["play_report_type"] = serde_json::Value::String(format!("{:02}", report_type as u8));
        out["play_report_data"] = serde_json::to_value(data_out).unwrap_or_default();

        save_to_file(&out, &get_path("play_report", title_id, &timestamp));
    }

    /// Save an error report from the error applet.
    pub fn save_error_report(
        &self,
        title_id: u64,
        result: u32,
        custom_text_main: Option<&str>,
        custom_text_detail: Option<&str>,
    ) {
        if !self.is_reporting_enabled() {
            return;
        }

        let timestamp = get_timestamp();
        let mut out = serde_json::json!({});

        out["yuzu_version"] = get_ruzu_version_data();
        out["report_common"] = get_report_common_data(title_id, result, &timestamp, None);

        out["error_custom_text"] = serde_json::json!({
            "main": custom_text_main.unwrap_or(""),
            "detail": custom_text_detail.unwrap_or(""),
        });

        save_to_file(&out, &get_path("error_report", title_id, &timestamp));
    }

    /// Save a filesystem access log message.
    pub fn save_fs_access_log(&self, log_message: &str) {
        let access_log_path =
            common_fs::path_util::get_ruzu_path(common_fs::path_util::RuzuPath::SDMCDir)
                .join("FsAccessLog.txt");

        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&access_log_path)
        {
            let _ = file.write_all(log_message.as_bytes());
        }
    }

    /// Save a report for an unimplemented HLE function.
    /// Corresponds to upstream `Reporter::SaveUnimplementedFunctionReport`.
    pub fn save_unimplemented_function_report(
        &self,
        title_id: u64,
        command_id: u32,
        name: &str,
        service_name: &str,
    ) {
        if !self.is_reporting_enabled() {
            return;
        }

        let timestamp = get_timestamp();
        let mut out = get_full_data_auto(&timestamp, title_id);

        out["function"] = serde_json::json!({
            "command_id": command_id,
            "function_name": name,
            "service_name": service_name,
        });

        save_to_file(&out, &get_path("unimpl_func_report", title_id, &timestamp));
    }

    /// Save a user-initiated debug report.
    pub fn save_user_report(&self, title_id: u64) {
        if !self.is_reporting_enabled() {
            return;
        }

        let timestamp = get_timestamp();
        save_to_file(
            &get_full_data_auto(&timestamp, title_id),
            &get_path("user_report", title_id, &timestamp),
        );
    }

    // --- Private methods ---

    fn clear_fs_access_log(&self) {
        let access_log_path =
            common_fs::path_util::get_ruzu_path(common_fs::path_util::RuzuPath::SDMCDir)
                .join("FsAccessLog.txt");

        match fs::File::create(&access_log_path) {
            Ok(_) => {} // Successfully truncated
            Err(_) => {
                log::error!("Failed to clear the filesystem access log.");
            }
        }
    }

    fn is_reporting_enabled(&self) -> bool {
        *settings::values().reporting_services.get_value()
    }
}

impl Default for Reporter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_timestamp_uses_real_local_calendar() {
        const CHILD: &str = "RUZU_TEST_REPORT_TIMESTAMP";
        if std::env::var_os(CHILD).is_none() {
            for zone in ["UTC", "EST5"] {
                assert!(std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "reporter::tests::report_timestamp_uses_real_local_calendar"
                    ])
                    .env(CHILD, "1")
                    .env("TZ", zone)
                    .status()
                    .unwrap()
                    .success());
            }
            return;
        }
        if std::env::var("TZ").unwrap() == "UTC" {
            assert_eq!(
                timestamp_at(1_709_210_096).as_deref(),
                Some("2024-02-29T12-34-56")
            );
            assert_eq!(
                timestamp_at(1_767_225_599).as_deref(),
                Some("2025-12-31T23-59-59")
            );
            assert_eq!(
                timestamp_at(1_767_225_600).as_deref(),
                Some("2026-01-01T00-00-00")
            );
        } else {
            assert_eq!(
                timestamp_at(1_709_210_096).as_deref(),
                Some("2024-02-29T07-34-56")
            );
            assert_eq!(
                timestamp_at(1_767_225_600).as_deref(),
                Some("2025-12-31T19-00-00")
            );
        }
    }
}
