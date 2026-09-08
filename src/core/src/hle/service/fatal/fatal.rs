// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/fatal/fatal.h
//! Port of zuyu/src/core/hle/service/fatal/fatal.cpp
//!
//! Fatal error service -- Module::Interface, FatalInfo, error report generation.

/// Architecture of the faulting process.
///
/// Corresponds to `FatalInfo::Architecture` in upstream fatal.cpp.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Architecture {
    AArch64 = 0,
    AArch32 = 1,
}


/// CPU context captured at the time of fatal error.
///
/// Corresponds to `FatalInfo` in upstream fatal.cpp.
/// `static_assert(sizeof(FatalInfo) == 0x250)`.
#[repr(C)]
#[derive(Clone)]
pub struct FatalInfo {
    pub registers: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    pub pstate: u64,
    pub afsr0: u64,
    pub afsr1: u64,
    pub esr: u64,
    pub far: u64,
    pub backtrace: [u64; 32],
    pub program_entry_point: u64,
    /// Bit flags indicating which registers have been set with values.
    pub set_flags: u64,
    pub backtrace_size: u32,
    /// Raw C++ enum storage: every guest bit pattern must remain valid Rust.
    pub arch: i32,
    pub unk10: u32,
}
const _: () = assert!(std::mem::size_of::<FatalInfo>() == 0x250);

impl FatalInfo {
    pub fn arch_as_string(&self) -> &'static str {
        if self.arch == Architecture::AArch64 as i32 { "AArch64" } else { "AArch32" }
    }
}

impl Default for FatalInfo {
    fn default() -> Self {
        // Zero-initialize the entire struct, matching upstream memset behavior.
        // SAFETY: FatalInfo is repr(C) with all-numeric fields.
        unsafe { std::mem::zeroed() }
    }
}

/// Fatal error type policy.
///
/// Corresponds to `FatalType` in upstream fatal.cpp.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatalType {
    ErrorReportAndScreen = 0,
    ErrorReport = 1,
    ErrorScreen = 2,
}


/// Generate a human-readable crash report from fatal info.
///
/// Corresponds to `GenerateErrorReport` in upstream fatal.cpp.
pub fn generate_error_report(system: crate::core::SystemRef, error_code: u32, info: &FatalInfo) {
    let title_id = system.get().get_application_process_program_id();
    let module = error_code & 0x1FF;
    let description = (error_code >> 9) & 0x1FFF;
    let mut crash_report = format!(
        "Ruzu {}-{} crash report\n\
         Title ID:                        {:016x}\n\
         Result:                          {:#x} ({:04}-{:04})\n\
         Set flags:                       {:#16X}\n\
         Program entry point:             {:#16X}\n\
         \n",
        common::scm_rev::SCM_BRANCH,
        common::scm_rev::SCM_DESC,
        title_id,
        error_code,
        2000 + module,
        description,
        info.set_flags,
        info.program_entry_point
    );

    if info.backtrace_size != 0 {
        crash_report += "Registers:\n";
        for i in 0..info.registers.len() {
            crash_report += &format!(
                "    X[{:02}]:                       {:016x}\n",
                i, info.registers[i]
            );
        }
        crash_report += &format!("    SP:                          {:016x}\n", info.sp);
        crash_report += &format!("    PC:                          {:016x}\n", info.pc);
        crash_report += &format!("    PSTATE:                      {:016x}\n", info.pstate);
        crash_report += &format!("    AFSR0:                       {:016x}\n", info.afsr0);
        crash_report += &format!("    AFSR1:                       {:016x}\n", info.afsr1);
        crash_report += &format!("    ESR:                         {:016x}\n", info.esr);
        crash_report += &format!("    FAR:                         {:016x}\n", info.far);
        crash_report += "\nBacktrace:\n";
        for i in 0..std::cmp::min(info.backtrace_size, 32) {
            crash_report += &format!(
                "    Backtrace[{:02}]:               {:016x}\n",
                i, info.backtrace[i as usize]
            );
        }
        crash_report += &format!("Architecture:                    {}\n", info.arch_as_string());
        crash_report += &format!("Unknown 10:                      {:#016x}\n", info.unk10);
    }

    log::error!("{}", crash_report);
    system.get_reporter().save_crash_report(
        title_id, error_code, info.set_flags, info.program_entry_point, info.sp, info.pc,
        info.pstate, info.afsr0, info.afsr1, info.esr, info.far, &info.registers,
        &info.backtrace, info.backtrace_size, info.arch_as_string(), info.unk10,
    );
}

/// Process a fatal error according to the error type policy.
///
/// Corresponds to `ThrowFatalError` in upstream fatal.cpp.
pub fn throw_fatal_error(
    system: crate::core::SystemRef,
    error_code: u32,
    fatal_type: u32,
    info: &FatalInfo,
) {
    log::error!("Threw fatal error type {} with error code {:#x}", fatal_type, error_code);
    match fatal_type {
        0 => {
            generate_error_report(system, error_code, info);
            log::error!("assert false: fatal error screen is not implemented");
            common::assert::assert_fail_soft_impl();
        }
        1 => generate_error_report(system, error_code, info),
        2 => {
            log::error!("assert false: fatal error screen is not implemented");
            common::assert::assert_fail_soft_impl();
        }
        _ => {} // C++ switches on the raw enum without a default handler.
    }
}

/// Module for fatal service, shared by fatal:p and fatal:u.
///
/// Corresponds to `Module` in upstream fatal.h.
pub struct Module;

impl Module {
    pub fn new() -> Self {
        Self
    }
}

/// Module::Interface -- base type for fatal:p and fatal:u.
///
/// Corresponds to `Module::Interface` in upstream fatal.h / fatal.cpp.
pub struct Interface {
    pub system: crate::core::SystemRef,
    pub module: std::sync::Arc<Module>,
    pub name: &'static str,
}

impl Interface {
    // Trait bridges remain in fatal_u.rs, but parsing and response ownership
    // belong to Module::Interface here, as in the C++ implementation.
    pub fn throw_fatal_handler(&self, ctx: &mut crate::hle::service::hle_ipc::HLERequestContext) {
        let error_code = crate::hle::service::ipc_helpers::RequestParser::new(ctx).pop_u32();
        self.throw_fatal(error_code);
        let mut rb = crate::hle::service::ipc_helpers::ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(crate::hle::result::RESULT_SUCCESS);
    }

    pub fn throw_fatal_with_policy_handler(&self, ctx: &mut crate::hle::service::hle_ipc::HLERequestContext) {
        let mut rp = crate::hle::service::ipc_helpers::RequestParser::new(ctx);
        let error_code = rp.pop_u32();
        let fatal_type = rp.pop_u32();
        self.throw_fatal_with_policy(error_code, fatal_type);
        let mut rb = crate::hle::service::ipc_helpers::ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(crate::hle::result::RESULT_SUCCESS);
    }

    pub fn throw_fatal_with_cpu_context_handler(&self, ctx: &mut crate::hle::service::hle_ipc::HLERequestContext) {
        let mut rp = crate::hle::service::ipc_helpers::RequestParser::new(ctx);
        let error_code = rp.pop_u32();
        let fatal_type = rp.pop_u32();
        let fatal_info = ctx.read_buffer(0);
        self.throw_fatal_with_cpu_context(error_code, fatal_type, &fatal_info);
        let mut rb = crate::hle::service::ipc_helpers::ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(crate::hle::result::RESULT_SUCCESS);
    }

    pub fn new(
        system: crate::core::SystemRef,
        module: std::sync::Arc<Module>,
        name: &'static str,
    ) -> Self {
        Self {
            system,
            module,
            name,
        }
    }

    /// ThrowFatal (cmd 0).
    ///
    /// Corresponds to `Module::Interface::ThrowFatal` in upstream fatal.cpp.
    pub fn throw_fatal(&self, error_code: u32) {
        log::error!("fatal ThrowFatal called");
        throw_fatal_error(
            self.system,
            error_code,
            FatalType::ErrorScreen as u32,
            &FatalInfo::default(),
        );
    }

    /// ThrowFatalWithPolicy (cmd 1).
    ///
    /// Corresponds to `Module::Interface::ThrowFatalWithPolicy` in upstream fatal.cpp.
    pub fn throw_fatal_with_policy(&self, error_code: u32, fatal_type: u32) {
        log::error!("fatal ThrowFatalWithPolicy called");
        throw_fatal_error(self.system, error_code, fatal_type, &FatalInfo::default());
    }

    /// ThrowFatalWithCpuContext (cmd 2).
    ///
    /// Corresponds to `Module::Interface::ThrowFatalWithCpuContext` in upstream fatal.cpp.
    pub fn throw_fatal_with_cpu_context(
        &self,
        error_code: u32,
        fatal_type: u32,
        fatal_info_buffer: &[u8],
    ) {
        log::error!("fatal ThrowFatalWithCpuContext called");
        let mut info = FatalInfo::default();

        assert!(
            fatal_info_buffer.len() == std::mem::size_of::<FatalInfo>(),
            "Invalid fatal info buffer size!"
        );
        // SAFETY: FatalInfo is repr(C), buffer size is checked
        unsafe {
            std::ptr::copy_nonoverlapping(
                fatal_info_buffer.as_ptr(),
                &mut info as *mut FatalInfo as *mut u8,
                std::mem::size_of::<FatalInfo>(),
            );
        }

        // Match the upstream little-endian scalar wrappers after the raw copy.
        for value in info.registers.iter_mut().chain(info.backtrace.iter_mut()) {
            *value = u64::from_le(*value);
        }
        for value in [&mut info.sp, &mut info.pc, &mut info.pstate, &mut info.afsr0,
            &mut info.afsr1, &mut info.esr, &mut info.far, &mut info.program_entry_point,
            &mut info.set_flags] {
            *value = u64::from_le(*value);
        }
        info.backtrace_size = u32::from_le(info.backtrace_size);
        info.unk10 = u32::from_le(info.unk10);
        throw_fatal_error(self.system, error_code, fatal_type, &info);
    }
}

/// LoopProcess -- registers fatal:p and fatal:u services.
///
/// Corresponds to `LoopProcess` in upstream fatal.cpp.
pub fn loop_process(system: crate::core::SystemRef) {
    use crate::hle::service::hle_ipc::SessionRequestHandlerPtr;
    use crate::hle::service::server_manager::ServerManager;

    log::debug!("Fatal::LoopProcess called");

    let module = std::sync::Arc::new(Module::new());
    let server_manager = ServerManager::new_shared(system);
    {
        let mut server_manager = server_manager.lock().unwrap();

        let m1 = module.clone();
        server_manager.register_named_service(
            "fatal:p",
            Box::new(move || -> SessionRequestHandlerPtr {
                std::sync::Arc::new(super::fatal_p::FatalP::new(m1.clone(), system))
            }),
            64,
        );

        let m2 = module.clone();
        server_manager.register_named_service(
            "fatal:u",
            Box::new(move || -> SessionRequestHandlerPtr {
                std::sync::Arc::new(super::fatal_u::FatalU::new(m2.clone(), system))
            }),
            64,
        );
    }

    ServerManager::run_server_shared(server_manager);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fatal_context_layout_accepts_raw_architecture_values() {
        use std::mem::{align_of, offset_of, size_of};
        assert_eq!(size_of::<FatalInfo>(), 0x250);
        assert_eq!(align_of::<FatalInfo>(), 8);
        assert_eq!(offset_of!(FatalInfo, sp), 0xF8);
        assert_eq!(offset_of!(FatalInfo, backtrace), 0x130);
        assert_eq!(offset_of!(FatalInfo, program_entry_point), 0x230);
        assert_eq!(offset_of!(FatalInfo, set_flags), 0x238);
        assert_eq!(offset_of!(FatalInfo, backtrace_size), 0x240);
        assert_eq!(offset_of!(FatalInfo, arch), 0x244);
        assert_eq!(offset_of!(FatalInfo, unk10), 0x248);
        for (raw, expected) in [(0, "AArch64"), (1, "AArch32"), (-1, "AArch32"), (i32::MAX, "AArch32")] {
            let info = FatalInfo { arch: raw, ..FatalInfo::default() };
            assert_eq!(info.arch_as_string(), expected);
        }
    }

    #[test]
    fn invalid_context_sizes_are_rejected_before_copying_guest_bytes() {
        let interface = Interface::new(crate::core::SystemRef::null(),
            std::sync::Arc::new(Module::new()), "fatal:u");
        for len in [0, 1, 0x24F, 0x251] {
            let buffer = vec![0; len];
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                interface.throw_fatal_with_cpu_context(0, FatalType::ErrorReport as u32, &buffer);
            })).unwrap_err();
            let message = panic.downcast_ref::<String>().map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied()).unwrap_or("");
            assert!(message.contains("Invalid fatal info buffer size"));
        }
    }

    #[test]
    fn fatal_reports_use_system_identity_policy_and_wire_context() {
        const CHILD: &str = "RUZU_TEST_FATAL_REPORTS";
        if std::env::var_os(CHILD).is_none() {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "hle::service::fatal::fatal::tests::fatal_reports_use_system_identity_policy_and_wire_context"])
                .env(CHILD, "1").status().unwrap().success());
            return;
        }
        std::thread::Builder::new().stack_size(32 * 1024 * 1024).spawn(|| {
            use std::sync::Arc;
            use crate::core::{System, SystemRef};
            use crate::hle::kernel::k_process::{KProcess, ProcessLock};
            use crate::hle::service::service::ServiceFramework;
            use crate::hle::service::hle_ipc::HLERequestContext;
            use common::fs::path_util::{set_ruzu_path, RuzuPath};
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let directory = std::env::temp_dir().join(format!("ruzu-fatal-report-{}-{nonce}", std::process::id()));
            std::fs::create_dir(&directory).unwrap();
            std::fs::create_dir(directory.join("sdmc")).unwrap();
            set_ruzu_path(RuzuPath::LogDir, &directory);
            set_ruzu_path(RuzuPath::SDMCDir, &directory.join("sdmc"));
            let mut system = Box::new(System::new());
            let mut process = KProcess::new();
            process.program_id = 42;
            system.set_current_process_arc(Arc::new(ProcessLock::new(process)));
            system.set_runtime_program_id(99);
            let module = Arc::new(Module::new());
            let service = super::super::fatal_u::FatalU::new(module.clone(), SystemRef::from_ref(&system));
            let private = super::super::fatal_p::FatalP::new(module, SystemRef::from_ref(&system));
            assert!(!private.interface.system.is_null());
            assert!(private.handlers().values().all(|h| h.handler_callback.is_none()));
            let reports = directory.join("crash_report");
            let read_report = || -> serde_json::Value {
                let paths: Vec<_> = std::fs::read_dir(&reports).unwrap().map(|e| e.unwrap().path()).collect();
                assert_eq!(paths.len(), 1);
                let report = serde_json::from_slice(&std::fs::read(&paths[0]).unwrap()).unwrap();
                std::fs::remove_file(&paths[0]).unwrap();
                report
            };
            let invoke_policy = |policy| {
                let mut ctx = HLERequestContext::new();
                ctx.command_buffer_mut()[2] = 0x1234;
                ctx.command_buffer_mut()[3] = policy;
                service.handlers()[&1].handler_callback.unwrap()(&service, &mut ctx);
                assert_eq!(ctx.write_size, 8);
                assert_eq!(ctx.command_buffer()[6], 0);
            };
            common::settings::values_mut().reporting_services.set_value(false);
            invoke_policy(FatalType::ErrorReport as u32);
            assert!(!reports.exists());
            common::settings::values_mut().reporting_services.set_value(true);
            common::settings::values_mut().use_debug_asserts.set_value(false);
            invoke_policy(u32::MAX); // Unknown enum values are not coerced to policy zero.
            assert!(!reports.exists());
            invoke_policy(FatalType::ErrorReport as u32);
            let report = read_report();
            assert_eq!(report["report_common"]["title_id"], "000000000000002A");
            assert_eq!(report["report_common"]["result_raw"], "00001234");
            assert_eq!(report["processor_state"]["architecture"], "AArch64");
            assert_eq!(report["processor_state"]["backtrace_size"], "00000000");
            invoke_policy(FatalType::ErrorReportAndScreen as u32);
            assert_eq!(read_report()["report_common"]["title_id"], "000000000000002A");
            invoke_policy(FatalType::ErrorScreen as u32);
            assert_eq!(std::fs::read_dir(&reports).unwrap().count(), 0);

            let mut wire = [0xA5u8; 0x250];
            for index in 0..72usize {
                wire[index * 8..index * 8 + 8].copy_from_slice(&(0xABC0 + index as u64).to_le_bytes());
            }
            wire[0x240..0x244].copy_from_slice(&40u32.to_le_bytes());
            wire[0x244..0x248].copy_from_slice(&(-1i32).to_ne_bytes());
            wire[0x248..0x24C].copy_from_slice(&0xABCDu32.to_le_bytes());
            service.interface.throw_fatal_with_cpu_context(0x1234, FatalType::ErrorReport as u32, &wire);
            let report = read_report();
            let cpu = &report["processor_state"];
            assert_eq!(report["report_common"]["title_id"], "000000000000002A");
            assert_eq!(cpu["architecture"], "AArch32");
            assert_eq!(cpu["backtrace_size"], "00000028");
            assert_eq!(cpu["backtrace"].as_array().unwrap().len(), 32);
            for i in 0..31 {
                assert_eq!(cpu["registers"][format!("X{i:02}")], format!("{:016X}", 0xABC0 + i));
            }
            for (name, offset) in [("sp", 0xF8), ("pc", 0x100), ("pstate", 0x108),
                ("afsr0", 0x110), ("afsr1", 0x118), ("esr", 0x120), ("far", 0x128),
                ("entry_point", 0x230), ("set_flags", 0x238)] {
                assert_eq!(cpu[name], format!("{:016X}", 0xABC0 + offset / 8));
            }
            for i in 0..32 { assert_eq!(cpu["backtrace"][i], format!("{:016X}", 0xABC0 + 0x130 / 8 + i)); }
            assert_eq!(cpu["unknown_10"], "0000ABCD");
            common::settings::values_mut().reporting_services.set_value(false);
            drop(private);
            drop(service);
            drop(system);
            std::fs::remove_dir_all(directory).unwrap();
        }).unwrap().join().unwrap();
    }
}
