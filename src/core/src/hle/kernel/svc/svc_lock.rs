//! Port of zuyu/src/core/hle/kernel/svc/svc_lock.cpp
//! Status: COMPLET (stubs for kernel calls)
//! Derniere synchro: 2026-03-11
//!
//! SVC handlers for mutex arbitration (ArbitrateLock, ArbitrateUnlock).

use crate::core::System;
use crate::hle::kernel::k_condition_variable::KConditionVariable;
use crate::hle::kernel::k_memory_layout::is_kernel_address;
use crate::hle::kernel::svc::svc_results::*;
use crate::hle::kernel::svc_common::Handle;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

fn should_trace_sync_debug() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_SYNC").is_some())
}

fn lock_trace_target() -> Option<u64> {
    use std::sync::OnceLock;
    static TARGET: OnceLock<Option<u64>> = OnceLock::new();
    *TARGET.get_or_init(|| {
        let raw = std::env::var("RUZU_TRACE_LOCK_ADDR").ok()?;
        let trimmed = raw.trim();
        let trimmed = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
            .unwrap_or(trimmed);
        u64::from_str_radix(trimmed, 16).ok()
    })
}

fn should_trace_lock_pi_addr(address: u64) -> bool {
    common::trace::is_enabled(common::trace::cat::LOCK_PI)
        && lock_trace_target().map_or(true, |target| target == address)
}

fn should_trace_sync_backtrace_once(tid: u64) -> bool {
    static DID_TRACE_TID73_LOCK: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    static DID_TRACE_TID73_UNLOCK: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    match tid {
        t if t == 73 => {
            !DID_TRACE_TID73_LOCK.swap(true, std::sync::atomic::Ordering::Relaxed)
                || !DID_TRACE_TID73_UNLOCK.swap(true, std::sync::atomic::Ordering::Relaxed)
        }
        _ => false,
    }
}

fn log_sync_context(system: &System, label: &str) {
    let Some(current_thread_id) = system.current_thread_id() else {
        return;
    };
    let Some(current_thread) = system.current_thread() else {
        return;
    };
    let core_index = current_thread.lock().unwrap().get_current_core().max(0) as usize;
    let process_arc = system.current_process_arc();
    let process = process_arc.lock().unwrap();
    let Some(cpu) = process.get_arm_interface(core_index) else {
        return;
    };
    let mut ctx = crate::arm::arm_interface::ThreadContext::default();
    cpu.get_context(&mut ctx);
    log::info!(
        "svc::{} ctx tid={} pc=0x{:08X} lr=0x{:08X} sp=0x{:08X}",
        label,
        current_thread_id,
        ctx.pc,
        ctx.lr,
        ctx.sp
    );
    if should_trace_sync_backtrace_once(current_thread_id) {
        let bt = crate::arm::debug::get_backtrace_from_context(&process, &ctx);
        for (index, entry) in bt.iter().take(12).enumerate() {
            log::info!(
                "svc::{} bt[{}]: tid={} module={} addr=0x{:X} orig=0x{:X} off=0x{:X} symbol={}",
                label,
                index,
                current_thread_id,
                entry.module,
                entry.address,
                entry.original_address,
                entry.offset,
                entry.name,
            );
        }
    }
}

/// Attempts to lock a mutex.
pub fn arbitrate_lock(
    system: &System,
    thread_handle: Handle,
    address: u64,
    tag: u32,
) -> ResultCode {
    log::trace!(
        "svc::ArbitrateLock called thread_handle=0x{:08X}, address=0x{:X}, tag=0x{:08X}",
        thread_handle,
        address,
        tag
    );
    if should_trace_sync_debug() {
        log::info!(
            "svc::ArbitrateLock tid={:?} thread_handle=0x{:08X} address=0x{:X} tag=0x{:08X}",
            system.current_thread_id(),
            thread_handle,
            address,
            tag
        );
        log_sync_context(system, "ArbitrateLock");
    }

    // Validate the input address.
    if is_kernel_address(address as usize) {
        return RESULT_INVALID_CURRENT_MEMORY;
    }
    if address % 4 != 0 {
        return RESULT_INVALID_ADDRESS;
    }

    let Some(current_thread) = system.current_thread() else {
        return RESULT_INVALID_HANDLE;
    };
    if should_trace_lock_pi_addr(address) {
        let (tid, tls_base, tpidr_el0) = {
            let thread = current_thread.lock().unwrap();
            (
                thread.get_thread_id(),
                thread.get_tls_address().get(),
                thread.get_tpidr_el0(),
            )
        };
        let (thread_type, tls_handle) = {
            let process_arc = system.current_process_arc();
            let process = process_arc.lock().unwrap();
            let is_64bit = process.is_64bit();
            process
                .get_memory()
                .map(|memory| {
                    let memory = memory.access().unwrap();
                    if is_64bit {
                        let thread_type =
                            if memory.is_valid_virtual_address_range(tls_base + 0x1f8, 8) {
                                memory.read_64(tls_base + 0x1f8)
                            } else {
                                0
                            };
                        let tls_handle = if thread_type != 0
                            && memory.is_valid_virtual_address_range(thread_type + 0x1b0, 4)
                        {
                            memory.read_32(thread_type + 0x1b0)
                        } else {
                            0
                        };
                        (thread_type, tls_handle)
                    } else {
                        let thread_type =
                            if memory.is_valid_virtual_address_range(tls_base + 0x1fc, 4) {
                                memory.read_32(tls_base + 0x1fc) as u64
                            } else {
                                0
                            };
                        let tls_handle = if thread_type != 0
                            && memory.is_valid_virtual_address_range(thread_type + 0x26, 2)
                        {
                            let version = memory.read_16(thread_type + 0x26);
                            let handle_addr = if version == 1 {
                                thread_type + 0xe4
                            } else {
                                thread_type + 0xe8
                            };
                            if memory.is_valid_virtual_address_range(handle_addr, 4) {
                                memory.read_32(handle_addr)
                            } else {
                                0
                            }
                        } else {
                            0
                        };
                        (thread_type, tls_handle)
                    }
                })
                .unwrap_or((0, 0))
        };
        let (owner_tid, tag_tid, current_handle, owner_object_id, tag_object_id) = {
            let process_arc = system.current_process_arc();
            let process = process_arc.lock().unwrap();
            let owner_object_id = process.handle_table.get_object(thread_handle);
            let owner_tid = owner_object_id
                .and_then(|object_id| process.get_thread_by_object_id(object_id))
                .map(|thread| thread.lock().unwrap().get_thread_id())
                .unwrap_or(0);
            let tag_object_id = process.handle_table.get_object(tag as Handle);
            let tag_tid = tag_object_id
                .and_then(|object_id| process.get_thread_by_object_id(object_id))
                .map(|thread| thread.lock().unwrap().get_thread_id())
                .unwrap_or(0);
            let current_handle = process
                .handle_table
                .entry_infos
                .iter()
                .enumerate()
                .find_map(|(index, info)| {
                    let linear_id = unsafe { info.linear_id };
                    if linear_id == 0 {
                        return None;
                    }
                    let candidate =
                        crate::hle::kernel::k_handle_table::encode_handle(index as u16, linear_id);
                    let object_id = process.handle_table.get_object(candidate)?;
                    let thread = process.get_thread_by_object_id(object_id)?;
                    (thread.lock().unwrap().get_thread_id() == tid).then_some(candidate)
                })
                .unwrap_or(0);
            (
                owner_tid,
                tag_tid,
                current_handle,
                owner_object_id.unwrap_or(0),
                tag_object_id.unwrap_or(0),
            )
        };
        common::trace::emit_raw(
            common::trace::cat::LOCK_PI,
            &[
                22,
                tid,
                address,
                thread_handle as u64,
                tag as u64,
                tls_handle as u64,
                owner_tid,
                tag_tid,
                current_handle as u64,
                tpidr_el0,
                RESULT_SUCCESS.get_inner_value() as u64,
                owner_object_id,
                tag_object_id,
                tls_base,
            ],
        );
        if tag == 0 {
            common::trace::emit_raw(
                common::trace::cat::LOCK_PI,
                &[
                    18,
                    tid,
                    address,
                    thread_handle as u64,
                    tag as u64,
                    tls_handle as u64,
                    thread_type,
                    tls_base,
                    0,
                    tpidr_el0,
                    RESULT_SUCCESS.get_inner_value() as u64,
                    0,
                    0,
                    0,
                ],
            );
        } else if tls_handle != 0 && tag != tls_handle {
            let (pc, lr, sp, x0, x1, x2, x20, jit_tpidrro) = {
                let core_index = current_thread.lock().unwrap().get_current_core().max(0) as usize;
                let process_arc = system.current_process_arc();
                let process = process_arc.lock().unwrap();
                if let Some(cpu) = process.get_arm_interface(core_index) {
                    let mut ctx = crate::arm::arm_interface::ThreadContext::default();
                    cpu.get_context(&mut ctx);
                    (
                        ctx.pc,
                        ctx.lr,
                        ctx.sp,
                        ctx.r[0],
                        ctx.r[1],
                        ctx.r[2],
                        ctx.r[20],
                        cpu.get_tpidrro_el0(),
                    )
                } else {
                    (0, 0, 0, 0, 0, 0, 0, 0)
                }
            };
            common::trace::emit_raw(
                common::trace::cat::LOCK_PI,
                &[
                    19,
                    tid,
                    address,
                    thread_handle as u64,
                    tag as u64,
                    tls_handle as u64,
                    thread_type,
                    tls_base,
                    0,
                    tpidr_el0,
                    RESULT_SUCCESS.get_inner_value() as u64,
                    0,
                    0,
                    0,
                ],
            );
            common::trace::emit_raw(
                common::trace::cat::LOCK_PI,
                &[
                    23,
                    tid,
                    address,
                    thread_handle as u64,
                    tag as u64,
                    tls_handle as u64,
                    pc,
                    lr,
                    sp,
                    x0,
                    x1,
                    x2,
                    x20,
                    jit_tpidrro,
                ],
            );
        }
    }

    let process = system.current_process_arc();
    let result = KConditionVariable::wait_for_address(
        &process,
        &current_thread,
        thread_handle,
        address,
        tag,
    );

    log::trace!(
        "svc::ArbitrateLock return thread_handle=0x{:08X}, address=0x{:X}, tag=0x{:08X}, result={:#x}",
        thread_handle,
        address,
        tag,
        result.get_inner_value()
    );

    result
}

/// Unlocks a mutex.
pub fn arbitrate_unlock(system: &System, address: u64) -> ResultCode {
    log::trace!("svc::ArbitrateUnlock called address=0x{:X}", address);
    if should_trace_sync_debug() {
        log::info!(
            "svc::ArbitrateUnlock tid={:?} address=0x{:X}",
            system.current_thread_id(),
            address
        );
        log_sync_context(system, "ArbitrateUnlock");
    }

    // Validate the input address.
    if is_kernel_address(address as usize) {
        return RESULT_INVALID_CURRENT_MEMORY;
    }
    if address % 4 != 0 {
        return RESULT_INVALID_ADDRESS;
    }

    let Some(current_thread) = system.current_thread() else {
        return RESULT_INVALID_HANDLE;
    };

    let process = system.current_process_arc();
    let result = KConditionVariable::signal_to_address(&process, &current_thread, address);

    log::trace!(
        "svc::ArbitrateUnlock return address=0x{:X}, result={:#x}",
        address,
        result.get_inner_value()
    );

    result
}
