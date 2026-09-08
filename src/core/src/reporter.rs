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
    Old3 = 2,
    New = 3,
    System = 4,
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
            let mut bytes = Vec::new();
            let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
            let mut serializer = serde_json::Serializer::with_formatter(&mut bytes, formatter);
            if let Err(e) = serde::Serialize::serialize(json, &mut serializer) {
                log::error!("Failed to serialize report '{}': {}", filename.display(), e);
                return;
            }
            bytes.push(b'\n'); // std::setw(4) << json << std::endl upstream.
            #[cfg(windows)]
            let bytes = String::from_utf8(bytes).unwrap().replace('\n', "\r\n").into_bytes();
            if let Err(e) = file.write_all(&bytes) {
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
    use common::scm_rev;
    serde_json::json!({
        "scm_rev": scm_rev::SCM_REV,
        "scm_branch": scm_rev::SCM_BRANCH,
        "scm_desc": scm_rev::SCM_DESC,
        "build_name": scm_rev::BUILD_NAME,
        "build_date": scm_rev::BUILD_DATE,
        "build_fullname": scm_rev::BUILD_FULLNAME,
        "build_version": scm_rev::BUILD_VERSION,
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

// Counterpart of GetHLEBufferDescriptorData<read_value, DescriptorType>.
// Address/size iterators replace the three C++ descriptor template types;
// output descriptors must never cause a guest-memory read.
fn get_hle_buffer_descriptor_data<const READ_VALUE: bool>(
    descriptors: impl Iterator<Item = (u64, u64)>,
    memory: &crate::memory::memory::Memory,
) -> serde_json::Value {
    serde_json::Value::Array(descriptors.map(|(address, size)| {
        let mut entry = serde_json::json!({
            "address": format!("{address:016X}"),
            "size": format!("{size:016X}"),
        });
        if READ_VALUE {
            let mut data = vec![0; size as usize];
            memory.read_block(address, &mut data);
            entry["data"] = serde_json::Value::String(hex::encode_upper(data));
        }
        entry
    }).collect())
}

fn get_hle_request_context_data(
    ctx: &crate::hle::service::hle_ipc::HLERequestContext,
    memory: &crate::memory::memory::Memory,
) -> serde_json::Value {
    serde_json::json!({
        "command_buffer": ctx.command_buffer().iter().map(|word| format!("{word:08X}")).collect::<Vec<_>>(),
        "buffer_descriptor_a": get_hle_buffer_descriptor_data::<true>(ctx.buffer_descriptor_a().iter().map(|d| (d.address(), d.size())), memory),
        "buffer_descriptor_b": get_hle_buffer_descriptor_data::<false>(ctx.buffer_descriptor_b().iter().map(|d| (d.address(), d.size())), memory),
        "buffer_descriptor_c": get_hle_buffer_descriptor_data::<false>(ctx.buffer_descriptor_c().iter().map(|d| (d.address(), d.size())), memory),
        "buffer_descriptor_x": get_hle_buffer_descriptor_data::<true>(ctx.buffer_descriptor_x().iter().map(|d| (d.address(), d.size())), memory),
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
            break_out["debug_buffer"] = serde_json::Value::String(hex::encode_upper(buf));
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

        let normal_out: Vec<String> = normal_channel.iter().map(|d| hex::encode_upper(d)).collect();
        let interactive_out: Vec<String> =
            interactive_channel.iter().map(|d| hex::encode_upper(d)).collect();

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

        let data_out: Vec<String> = data.iter().map(|d| hex::encode_upper(d)).collect();

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
    pub fn save_fs_access_log(&self, log_message: &[u8]) {
        let access_log_path =
            common_fs::path_util::get_ruzu_path(common_fs::path_util::RuzuPath::SDMCDir)
                .join("FsAccessLog.txt");

        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&access_log_path)
        {
            // Upstream string_view can contain non-UTF-8 bytes. TextFile uses
            // the native CRT newline conversion on Windows, but none on POSIX.
            #[cfg(windows)]
            let text = {
                let mut text = Vec::with_capacity(log_message.len());
                for &byte in log_message {
                    if byte == b'\n' { text.push(b'\r'); }
                    text.push(byte);
                }
                text
            };
            #[cfg(windows)]
            let log_message = text.as_slice();
            let _ = file.write_all(log_message);
        }
    }

    /// Save a report for an unimplemented HLE function.
    /// Corresponds to upstream `Reporter::SaveUnimplementedFunctionReport`.
    pub fn save_unimplemented_function_report(
        &self,
        system: crate::core::SystemRef,
        ctx: &crate::hle::service::hle_ipc::HLERequestContext,
        command_id: u32,
        name: &str,
        service_name: &str,
    ) {
        if !self.is_reporting_enabled() {
            return;
        }

        let timestamp = get_timestamp();
        let title_id = system.get().get_application_process_program_id();
        let mut out = get_full_data_auto(&timestamp, title_id);

        // Reporter is standalone in Rust; the caller supplies its SystemRef.
        // Use application memory like upstream, not the IPC client's memory.
        let memory = system.get().memory_shared().expect("application memory is not initialized");
        let mut function_out = get_hle_request_context_data(ctx, &memory.lock().unwrap());
        function_out["command_id"] = command_id.into();
        function_out["function_name"] = name.into();
        function_out["service_name"] = service_name.into();
        out["function"] = function_out;

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
    fn report_identity_comes_from_shared_build_metadata() {
        use common::scm_rev;
        let report = get_ruzu_version_data();
        assert_eq!(report.as_object().unwrap().len(), 7);
        for (name, expected) in [
            ("scm_rev", scm_rev::SCM_REV), ("scm_branch", scm_rev::SCM_BRANCH),
            ("scm_desc", scm_rev::SCM_DESC), ("build_name", scm_rev::BUILD_NAME),
            ("build_date", scm_rev::BUILD_DATE), ("build_fullname", scm_rev::BUILD_FULLNAME),
            ("build_version", scm_rev::BUILD_VERSION),
        ] {
            assert_eq!(report[name], expected);
        }
    }

    #[test]
    fn unimplemented_report_captures_request_and_only_input_buffer_data() {
        const CHILD: &str = "RUZU_TEST_IPC_REPORT_CONTEXT";
        if std::env::var_os(CHILD).is_none() {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "reporter::tests::unimplemented_report_captures_request_and_only_input_buffer_data"])
                .env(CHILD, "1").status().unwrap().success());
            return;
        }
        std::thread::Builder::new().stack_size(32 * 1024 * 1024).spawn(|| {
            use std::sync::{Arc, Mutex};
            use crate::core::{System, SystemRef};
            use crate::device_memory::DeviceMemory;
            use crate::hle::ipc;
            use crate::hle::kernel::k_process::{KProcess, ProcessLock};
            use crate::hle::service::hle_ipc::HLERequestContext;
            use crate::memory::memory::Memory;
            use common::page_table::{PageTable, PageType};
            use common::fs::path_util::{set_ruzu_path, RuzuPath};

            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let directory = std::env::temp_dir().join(format!("ruzu-ipc-report-{}-{nonce}", std::process::id()));
            fs::create_dir(&directory).unwrap();
            fs::create_dir(directory.join("sdmc")).unwrap();
            set_ruzu_path(RuzuPath::LogDir, &directory);
            set_ruzu_path(RuzuPath::SDMCDir, &directory.join("sdmc"));
            let reporter = Reporter::new();
            let mut ctx = HLERequestContext::new();
            settings::values_mut().reporting_services.set_value(false);
            reporter.save_unimplemented_function_report(SystemRef::null(), &ctx, 99, "SyntheticCommand", "test:report");
            assert!(!directory.join("unimpl_func_report").exists());

            let device = Box::new(DeviceMemory::new());
            let mut table = Box::new(PageTable::new());
            table.resize(32, 12);
            table.map_pages(3, 1, 0x3000, PageType::Memory,
                device.buffer.backing_base_pointer() as usize + 0x3000);
            let memory = Arc::new(Mutex::new(unsafe {
                Memory::new(SystemRef::null(), device.as_ref() as *const _, &device.buffer as *const _)
            }));
            memory.lock().unwrap().set_current_page_table(table.as_mut() as *mut _, true);
            memory.lock().unwrap().write_8(0x3000, 0xAB);
            memory.lock().unwrap().write_8(0x3001, 0xCD);
            memory.lock().unwrap().write_8(0x3010, 0xEF);
            let mut system = Box::new(System::new());
            let mut process = KProcess::new();
            process.program_id = 42;
            process.page_table.set_memory(memory.clone());
            system.set_current_process_arc(Arc::new(ProcessLock::new(process)));
            system.set_runtime_program_id(123); // Must use the process, not the launch cache.

            let mut words = [0u32; ipc::COMMAND_BUFFER_LENGTH];
            words[0] = ipc::CommandType::Request as u32 | (1 << 16) | (1 << 20) | (1 << 24);
            words[1] = 8 | ((ipc::BufferDescriptorCFlag::OneDescriptor as u32) << 10);
            words[2] = 1 << 16; // X: one byte at 0x3010.
            words[3] = 0x3010;
            words[4..7].copy_from_slice(&[2, 0x3000, 0]); // A
            words[7..10].copy_from_slice(&[16, 0x9000, 0]); // B, deliberately unmapped.
            words[12] = u32::from_le_bytes(*b"SFCI");
            words[14] = 99;
            words[18] = 0xA000; // C, also unmapped.
            words[19] = 32 << 16;
            words[ipc::COMMAND_BUFFER_LENGTH - 1] = 0xDEAD_BEEF;
            ctx.populate_from_incoming_command_buffer(&words);
            assert_eq!(ctx.get_command(), 99);
            assert!(ctx.get_memory().is_none()); // Read application memory, not ctx memory.
            let request_words = *ctx.command_buffer();
            settings::values_mut().reporting_services.set_value(true);
            reporter.save_unimplemented_function_report(SystemRef::from_ref(&system), &ctx,
                ctx.get_command(), "SyntheticCommand", "test:report");
            assert_eq!(ctx.command_buffer(), &request_words);
            // A later stub reply must not overwrite the saved input snapshot.
            crate::hle::service::ipc_helpers::ResponseBuilder::new(&mut ctx, 2, 0, 0)
                .push_result(crate::hle::result::RESULT_SUCCESS);
            let paths: Vec<_> = fs::read_dir(directory.join("unimpl_func_report")).unwrap().map(|e| e.unwrap().path()).collect();
            assert_eq!(paths.len(), 1);
            let report: serde_json::Value = serde_json::from_slice(&fs::read(&paths[0]).unwrap()).unwrap();
            assert_eq!(report["report_common"]["title_id"], "000000000000002A");
            let function = &report["function"];
            assert_eq!(function["command_id"], 99);
            assert_eq!(function["function_name"], "SyntheticCommand");
            assert_eq!(function["service_name"], "test:report");
            assert_eq!(function["command_buffer"], serde_json::json!(request_words.iter().map(|word| format!("{word:08X}")).collect::<Vec<_>>()));
            for (kind, address, size, data) in [
                ("a", 0x3000, 2, Some("ABCD")), ("x", 0x3010, 1, Some("EF")),
                ("b", 0x9000, 16, None), ("c", 0xA000, 32, None),
            ] {
                let entries = function[format!("buffer_descriptor_{kind}")].as_array().unwrap();
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0]["address"], format!("{address:016X}"));
                assert_eq!(entries[0]["size"], format!("{size:016X}"));
                assert_eq!(entries[0].get("data").and_then(|v| v.as_str()), data);
            }
            settings::values_mut().reporting_services.set_value(false);
            drop(system);
            drop(memory);
            drop(table);
            drop(device);
            fs::remove_dir_all(directory).unwrap();
        }).unwrap().join().unwrap();
    }

    #[test]
    fn diagnostic_payloads_preserve_hex_case_and_empty_channels() {
        const CHILD: &str = "RUZU_TEST_REPORT_PAYLOADS";
        if std::env::var_os(CHILD).is_none() {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "reporter::tests::diagnostic_payloads_preserve_hex_case_and_empty_channels"])
                .env(CHILD, "1").status().unwrap().success());
            return;
        }
        use common::fs::path_util::{set_ruzu_path, RuzuPath};
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let directory = std::env::temp_dir().join(format!("ruzu-report-data-{}-{nonce}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        set_ruzu_path(RuzuPath::LogDir, &directory);
        let sdmc = directory.join("sdmc");
        fs::create_dir(&sdmc).unwrap();
        set_ruzu_path(RuzuPath::SDMCDir, &sdmc);
        let read_report = |kind: &str| -> serde_json::Value {
            let files: Vec<_> = fs::read_dir(directory.join(kind)).unwrap().map(|e| e.unwrap().path()).collect();
            assert_eq!(files.len(), 1);
            let bytes = fs::read(&files[0]).unwrap();
            let newline = if cfg!(windows) { "\r\n" } else { "\n" };
            assert!(bytes.starts_with(format!("{{{newline}    \"").as_bytes()));
            assert!(bytes.ends_with(newline.as_bytes()));
            serde_json::from_slice(&bytes).unwrap()
        };
        settings::values_mut().reporting_services.set_value(false);
        let reporter = Reporter::new();
        reporter.save_svc_break_report(42, 0, false, 0, 0, Some(&[0xAB]));
        assert!(!directory.join("svc_break_report").exists());
        settings::values_mut().reporting_services.set_value(true);
        reporter.save_svc_break_report(42, 0, false, 0, 0, Some(&[0xAB, 0xCD]));
        assert_eq!(read_report("svc_break_report")["svc_break"]["debug_buffer"], "ABCD");
        reporter.save_unimplemented_applet_report(42, 0, 0, 0, 0, false, 0,
            &[vec![0xEF], vec![]], &[vec![0xAB, 0xCD]]);
        let report = read_report("unimpl_applet_report");
        assert_eq!(report["applet_normal_data"], serde_json::json!(["EF", ""]));
        assert_eq!(report["applet_interactive_data"], serde_json::json!(["ABCD"]));
        settings::values_mut().reporting_services.set_value(false);
        fs::remove_dir_all(directory).unwrap();
    }

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
