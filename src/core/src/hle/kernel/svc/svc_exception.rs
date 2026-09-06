//! Counterpart of Eden core/hle/kernel/svc/svc_exception.cpp.
//!
//! SVC handlers for Break and ReturnFromException.

use crate::core::System;
use crate::hle::result::ResultCode;

/// Break program execution.
///
/// Upstream logs the break reason, handles debug buffers, saves a break report,
/// and optionally notifies the debugger.
pub fn break_execution(system: &System, reason: u32, info1: u64, info2: u64) {
    let break_reason_raw = reason & !0x80000000u32;
    let notification_only = (reason & 0x80000000) != 0;

    let mut has_dumped_buffer = false;
    let mut debug_buffer = Vec::new();

    // Same local lambda and capture state as upstream. Retain the bytes for
    // Reporter; a four-byte error code counts as dumped but leaves the vector empty.
    let handle_debug_buffer = |addr: u64, sz: u64, dumped: &mut bool, debug_buffer: &mut Vec<u8>| {
        if sz == 0 || addr == 0 || *dumped {
            return;
        }
        if let Some(memory) = system.get_svc_memory() {
            let memory = memory.lock().unwrap();
            if sz == 4 {
                log::error!("debug_buffer_err_code={:X}", memory.read_32(addr));
            } else {
                debug_buffer.resize(sz as usize, 0);
                memory.read_block(addr, debug_buffer);
            }
        } else {
            // Existing bootstrap memory adaptation, without an initialized Memory bridge.
            let memory = system.shared_process_memory().read().unwrap();
            if sz == 4 {
                log::error!("debug_buffer_err_code={:X}", memory.read_32(addr));
            } else {
                debug_buffer.resize(sz as usize, 0);
                for (i, byte) in debug_buffer.iter_mut().enumerate() {
                    *byte = memory.read_8(addr + i as u64);
                }
            }
        }
        if sz != 4 {
            let mut hexdump = String::new();
            for (i, byte) in debug_buffer.iter().enumerate() {
                hexdump.push_str(&format!("{byte:02X} "));
                if (i + 1) % 32 == 0 {
                    hexdump.push('\n');
                }
            }
            log::error!("debug_buffer=\n{hexdump}");
        }
        *dumped = true;
    };

    // Match upstream break reason handling.
    match break_reason_raw {
        0 => {
            // BreakReason::Panic
            log::error!(
                "Userspace PANIC! info1=0x{:016X}, info2=0x{:016X}",
                info1,
                info2
            );
            handle_debug_buffer(info1, info2, &mut has_dumped_buffer, &mut debug_buffer);
        }
        1 => {
            // BreakReason::Assert
            log::error!(
                "Userspace Assertion failed! info1=0x{:016X}, info2=0x{:016X}",
                info1,
                info2
            );
            handle_debug_buffer(info1, info2, &mut has_dumped_buffer, &mut debug_buffer);
        }
        2 => {
            // BreakReason::User
            log::warn!(
                "Userspace Break! 0x{:016X} with size 0x{:016X}",
                info1,
                info2
            );
            handle_debug_buffer(info1, info2, &mut has_dumped_buffer, &mut debug_buffer);
        }
        3 => {
            // BreakReason::PreLoadDll
            log::info!(
                "Userspace Attempting to load an NRO at 0x{:016X} with size 0x{:016X}",
                info1,
                info2
            );
        }
        4 => {
            // BreakReason::PostLoadDll
            log::info!(
                "Userspace Loaded an NRO at 0x{:016X} with size 0x{:016X}",
                info1,
                info2
            );
        }
        5 => {
            // BreakReason::PreUnloadDll
            log::info!(
                "Userspace Attempting to unload an NRO at 0x{:016X} with size 0x{:016X}",
                info1,
                info2
            );
        }
        6 => {
            // BreakReason::PostUnloadDll
            log::info!(
                "Userspace Unloaded an NRO at 0x{:016X} with size 0x{:016X}",
                info1,
                info2
            );
        }
        7 => {
            // BreakReason::CppException
            log::error!("Signalling debugger. Uncaught C++ exception encountered.");
        }
        _ => {
            log::warn!(
                "Signalling debugger, Unknown break reason {:#X}, info1=0x{:016X}, info2=0x{:016X}",
                reason,
                info1,
                info2
            );
            handle_debug_buffer(info1, info2, &mut has_dumped_buffer, &mut debug_buffer);
        }
    }

    // Reporter is standalone in Rust. Keep the upstream reporting gate before
    // resolving the application identity, then save before the late dump/backtrace.
    if *common::settings::values().reporting_services.get_value() {
        system.reporter.save_svc_break_report(
            system.get_application_process_program_id(),
            reason, notification_only, info1, info2,
            has_dumped_buffer.then_some(debug_buffer.as_slice()),
        );
    }

    if !notification_only {
        log::error!(
            "Emulated program broke execution! reason=0x{:016X}, info1=0x{:016X}, info2=0x{:016X}",
            reason as u64,
            info1,
            info2
        );

        handle_debug_buffer(info1, info2, &mut has_dumped_buffer, &mut debug_buffer);

        if let Some(kernel) = system.kernel() {
            kernel.current_physical_core().log_backtrace();
        }
    }

    // Upstream: Debugger notification.
    // const bool is_hbl = GetCurrentProcess(kernel).IsHbl();
    // const bool should_break = is_hbl || !notification_only;
    // if (system.DebuggerEnabled() && should_break) {
    //     auto* thread = system.Kernel().GetCurrentEmuThread();
    //     system.GetDebugger().NotifyThreadStopped(thread);
    //     thread->RequestSuspend(SuspendType::Debug);
    // }
    // Debugger not yet ported — when available, wire up debugger notification here.
}

/// Upstream `Break64From32` forwards to `Break`, but the Rust AArch32 dispatch
/// can also pass the captured guest argument registers for optional diagnostics.
pub fn break64_from_32(system: &System, reason: u32, arg: u32, size: u32, args: &[u64]) {
    dump_a32_break_context(system, arg as u64, size as u64, args);
    break_execution(system, reason, arg as u64, size as u64);
}

/// Upstream `Break64` wrapper.
pub fn break64(system: &System, reason: u32, arg: u64, size: u64) {
    if common::trace::is_enabled(common::trace::cat::A64_BREAK_CTX) {
        let core = system
            .kernel()
            .map(|kernel| kernel.current_physical_core_index() as usize)
            .unwrap_or(0);
        let tid = crate::hle::kernel::kernel::get_current_thread_id_fast().unwrap_or(0);
        let pc = crate::hle::kernel::kernel::GUEST_PC
            .get(core)
            .map(|value| value.load(std::sync::atomic::Ordering::Acquire))
            .unwrap_or(0);
        let lr = crate::hle::kernel::kernel::GUEST_LR
            .get(core)
            .map(|value| value.load(std::sync::atomic::Ordering::Acquire))
            .unwrap_or(0);
        let sp = crate::hle::kernel::kernel::GUEST_SP
            .get(core)
            .map(|value| value.load(std::sync::atomic::Ordering::Acquire))
            .unwrap_or(0);
        common::trace::emit_raw(
            common::trace::cat::A64_BREAK_CTX,
            &[
                0,
                reason as u64,
                core as u64,
                tid,
                pc,
                lr,
                sp,
                arg,
                size,
                reason as u64,
                0,
                0,
                0,
                0,
            ],
        );
        if let Some(memory) = system.get_svc_memory() {
            let memory = memory.lock().unwrap();
            for offset in (0..0x100).step_by(0x20) {
                let base = sp + offset;
                common::trace::emit_raw(
                    common::trace::cat::A64_BREAK_CTX,
                    &[
                        1,
                        base,
                        memory.read_64(base),
                        memory.read_64(base + 8),
                        memory.read_64(base + 16),
                        memory.read_64(base + 24),
                    ],
                );
            }
            if arg != 0 {
                for offset in (0..0x80).step_by(0x20) {
                    let base = arg + offset;
                    if memory.is_valid_virtual_address_range(base, 0x20) {
                        let values = [
                            memory.read_64(base),
                            memory.read_64(base + 8),
                            memory.read_64(base + 16),
                            memory.read_64(base + 24),
                        ];
                        common::trace::emit_raw(
                            common::trace::cat::A64_BREAK_CTX,
                            &[2, base, values[0], values[1], values[2], values[3]],
                        );
                        for &ptr in &values {
                            if ptr == 0 || !memory.is_valid_virtual_address_range(ptr, 1) {
                                continue;
                            }
                            let mut raw = [0u64; 4];
                            let mut len = 0usize;
                            for index in 0..32usize {
                                if !memory.is_valid_virtual_address_range(ptr + index as u64, 1) {
                                    break;
                                }
                                let byte = memory.read_8(ptr + index as u64);
                                raw[index / 8] |= (byte as u64) << ((index % 8) * 8);
                                len += 1;
                                if byte == 0 {
                                    break;
                                }
                            }
                            common::trace::emit_raw(
                                common::trace::cat::A64_BREAK_CTX,
                                &[3, ptr, len as u64, raw[0], raw[1], raw[2], raw[3]],
                            );
                        }
                    }
                }
            }
        }
    }
    dump_a64_break_context(system, arg, size);
    log::error!(
        "!!! svcBreak(reason={:#x}, info1={:#x}, info2={:#x}) - GAME ABORTED !!!",
        reason,
        arg,
        size
    );
    break_execution(system, reason, arg, size);
}

fn dump_a64_break_context(system: &System, info1: u64, info2: u64) {
    if std::env::var_os("RUZU_DUMP_BREAK_STACK").is_none() {
        return;
    }

    let Some(kernel) = system.kernel() else {
        return;
    };
    let core_index = kernel.current_physical_core_index() as usize;
    let Some(process_arc) = system.current_process_arc_opt() else {
        return;
    };
    let process = process_arc.lock().unwrap();
    let Some(jit) = process.get_arm_interface(core_index) else {
        return;
    };

    let mut ctx = crate::arm::arm_interface::ThreadContext::default();
    jit.get_context(&mut ctx);
    log::error!("=== A64 GUEST REGISTER DUMP AT BREAK ===");
    log::error!(
        "  break_pc=0x{:016X} break_lr=0x{:016X} break_sp=0x{:016X} break_fp=0x{:016X} core={}",
        ctx.pc,
        ctx.lr,
        ctx.sp,
        ctx.fp,
        core_index
    );
    for index in 0..8 {
        log::error!("  x{:2} = 0x{:016X}", index, ctx.r[index]);
    }
    for index in 19..=28 {
        log::error!("  x{:2} = 0x{:016X}", index, ctx.r[index]);
    }

    let backtrace = crate::arm::debug::get_backtrace_from_context(&process, &ctx);
    for (index, entry) in backtrace.iter().enumerate().take(32) {
        log::error!(
            "  break_bt[{}] addr=0x{:016X} orig=0x{:016X} module={} name={} offset=0x{:X}",
            index,
            entry.address,
            entry.original_address,
            entry.module,
            entry.name,
            entry.offset,
        );
    }

    if info1 != 0 && info2 > 0 && info2 < 0x200 {
        let len = info2 as usize;
        let Some(memory) = process.get_memory() else {
            return;
        };
        let mem = memory.lock().unwrap();
        if info1.checked_add(info2).is_some() && mem.is_valid_virtual_address_range(info1, info2) {
            let mut hexdump = String::new();
            for index in 0..len {
                let byte = mem.read_8(info1 + index as u64);
                hexdump.push_str(&format!("{:02X} ", byte));
            }
            log::error!("  break_debug_buffer={}", hexdump.trim_end());
        }
    }

    dump_a64_stack_scan(&process, ctx.sp);
}

fn dump_a64_stack_scan(process: &crate::hle::kernel::k_process::KProcess, sp: u64) {
    const STACK_SCAN_BYTES: usize = 0x400;
    const STACK_DUMP_BYTES: usize = 0x100;

    let Some(memory) = process.get_memory() else {
        return;
    };
    let mem = memory.lock().unwrap();
    if sp.checked_add(STACK_SCAN_BYTES as u64).is_none()
        || !mem.is_valid_virtual_address_range(sp, 8)
    {
        log::error!("  break_stack: sp=0x{:016X} is not mapped", sp);
        return;
    }

    let dump_len = (0..STACK_DUMP_BYTES)
        .step_by(8)
        .take_while(|offset| mem.is_valid_virtual_address_range(sp + *offset as u64, 8))
        .count()
        * 8;
    log::error!(
        "  break_stack_qwords sp=0x{:016X} dump_len=0x{:X}",
        sp,
        dump_len
    );
    for offset in (0..dump_len).step_by(0x20) {
        let mut values = [0u64; 4];
        for (index, value) in values.iter_mut().enumerate() {
            let addr = sp + offset as u64 + (index as u64 * 8);
            if mem.is_valid_virtual_address_range(addr, 8) {
                *value = mem.read_64(addr);
            }
        }
        log::error!(
            "  stack[+0x{:03X}] {:016X} {:016X} {:016X} {:016X}",
            offset,
            values[0],
            values[1],
            values[2],
            values[3]
        );
    }

    for offset in (0..STACK_SCAN_BYTES).step_by(8) {
        let addr = sp + offset as u64;
        if !mem.is_valid_virtual_address_range(addr, 8) {
            break;
        }
        let value = mem.read_64(addr);
        if looks_like_animus_code_address(value) {
            log::error!(
                "  stack_code_candidate sp+0x{:03X}=0x{:016X}",
                offset,
                value
            );
        }
    }
}

fn looks_like_animus_code_address(value: u64) -> bool {
    // ANIMUS' executable NSOs observed in this investigation are mapped in
    // the 0x8000_0000..0x8520_0000 region. Keep the check deliberately broad
    // so this diagnostic still works across ASLR-disabled local runs.
    (0x8000_0000..0x8600_0000).contains(&value)
}

fn dump_a32_break_context(system: &System, info1: u64, info2: u64, args: &[u64]) {
    log::error!("=== GUEST REGISTER DUMP AT BREAK ===");
    for (index, value) in args.iter().enumerate() {
        log::error!("  r{:2} = {:#010x}", index, value);
    }

    if let Some(kernel) = system.kernel() {
        let core_index = kernel.current_physical_core_index() as usize;
        if let Some(process_arc) = system.current_process_arc_opt() {
            let process = process_arc.lock().unwrap();
            if let Some(jit) = process.get_arm_interface(core_index) {
                let mut ctx = crate::arm::arm_interface::ThreadContext::default();
                jit.get_context(&mut ctx);
                log::error!(
                    "  break_pc=0x{:08X} break_lr=0x{:08X} break_sp=0x{:08X} core={}",
                    ctx.r[15] as u32,
                    ctx.r[14] as u32,
                    ctx.r[13] as u32,
                    core_index
                );
                if std::env::var_os("RUZU_DUMP_BREAK_STACK").is_some() {
                    let backtrace = crate::arm::debug::get_backtrace_from_context(&process, &ctx);
                    for (index, entry) in backtrace.iter().enumerate().take(32) {
                        log::error!(
                            "  break_bt[{}] addr=0x{:08X} orig=0x{:08X} module={} name={} offset=0x{:X}",
                            index,
                            entry.address as u32,
                            entry.original_address as u32,
                            entry.module,
                            entry.name,
                            entry.offset,
                        );
                    }
                }
            }
        }
    }

    if info1 != 0 && info2 > 0 && info2 < 0x200 {
        let len = info2 as usize;
        let mut buf = vec![0u8; len];
        if let Some(memory) = system.get_svc_memory() {
            let m = memory.lock().unwrap();
            m.read_block(info1, &mut buf);
        } else {
            let process_arc = system.current_process_arc();
            let process = process_arc.lock().unwrap();
            let mem = process.process_memory.read().unwrap();
            if mem.is_valid_range(info1, len) {
                for (index, byte) in buf.iter_mut().enumerate() {
                    *byte = mem.read_8(info1 + index as u64);
                }
            }
        }
        if let Ok(msg) = String::from_utf8(buf) {
            log::error!("  Break message: {}", msg.trim_end_matches('\0'));
        }
    }

    if std::env::var_os("RUZU_DUMP_BREAK_STACK").is_some() {
        dump_known_break_strings(system);
    }

    log::error!(
        "!!! svcBreak(reason={:#x}, info1={:#x}, info2={:#x}) - GAME ABORTED !!!",
        args.first().copied().unwrap_or_default() as u32,
        info1,
        info2
    );
}

fn dump_known_break_strings(system: &System) {
    let Some(memory) = system.get_svc_memory() else {
        return;
    };
    let m = memory.lock().unwrap();
    for addr in [0x20bc7fau64, 0x20bc827, 0x2244ec7] {
        let mut buf = vec![0u8; 128];
        for index in 0..128u64 {
            buf[index as usize] = m.read_8(addr + index);
        }
        if let Some(end) = buf.iter().position(|&byte| byte == 0) {
            buf.truncate(end);
        }
        if let Ok(value) = String::from_utf8(buf) {
            if !value.is_empty() && value.len() < 120 {
                log::error!("  [{addr:#x}] = \"{value}\"");
            }
        }
    }
}

/// Return from exception.
/// Upstream: UNIMPLEMENTED() — intentionally unimplemented.
pub fn return_from_exception(_result: ResultCode) {
    log::warn!("svc::ReturnFromException: Upstream UNIMPLEMENTED");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn break_report_preserves_optional_buffer_and_capture_order() {
        const CHILD: &str = "RUZU_TEST_SVC_BREAK_REPORT";
        if std::env::var_os(CHILD).is_none() {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "hle::kernel::svc::svc_exception::tests::break_report_preserves_optional_buffer_and_capture_order"])
                .env(CHILD, "1").status().unwrap().success());
            return;
        }
        std::thread::Builder::new().stack_size(32 * 1024 * 1024).spawn(|| {
            use std::sync::{Arc, Mutex, RwLock};
            use crate::core::SystemRef;
            use crate::device_memory::DeviceMemory;
            use crate::hle::kernel::k_process::{KProcess, ProcessLock, ProcessMemoryData};
            use crate::memory::memory::Memory;
            use common::page_table::{PageTable, PageType};
            use common::fs::path_util::{set_ruzu_path, RuzuPath};
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let directory = std::env::temp_dir().join(format!("ruzu-break-report-{}-{nonce}", std::process::id()));
            std::fs::create_dir(&directory).unwrap();
            std::fs::create_dir(directory.join("sdmc")).unwrap();
            set_ruzu_path(RuzuPath::LogDir, &directory);
            set_ruzu_path(RuzuPath::SDMCDir, &directory.join("sdmc"));
            let device = Box::new(DeviceMemory::new());
            let mut table = Box::new(PageTable::new());
            table.resize(32, 12);
            table.map_pages(3, 1, 0x3000, PageType::Memory,
                device.buffer.backing_base_pointer() as usize + 0x3000);
            let memory = Arc::new(Mutex::new(unsafe {
                Memory::new(SystemRef::null(), device.as_ref() as *const _, &device.buffer as *const _)
            }));
            memory.lock().unwrap().set_current_page_table(table.as_mut() as *mut _, true);
            memory.lock().unwrap().write_32(0x3000, 0x12EF_CDAB);
            let mut system = Box::new(System::new());
            let mut process = KProcess::new();
            process.program_id = 42;
            process.page_table.set_memory(memory.clone());
            let process = Arc::new(ProcessLock::new(process));
            system.set_current_process_arc(process.clone());
            system.set_runtime_program_id(99);
            let mut shared = ProcessMemoryData::new();
            shared.base = 0x3000;
            shared.data = vec![0xAB, 0xCD, 0xEF, 0x12];
            system.set_shared_process_memory(Arc::new(RwLock::new(shared)));
            common::settings::values_mut().reporting_services.set_value(false);
            break_execution(&system, 0x8000_0001, 0x3000, 3);
            let reports = directory.join("svc_break_report");
            assert!(!reports.exists());
            common::settings::values_mut().reporting_services.set_value(true);
            for use_native_memory in [true, false] {
                if !use_native_memory {
                    process.lock().unwrap().page_table.get_base_mut().m_memory = None;
                }
                for (reason, address, size, expected_buffer) in [
                    (0x8000_0000, 0x3000, 3, Some("ABCDEF")),
                    (0x8000_0001, 0x3000, 4, Some("")),
                    (0x8000_0002, 0x3000, 3, Some("ABCDEF")),
                    (0x8000_00FF, 0x3000, 3, Some("ABCDEF")),
                    (0x8000_0001, 0, 3, None),
                    (0x8000_0001, 0x3000, 0, None),
                    (0x8000_0003, 0x3000, 3, None),
                    (7, 0x3000, 3, None), // CppException's dump occurs AFTER the report.
                ] {
                    break_execution(&system, reason, address, size);
                    let paths: Vec<_> = std::fs::read_dir(&reports).unwrap().map(|e| e.unwrap().path()).collect();
                    assert_eq!(paths.len(), 1);
                    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&paths[0]).unwrap()).unwrap();
                    assert_eq!(report["report_common"]["title_id"], "000000000000002A");
                    assert_eq!(report["svc_break"]["type"], format!("{reason:08X}"));
                    assert_eq!(report["svc_break"]["signal_debugger"], ((reason & 0x8000_0000) != 0).to_string());
                    assert_eq!(report["svc_break"]["debug_buffer"].as_str(), expected_buffer);
                    if expected_buffer.is_none() { assert!(report["svc_break"].get("debug_buffer").is_none()); }
                    std::fs::remove_file(&paths[0]).unwrap();
                }
            }
            common::settings::values_mut().reporting_services.set_value(false);
            drop(system);
            drop(process);
            drop(memory);
            drop(table);
            drop(device);
            std::fs::remove_dir_all(directory).unwrap();
        }).unwrap().join().unwrap();
    }
}
