// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/kernel/svc.h and svc.cpp
//! Status: COMPLET (full dispatch wiring)
//! Derniere synchro: 2026-03-13
//!
//! This file is auto-generated in upstream (svc_generator.py).
//! It provides:
//! - SvcId enumeration for all supervisor call numbers
//! - Argument marshalling helpers
//! - 32-bit and 64-bit dispatch tables
//! - The main `call` entry point

use crate::core::System;
use crate::hardware_properties;
use crate::hle::kernel::svc::svc_activity;
use crate::hle::kernel::svc::svc_address_arbiter;
use crate::hle::kernel::svc::svc_cache;
use crate::hle::kernel::svc::svc_code_memory;
use crate::hle::kernel::svc::svc_condition_variable;
use crate::hle::kernel::svc::svc_debug_string;
use crate::hle::kernel::svc::svc_event;
use crate::hle::kernel::svc::svc_exception;
use crate::hle::kernel::svc::svc_info;
use crate::hle::kernel::svc::svc_ipc;
use crate::hle::kernel::svc::svc_light_ipc;
use crate::hle::kernel::svc::svc_lock;
use crate::hle::kernel::svc::svc_memory;
use crate::hle::kernel::svc::svc_physical_memory;
use crate::hle::kernel::svc::svc_port;
use crate::hle::kernel::svc::svc_process;
use crate::hle::kernel::svc::svc_process_memory;
use crate::hle::kernel::svc::svc_processor;
use crate::hle::kernel::svc::svc_query_memory;
use crate::hle::kernel::svc::svc_shared_memory;
use crate::hle::kernel::svc::svc_synchronization;
use crate::hle::kernel::svc::svc_thread;
use crate::hle::kernel::svc::svc_tick;
use crate::hle::kernel::svc::svc_transfer_memory;
use crate::hle::kernel::svc::svc_types::MemoryPermission;

fn trace_cv_timeout_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_CV_TIMEOUT").is_some())
}

fn force_wait_mask_after_signal_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_FORCE_WAIT_MASK_AFTER_SIGNAL").is_some())
}

fn log_svc_sync_handle_target() -> Option<u32> {
    static TARGET: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *TARGET.get_or_init(|| {
        let value = std::env::var("RUZU_LOG_SVC_SYNC_HANDLE").ok()?;
        let trimmed = value.trim_start_matches("0x").trim_start_matches("0X");
        u32::from_str_radix(trimmed, 16)
            .ok()
            .or_else(|| value.parse().ok())
    })
}

fn decode_memory_permission(raw: u32) -> MemoryPermission {
    match raw {
        0 => MemoryPermission::None,
        1 => MemoryPermission::Read,
        2 => MemoryPermission::Write,
        3 => MemoryPermission::ReadWrite,
        4 => MemoryPermission::Execute,
        5 => MemoryPermission::ReadExecute,
        val if val == (1 << 28) => MemoryPermission::DontCare,
        _ => MemoryPermission::DontCare,
    }
}

fn drain_current_thread_termination(system: &System) {
    let current_thread = system.current_thread();
    let Some(current_thread) = current_thread else {
        return;
    };

    if {
        let thread = current_thread.lock().unwrap();
        thread.is_termination_requested() && !thread.is_signaled()
    } {
        current_thread.lock().unwrap().exit();
        system.scheduler_arc().lock().unwrap().request_schedule();
    }
}

fn log_unknown_svc_context(system: &System, imm: u32, is_64bit: bool) {
    use std::sync::atomic::Ordering;

    let n = UNKNOWN_SVC_CONTEXT_COUNT.fetch_add(1, Ordering::Relaxed);
    if n >= 32 {
        log::error!(
            "Unknown SVC 0x{:02X} in {}-bit mode (context suppressed after 32 reports)",
            imm,
            if is_64bit { 64 } else { 32 }
        );
        return;
    }

    let _ = system;
    log::error!(
        "Unknown SVC 0x{:02X} in {}-bit mode",
        imm,
        if is_64bit { 64 } else { 32 },
    );
}

/// SVC identifier — maps to the immediate value in the SVC instruction.
/// Corresponds to upstream `Kernel::Svc::SvcId`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvcId {
    SetHeapSize = 0x01,
    SetMemoryPermission = 0x02,
    SetMemoryAttribute = 0x03,
    MapMemory = 0x04,
    UnmapMemory = 0x05,
    QueryMemory = 0x06,
    ExitProcess = 0x07,
    CreateThread = 0x08,
    StartThread = 0x09,
    ExitThread = 0x0A,
    SleepThread = 0x0B,
    GetThreadPriority = 0x0C,
    SetThreadPriority = 0x0D,
    GetThreadCoreMask = 0x0E,
    SetThreadCoreMask = 0x0F,
    GetCurrentProcessorNumber = 0x10,
    SignalEvent = 0x11,
    ClearEvent = 0x12,
    MapSharedMemory = 0x13,
    UnmapSharedMemory = 0x14,
    CreateTransferMemory = 0x15,
    CloseHandle = 0x16,
    ResetSignal = 0x17,
    WaitSynchronization = 0x18,
    CancelSynchronization = 0x19,
    ArbitrateLock = 0x1A,
    ArbitrateUnlock = 0x1B,
    WaitProcessWideKeyAtomic = 0x1C,
    SignalProcessWideKey = 0x1D,
    GetSystemTick = 0x1E,
    ConnectToNamedPort = 0x1F,
    SendSyncRequestLight = 0x20,
    SendSyncRequest = 0x21,
    SendSyncRequestWithUserBuffer = 0x22,
    SendAsyncRequestWithUserBuffer = 0x23,
    GetProcessId = 0x24,
    GetThreadId = 0x25,
    Break = 0x26,
    OutputDebugString = 0x27,
    ReturnFromException = 0x28,
    GetInfo = 0x29,
    FlushEntireDataCache = 0x2A,
    FlushDataCache = 0x2B,
    MapPhysicalMemory = 0x2C,
    UnmapPhysicalMemory = 0x2D,
    GetDebugFutureThreadInfo = 0x2E,
    GetLastThreadInfo = 0x2F,
    GetResourceLimitLimitValue = 0x30,
    GetResourceLimitCurrentValue = 0x31,
    SetThreadActivity = 0x32,
    GetThreadContext3 = 0x33,
    WaitForAddress = 0x34,
    SignalToAddress = 0x35,
    SynchronizePreemptionState = 0x36,
    GetResourceLimitPeakValue = 0x37,
    CreateIoPool = 0x39,
    CreateIoRegion = 0x3A,
    KernelDebug = 0x3C,
    ChangeKernelTraceState = 0x3D,
    CreateSession = 0x40,
    AcceptSession = 0x41,
    ReplyAndReceiveLight = 0x42,
    ReplyAndReceive = 0x43,
    ReplyAndReceiveWithUserBuffer = 0x44,
    CreateEvent = 0x45,
    MapIoRegion = 0x46,
    UnmapIoRegion = 0x47,
    MapPhysicalMemoryUnsafe = 0x48,
    UnmapPhysicalMemoryUnsafe = 0x49,
    SetUnsafeLimit = 0x4A,
    CreateCodeMemory = 0x4B,
    ControlCodeMemory = 0x4C,
    SleepSystem = 0x4D,
    ReadWriteRegister = 0x4E,
    SetProcessActivity = 0x4F,
    CreateSharedMemory = 0x50,
    MapTransferMemory = 0x51,
    UnmapTransferMemory = 0x52,
    CreateInterruptEvent = 0x53,
    QueryPhysicalAddress = 0x54,
    QueryIoMapping = 0x55,
    CreateDeviceAddressSpace = 0x56,
    AttachDeviceAddressSpace = 0x57,
    DetachDeviceAddressSpace = 0x58,
    MapDeviceAddressSpaceByForce = 0x59,
    MapDeviceAddressSpaceAligned = 0x5A,
    UnmapDeviceAddressSpace = 0x5C,
    InvalidateProcessDataCache = 0x5D,
    StoreProcessDataCache = 0x5E,
    FlushProcessDataCache = 0x5F,
    DebugActiveProcess = 0x60,
    BreakDebugProcess = 0x61,
    TerminateDebugProcess = 0x62,
    GetDebugEvent = 0x63,
    ContinueDebugEvent = 0x64,
    GetProcessList = 0x65,
    GetThreadList = 0x66,
    GetDebugThreadContext = 0x67,
    SetDebugThreadContext = 0x68,
    QueryDebugProcessMemory = 0x69,
    ReadDebugProcessMemory = 0x6A,
    WriteDebugProcessMemory = 0x6B,
    SetHardwareBreakPoint = 0x6C,
    GetDebugThreadParam = 0x6D,
    GetSystemInfo = 0x6F,
    CreatePort = 0x70,
    ManageNamedPort = 0x71,
    ConnectToPort = 0x72,
    SetProcessMemoryPermission = 0x73,
    MapProcessMemory = 0x74,
    UnmapProcessMemory = 0x75,
    QueryProcessMemory = 0x76,
    MapProcessCodeMemory = 0x77,
    UnmapProcessCodeMemory = 0x78,
    CreateProcess = 0x79,
    StartProcess = 0x7A,
    TerminateProcess = 0x7B,
    GetProcessInfo = 0x7C,
    CreateResourceLimit = 0x7D,
    SetResourceLimitLimitValue = 0x7E,
    CallSecureMonitor = 0x7F,
    MapInsecureMemory = 0x90,
    UnmapInsecureMemory = 0x91,
}

impl SvcId {
    /// Try to convert a raw u32 immediate to an SvcId.
    pub fn from_u32(imm: u32) -> Option<Self> {
        match imm {
            0x01 => Some(Self::SetHeapSize),
            0x02 => Some(Self::SetMemoryPermission),
            0x03 => Some(Self::SetMemoryAttribute),
            0x04 => Some(Self::MapMemory),
            0x05 => Some(Self::UnmapMemory),
            0x06 => Some(Self::QueryMemory),
            0x07 => Some(Self::ExitProcess),
            0x08 => Some(Self::CreateThread),
            0x09 => Some(Self::StartThread),
            0x0A => Some(Self::ExitThread),
            0x0B => Some(Self::SleepThread),
            0x0C => Some(Self::GetThreadPriority),
            0x0D => Some(Self::SetThreadPriority),
            0x0E => Some(Self::GetThreadCoreMask),
            0x0F => Some(Self::SetThreadCoreMask),
            0x10 => Some(Self::GetCurrentProcessorNumber),
            0x11 => Some(Self::SignalEvent),
            0x12 => Some(Self::ClearEvent),
            0x13 => Some(Self::MapSharedMemory),
            0x14 => Some(Self::UnmapSharedMemory),
            0x15 => Some(Self::CreateTransferMemory),
            0x16 => Some(Self::CloseHandle),
            0x17 => Some(Self::ResetSignal),
            0x18 => Some(Self::WaitSynchronization),
            0x19 => Some(Self::CancelSynchronization),
            0x1A => Some(Self::ArbitrateLock),
            0x1B => Some(Self::ArbitrateUnlock),
            0x1C => Some(Self::WaitProcessWideKeyAtomic),
            0x1D => Some(Self::SignalProcessWideKey),
            0x1E => Some(Self::GetSystemTick),
            0x1F => Some(Self::ConnectToNamedPort),
            0x20 => Some(Self::SendSyncRequestLight),
            0x21 => Some(Self::SendSyncRequest),
            0x22 => Some(Self::SendSyncRequestWithUserBuffer),
            0x23 => Some(Self::SendAsyncRequestWithUserBuffer),
            0x24 => Some(Self::GetProcessId),
            0x25 => Some(Self::GetThreadId),
            0x26 => Some(Self::Break),
            0x27 => Some(Self::OutputDebugString),
            0x28 => Some(Self::ReturnFromException),
            0x29 => Some(Self::GetInfo),
            0x2A => Some(Self::FlushEntireDataCache),
            0x2B => Some(Self::FlushDataCache),
            0x2C => Some(Self::MapPhysicalMemory),
            0x2D => Some(Self::UnmapPhysicalMemory),
            0x2E => Some(Self::GetDebugFutureThreadInfo),
            0x2F => Some(Self::GetLastThreadInfo),
            0x30 => Some(Self::GetResourceLimitLimitValue),
            0x31 => Some(Self::GetResourceLimitCurrentValue),
            0x32 => Some(Self::SetThreadActivity),
            0x33 => Some(Self::GetThreadContext3),
            0x34 => Some(Self::WaitForAddress),
            0x35 => Some(Self::SignalToAddress),
            0x36 => Some(Self::SynchronizePreemptionState),
            0x37 => Some(Self::GetResourceLimitPeakValue),
            0x39 => Some(Self::CreateIoPool),
            0x3A => Some(Self::CreateIoRegion),
            0x3C => Some(Self::KernelDebug),
            0x3D => Some(Self::ChangeKernelTraceState),
            0x40 => Some(Self::CreateSession),
            0x41 => Some(Self::AcceptSession),
            0x42 => Some(Self::ReplyAndReceiveLight),
            0x43 => Some(Self::ReplyAndReceive),
            0x44 => Some(Self::ReplyAndReceiveWithUserBuffer),
            0x45 => Some(Self::CreateEvent),
            0x46 => Some(Self::MapIoRegion),
            0x47 => Some(Self::UnmapIoRegion),
            0x48 => Some(Self::MapPhysicalMemoryUnsafe),
            0x49 => Some(Self::UnmapPhysicalMemoryUnsafe),
            0x4A => Some(Self::SetUnsafeLimit),
            0x4B => Some(Self::CreateCodeMemory),
            0x4C => Some(Self::ControlCodeMemory),
            0x4D => Some(Self::SleepSystem),
            0x4E => Some(Self::ReadWriteRegister),
            0x4F => Some(Self::SetProcessActivity),
            0x50 => Some(Self::CreateSharedMemory),
            0x51 => Some(Self::MapTransferMemory),
            0x52 => Some(Self::UnmapTransferMemory),
            0x53 => Some(Self::CreateInterruptEvent),
            0x54 => Some(Self::QueryPhysicalAddress),
            0x55 => Some(Self::QueryIoMapping),
            0x56 => Some(Self::CreateDeviceAddressSpace),
            0x57 => Some(Self::AttachDeviceAddressSpace),
            0x58 => Some(Self::DetachDeviceAddressSpace),
            0x59 => Some(Self::MapDeviceAddressSpaceByForce),
            0x5A => Some(Self::MapDeviceAddressSpaceAligned),
            0x5C => Some(Self::UnmapDeviceAddressSpace),
            0x5D => Some(Self::InvalidateProcessDataCache),
            0x5E => Some(Self::StoreProcessDataCache),
            0x5F => Some(Self::FlushProcessDataCache),
            0x60 => Some(Self::DebugActiveProcess),
            0x61 => Some(Self::BreakDebugProcess),
            0x62 => Some(Self::TerminateDebugProcess),
            0x63 => Some(Self::GetDebugEvent),
            0x64 => Some(Self::ContinueDebugEvent),
            0x65 => Some(Self::GetProcessList),
            0x66 => Some(Self::GetThreadList),
            0x67 => Some(Self::GetDebugThreadContext),
            0x68 => Some(Self::SetDebugThreadContext),
            0x69 => Some(Self::QueryDebugProcessMemory),
            0x6A => Some(Self::ReadDebugProcessMemory),
            0x6B => Some(Self::WriteDebugProcessMemory),
            0x6C => Some(Self::SetHardwareBreakPoint),
            0x6D => Some(Self::GetDebugThreadParam),
            0x6F => Some(Self::GetSystemInfo),
            0x70 => Some(Self::CreatePort),
            0x71 => Some(Self::ManageNamedPort),
            0x72 => Some(Self::ConnectToPort),
            0x73 => Some(Self::SetProcessMemoryPermission),
            0x74 => Some(Self::MapProcessMemory),
            0x75 => Some(Self::UnmapProcessMemory),
            0x76 => Some(Self::QueryProcessMemory),
            0x77 => Some(Self::MapProcessCodeMemory),
            0x78 => Some(Self::UnmapProcessCodeMemory),
            0x79 => Some(Self::CreateProcess),
            0x7A => Some(Self::StartProcess),
            0x7B => Some(Self::TerminateProcess),
            0x7C => Some(Self::GetProcessInfo),
            0x7D => Some(Self::CreateResourceLimit),
            0x7E => Some(Self::SetResourceLimitLimitValue),
            0x7F => Some(Self::CallSecureMonitor),
            0x90 => Some(Self::MapInsecureMemory),
            0x91 => Some(Self::UnmapInsecureMemory),
            _ => None,
        }
    }
}

// =============================================================================
// Argument marshalling helpers
// =============================================================================

/// SVC argument register file: 8 x u64 registers (r0-r7 / x0-x7).
pub type SvcArgs = [u64; 8];

/// Extract a u32 argument from the register file.
#[inline]
fn get_arg32(args: &SvcArgs, n: usize) -> u32 {
    args[n] as u32
}

/// Store a u32 result into the register file.
#[inline]
fn set_arg32(args: &mut SvcArgs, n: usize, value: u32) {
    args[n] = value as u64;
}

/// Extract a u64 argument from the register file.
#[inline]
fn get_arg64(args: &SvcArgs, n: usize) -> u64 {
    args[n]
}

/// Store a u64 result into the register file.
#[inline]
fn set_arg64(args: &mut SvcArgs, n: usize, value: u64) {
    args[n] = value;
}

/// Gather a u64 from two u32 registers (ARM32 ABI).
/// lo register holds the low 32 bits, hi register holds the high 32 bits.
#[inline]
fn gather64(args: &SvcArgs, lo: usize, hi: usize) -> u64 {
    (args[lo] as u32 as u64) | ((args[hi] as u32 as u64) << 32)
}

/// Scatter a u64 into two u32 registers (ARM32 ABI).
#[inline]
fn scatter64(args: &mut SvcArgs, lo: usize, hi: usize, val: u64) {
    args[lo] = val as u32 as u64;
    args[hi] = (val >> 32) as u32 as u64;
}

// =============================================================================
// Stub result for unimplemented SVCs that should return success
// =============================================================================

/// Return success (0) for stubbed SVCs. Matches upstream behavior where the
/// handler would succeed but we haven't implemented the kernel object access.
const STUB_SUCCESS: u32 = 0;

/// Allocate a stub handle registered in the process handle table.
///
/// Upstream SVCs like CreateTransferMemory create a real kernel object
/// (e.g. KTransferMemory) and add it to the handle table via
/// `handle_table.Add(out, trmem)`. When the game later calls CloseHandle,
/// it must find the handle in the table or it gets ResultInvalidHandle.
///
/// For stubbed SVCs where we don't create the real kernel object yet,
/// we still must register a handle in the process table so that
/// CloseHandle succeeds. We use the kernel's object ID allocator to
/// get a unique ID as the opaque "object".
fn alloc_stub_handle(system: &System) -> u32 {
    let object_id = system
        .kernel()
        .map(|k| k.create_new_object_id())
        .unwrap_or(1) as u64;
    system
        .current_process_arc()
        .lock()
        .unwrap()
        .handle_table
        .add(object_id)
        .unwrap_or(0)
}

// =============================================================================
// 32-bit dispatch (AArch32 / ILP32)
// =============================================================================

/// Dispatch a 32-bit (AArch32 / ILP32) SVC.
///
/// Corresponds to upstream `Call32`. Register layouts match the auto-generated
/// SvcWrap_*64From32 wrappers in svc.cpp.
fn call32(system: &System, imm: u32, args: &mut SvcArgs) {
    match SvcId::from_u32(imm) {
        // =====================================================================
        // Memory management
        // =====================================================================
        Some(SvcId::SetHeapSize) => {
            // IN: size=arg32[1]; OUT: ret=arg32[0], out_address=arg32[1]
            let size = get_arg32(args, 1) as u64;
            let mut heap_base = 0;
            let result =
                svc_physical_memory::set_heap_size_current_process(system, &mut heap_base, size);
            log::info!("  SetHeapSize({:#x}) -> heap at {:#x}", size, heap_base);
            set_arg32(args, 0, result.get_inner_value());
            set_arg32(args, 1, heap_base as u32);
        }
        Some(SvcId::SetMemoryPermission) => {
            // IN: address=arg32[0], size=arg32[1], perm=arg32[2]; OUT: ret=arg32[0]
            let address = get_arg32(args, 0) as u64;
            let size = get_arg32(args, 1) as u64;
            let perm = get_arg32(args, 2);
            let result = svc_memory::set_memory_permission(
                system,
                address,
                size,
                decode_memory_permission(perm),
            );
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::SetMemoryAttribute) => {
            // IN: address=arg32[0], size=arg32[1], mask=arg32[2], attr=arg32[3]; OUT: ret=arg32[0]
            let address = get_arg32(args, 0) as u64;
            let size = get_arg32(args, 1) as u64;
            let mask = get_arg32(args, 2);
            let attr = get_arg32(args, 3);
            let result = svc_memory::set_memory_attribute(system, address, size, mask, attr);
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::MapMemory) => {
            // IN: dst=arg32[0], src=arg32[1], size=arg32[2]; OUT: ret=arg32[0]
            let dst = get_arg32(args, 0) as u64;
            let src = get_arg32(args, 1) as u64;
            let size = get_arg32(args, 2) as u64;
            let result = svc_memory::map_memory(system, dst, src, size);
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::UnmapMemory) => {
            // IN: dst=arg32[0], src=arg32[1], size=arg32[2]; OUT: ret=arg32[0]
            let dst = get_arg32(args, 0) as u64;
            let src = get_arg32(args, 1) as u64;
            let size = get_arg32(args, 2) as u64;
            let result = svc_memory::unmap_memory(system, dst, src, size);
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::QueryMemory) => {
            // IN: out_memory_info=arg32[0], address=arg32[2]; OUT: ret=arg32[0], page_info=arg32[1]
            let mem_info_ptr = get_arg32(args, 0) as u64;
            let query_addr = get_arg32(args, 2) as u64;
            let mut page_info = crate::hle::kernel::svc::svc_types::PageInfo::default();
            let result =
                svc_query_memory::query_memory(system, mem_info_ptr, &mut page_info, query_addr);
            set_arg32(args, 0, result.get_inner_value());
            set_arg32(args, 1, page_info.flags);
        }
        Some(SvcId::MapPhysicalMemory) => {
            // IN: address=arg32[0], size=arg32[1]; OUT: ret=arg32[0]
            let address = get_arg32(args, 0) as u64;
            let size = get_arg32(args, 1) as u64;
            let result = svc_physical_memory::map_physical_memory(system, address, size);
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::UnmapPhysicalMemory) => {
            // IN: address=arg32[0], size=arg32[1]; OUT: ret=arg32[0]
            let address = get_arg32(args, 0) as u64;
            let size = get_arg32(args, 1) as u64;
            let result = svc_physical_memory::unmap_physical_memory(system, address, size);
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::MapPhysicalMemoryUnsafe) => {
            let address = get_arg32(args, 0) as u64;
            let size = get_arg32(args, 1) as u64;
            let result = svc_physical_memory::map_physical_memory_unsafe(address, size);
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::UnmapPhysicalMemoryUnsafe) => {
            let address = get_arg32(args, 0) as u64;
            let size = get_arg32(args, 1) as u64;
            let result = svc_physical_memory::unmap_physical_memory_unsafe(address, size);
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::SetUnsafeLimit) => {
            let limit = get_arg32(args, 0) as u64;
            let result = svc_physical_memory::set_unsafe_limit(limit);
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::MapInsecureMemory) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::UnmapInsecureMemory) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }

        // =====================================================================
        // Process management
        // =====================================================================
        Some(SvcId::ExitProcess) => {
            svc_process::exit_process(system);
        }
        Some(SvcId::GetProcessId) => {
            // IN: process_handle=arg32[1]; OUT: ret=arg32[0], pid=scatter64[1,2]
            set_arg32(args, 0, STUB_SUCCESS);
            scatter64(args, 1, 2, 1); // pid = 1
        }
        Some(SvcId::GetProcessList) => {
            // IN: out_ids=arg32[1], max=arg32[2]; OUT: ret=arg32[0], count=arg32[1]
            set_arg32(args, 0, STUB_SUCCESS);
            set_arg32(args, 1, 0);
        }
        Some(SvcId::GetProcessInfo) => {
            // IN: handle=arg32[1], type=arg32[2]; OUT: ret=arg32[0], out=scatter64[1,2]
            set_arg32(args, 0, STUB_SUCCESS);
            scatter64(args, 1, 2, 0);
        }
        Some(SvcId::CreateProcess) => {
            // IN: params=arg32[1], caps=arg32[2], num_caps=arg32[3]; OUT: ret=arg32[0], handle=arg32[1]
            log::warn!("  CreateProcess: stub");
            set_arg32(args, 0, STUB_SUCCESS);
            set_arg32(args, 1, alloc_stub_handle(system));
        }
        Some(SvcId::StartProcess) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::TerminateProcess) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::SetProcessActivity) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }

        // =====================================================================
        // Thread management
        // =====================================================================
        Some(SvcId::CreateThread) => {
            // IN: func=arg32[1], arg=arg32[2], stack_bottom=arg32[3], priority=arg32[0], core_id=arg32[4]
            // OUT: ret=arg32[0], out_handle=arg32[1]
            let priority = get_arg32(args, 0) as i32;
            let entry_point = get_arg32(args, 1) as u64;
            let arg = get_arg32(args, 2) as u64;
            let stack_bottom = get_arg32(args, 3) as u64;
            let core_id = get_arg32(args, 4) as i32;
            let mut out_handle = 0;
            let result = svc_thread::create_thread(
                system,
                &mut out_handle,
                entry_point,
                arg,
                stack_bottom,
                priority,
                core_id,
            );
            set_arg32(args, 0, result.get_inner_value());
            set_arg32(args, 1, out_handle);
        }
        Some(SvcId::StartThread) => {
            // IN: handle=arg32[0]; OUT: ret=arg32[0]
            let handle = get_arg32(args, 0);
            let result = svc_thread::start_thread(system, handle);
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::ExitThread) => {
            svc_thread::exit_thread(system);
        }
        Some(SvcId::SleepThread) => {
            // IN: ns=gather64[0,1]; OUT: (none)
            let ns = gather64(args, 0, 1) as i64;
            svc_thread::sleep_thread(system, ns);
        }
        Some(SvcId::GetThreadPriority) => {
            // IN: handle=arg32[1]; OUT: ret=arg32[0], priority=arg32[1]
            let handle = get_arg32(args, 1);
            let mut out_priority = 0;
            let result = svc_thread::get_thread_priority(system, &mut out_priority, handle);
            log::info!(
                "  GetThreadPriority(handle={:#x}) -> result={:#x}, priority={}",
                handle,
                result.get_inner_value(),
                out_priority
            );
            set_arg32(args, 0, result.get_inner_value());
            set_arg32(args, 1, out_priority as u32);
        }
        Some(SvcId::SetThreadPriority) => {
            // IN: handle=arg32[0], priority=arg32[1]; OUT: ret=arg32[0]
            let handle = get_arg32(args, 0);
            let priority = get_arg32(args, 1) as i32;
            let result = svc_thread::set_thread_priority(system, handle, priority);
            log::info!(
                "  SetThreadPriority(handle={:#x}, priority={}) -> result={:#x}",
                handle,
                priority,
                result.get_inner_value()
            );
            if result.is_error() {
                let process_arc = system.current_process_arc();
                let process = process_arc.lock().unwrap();
                let prio_mask = process.get_priority_mask();
                let prio_check = process.check_thread_priority(priority);
                log::error!(
                    "  priority_mask={:#018x}, check_thread_priority({})={}",
                    prio_mask,
                    priority,
                    prio_check
                );
                log::error!(
                    "  Handle table dump: count={}, table_size={}",
                    process.handle_table.count,
                    process.handle_table.table_size
                );
                for i in 0..std::cmp::min(process.handle_table.count as usize + 2, 10) {
                    let obj = process.handle_table.objects[i];
                    let li = unsafe { process.handle_table.entry_infos[i].linear_id };
                    if obj != 0 {
                        let h = crate::hle::kernel::k_handle_table::encode_handle(i as u16, li);
                        log::error!("    [{}] object_id={} handle={:#x}", i, obj, h);
                    }
                }
                // Check the specific index
                let (idx, lid, _) = crate::hle::kernel::k_handle_table::decode_handle(handle);
                log::error!("  Decoded handle: index={}, linear_id={}", idx, lid);
                log::error!(
                    "  At index {}: object_id={}, entry linear_id={}",
                    idx,
                    process.handle_table.objects[idx as usize],
                    unsafe { process.handle_table.entry_infos[idx as usize].linear_id }
                );
            }
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::GetThreadCoreMask) => {
            // IN: handle=arg32[2]; OUT: ret=arg32[0], core_id=arg32[1]
            let handle = get_arg32(args, 2);
            let mut out_core_id = 0;
            let mut out_affinity_mask = 0;
            let result = svc_thread::get_thread_core_mask(
                system,
                &mut out_core_id,
                &mut out_affinity_mask,
                handle,
            );
            set_arg32(args, 0, result.get_inner_value());
            set_arg32(args, 1, out_core_id as u32);
            scatter64(args, 2, 3, out_affinity_mask);
        }
        Some(SvcId::SetThreadCoreMask) => {
            // IN: handle=arg32[0], core_id=arg32[1], affinity=gather64[2,3]; OUT: ret=arg32[0]
            let handle = get_arg32(args, 0);
            let core_id = get_arg32(args, 1) as i32;
            let affinity_mask = gather64(args, 2, 3);
            let result = svc_thread::set_thread_core_mask(system, handle, core_id, affinity_mask);
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::GetCurrentProcessorNumber) => {
            // IN: (none); OUT: ret=arg32[0]
            set_arg32(
                args,
                0,
                svc_processor::get_current_processor_number(system) as u32,
            );
        }
        Some(SvcId::GetThreadId) => {
            // IN: handle=arg32[1]; OUT: ret=arg32[0], tid=scatter64[1,2]
            let handle = get_arg32(args, 1);
            let mut out_thread_id = 0;
            let result = svc_thread::get_thread_id(system, &mut out_thread_id, handle);
            set_arg32(args, 0, result.get_inner_value());
            scatter64(args, 1, 2, out_thread_id);
        }
        Some(SvcId::SetThreadActivity) => {
            let handle = get_arg32(args, 0);
            let result = match get_arg32(args, 1) {
                0 => svc_activity::set_thread_activity(
                    system,
                    handle,
                    crate::hle::kernel::svc::svc_types::ThreadActivity::Runnable,
                ),
                1 => svc_activity::set_thread_activity(
                    system,
                    handle,
                    crate::hle::kernel::svc::svc_types::ThreadActivity::Paused,
                ),
                _ => crate::hle::kernel::svc::svc_results::RESULT_INVALID_ENUM_VALUE,
            };
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::GetThreadContext3) => {
            let result = svc_thread::get_thread_context3(
                system,
                get_arg32(args, 0) as u64,
                get_arg32(args, 1),
            );
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::GetThreadList) => {
            // IN: out=arg32[1], max=arg32[2], debug=arg32[3]; OUT: ret=arg32[0], count=arg32[1]
            set_arg32(args, 0, STUB_SUCCESS);
            set_arg32(args, 1, 0);
        }

        // =====================================================================
        // Synchronization
        // =====================================================================
        Some(SvcId::SignalEvent) => {
            let result = svc_event::signal_event(system, get_arg32(args, 0));
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::ClearEvent) => {
            let result = svc_event::clear_event(system, get_arg32(args, 0));
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::CloseHandle) => {
            // IN: handle=arg32[0]; OUT: ret=arg32[0]
            let result = svc_synchronization::close_handle(system, get_arg32(args, 0));
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::ResetSignal) => {
            let result = svc_synchronization::reset_signal(system, get_arg32(args, 0));
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::WaitSynchronization) => {
            // IN: handles=arg32[1], num=arg32[2], timeout=gather64[0,3]; OUT: ret=arg32[0], index=arg32[1]
            let mut out_index = -1;
            let result = svc_synchronization::wait_synchronization(
                system,
                &mut out_index,
                get_arg32(args, 1) as u64,
                get_arg32(args, 2) as i32,
                gather64(args, 0, 3) as i64,
            );
            set_arg32(args, 0, result.get_inner_value());
            set_arg32(args, 1, out_index as u32);
        }
        Some(SvcId::CancelSynchronization) => {
            let result = svc_synchronization::cancel_synchronization(system, get_arg32(args, 0));
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::ArbitrateLock) => {
            let result = svc_lock::arbitrate_lock(
                system,
                get_arg32(args, 0),
                get_arg32(args, 1) as u64,
                get_arg32(args, 2),
            );
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::ArbitrateUnlock) => {
            let result = svc_lock::arbitrate_unlock(system, get_arg32(args, 0) as u64);
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::WaitProcessWideKeyAtomic) => {
            let mutex_addr = get_arg32(args, 0) as u64;
            let cv_key = get_arg32(args, 1) as u64;
            let tag = get_arg32(args, 2);
            let timeout = gather64(args, 3, 4) as i64;
            // RUZU_TRACE_CV_TIMEOUT=1 — log the full 64-bit timeout so we can
            // tell infinite (-1) from a 4.3s u32-truncated value (0xFFFFFFFF).
            if trace_cv_timeout_enabled() {
                log::info!(
                    "[CV_TIMEOUT] tid={} mutex=0x{:X} cv=0x{:X} tag=0x{:X} args[3]=0x{:X} args[4]=0x{:X} timeout_i64={} (={}ms)",
                    system
                        .current_thread()
                        .and_then(|t| t.lock().ok().map(|g| g.get_thread_id()))
                        .unwrap_or(0),
                    mutex_addr,
                    cv_key,
                    tag,
                    get_arg32(args, 3),
                    get_arg32(args, 4),
                    timeout,
                    timeout / 1_000_000,
                );
            }
            let result = svc_condition_variable::wait_process_wide_key_atomic(
                system, mutex_addr, cv_key, tag, timeout,
            );
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::SignalProcessWideKey) => {
            let cv_key = get_arg32(args, 0) as u64;
            svc_condition_variable::signal_process_wide_key(
                system,
                cv_key,
                get_arg32(args, 1) as i32,
            );
            // After signaling cv at `cv_key`, set WAIT_MASK (0x40000000) on the
            // paired mutex word at `cv_key - 8` (libnx AuxBufferInfo layout:
            // mutex@+0, padding@+4, cv@+8). This forces the next libnx
            // __nx_arbiter_unlock fast-path CAS to FAIL (cur != self_handle
            // because of WAIT_MASK), routing the unlock through the kernel
            // ArbitrateUnlock SVC where any actually-waiting thread (registered
            // after we set WAIT_MASK) gets woken correctly.
            //
            // Cost: every cv-signal forces a subsequent ArbitrateUnlock SVC
            // even with no waiter (kernel finds nothing to wake, returns).
            //
            // Limitation: only fires for cv_key-8 layout; some games use
            // separate mutex+cv addresses where -8 won't be the mutex.
            if force_wait_mask_after_signal_enabled() && cv_key >= 8 {
                let mutex_addr = cv_key - 8;
                let process_arc = system.current_process_arc().clone();
                {
                    let process = process_arc.lock().unwrap();
                    if let Some(memory) = process.page_table.get_base().m_memory.as_ref() {
                        let mem = memory.lock().unwrap();
                        let current = mem.read_32(mutex_addr);
                        // Only OR if there's a holder (low 28 bits nonzero) and
                        // WAIT_MASK isn't already set.
                        if current != 0 && (current & 0x40000000) == 0 {
                            mem.write_32_no_rasterizer(mutex_addr, current | 0x40000000);
                        }
                    }
                }
            }
        }
        Some(SvcId::WaitForAddress) => {
            let result = svc_address_arbiter::wait_for_address(
                system,
                get_arg32(args, 0) as u64,
                get_arg32(args, 1),
                get_arg32(args, 2) as i32,
                gather64(args, 3, 4) as i64,
            );
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::SignalToAddress) => {
            let result = svc_address_arbiter::signal_to_address(
                system,
                get_arg32(args, 0) as u64,
                get_arg32(args, 1),
                get_arg32(args, 2) as i32,
                get_arg32(args, 3) as i32,
            );
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::SynchronizePreemptionState) => {
            svc_synchronization::synchronize_preemption_state(system);
        }
        Some(SvcId::CreateEvent) => {
            // OUT: ret=arg32[0], write_handle=arg32[1], read_handle=arg32[2]
            let mut write_handle = 0;
            let mut read_handle = 0;
            let result = svc_event::create_event(system, &mut write_handle, &mut read_handle);
            set_arg32(args, 0, result.get_inner_value());
            set_arg32(args, 1, write_handle);
            set_arg32(args, 2, read_handle);
        }

        // =====================================================================
        // Timing
        // =====================================================================
        Some(SvcId::GetSystemTick) => {
            // IN: (none); OUT: tick=scatter64[0,1]
            let tick = svc_tick::get_system_tick(system);
            scatter64(args, 0, 1, tick as u64);
        }

        // =====================================================================
        // IPC
        // =====================================================================
        Some(SvcId::ConnectToNamedPort) => {
            let mut out = 0;
            let result =
                svc_port::connect_to_named_port(system, &mut out, get_arg32(args, 1) as u64);
            set_arg32(args, 0, result.get_inner_value());
            set_arg32(args, 1, out);
        }
        Some(SvcId::SendSyncRequestLight) => {
            svc_light_ipc::svc_wrap_send_sync_request_light(system, args);
        }
        Some(SvcId::SendSyncRequest) => {
            let session_handle = get_arg32(args, 0);
            if log_svc_sync_handle_target().is_some_and(|target| target == session_handle) {
                log::info!(
                    "svc::SendSyncRequest pre handle={:#x} r0={:#010x} r1={:#010x} r2={:#010x} r3={:#010x} r4={:#010x} r5={:#010x} r6={:#010x} r7={:#010x}",
                    session_handle,
                    get_arg32(args, 0),
                    get_arg32(args, 1),
                    get_arg32(args, 2),
                    get_arg32(args, 3),
                    get_arg32(args, 4),
                    get_arg32(args, 5),
                    get_arg32(args, 6),
                    get_arg32(args, 7),
                );
            }
            let result = svc_ipc::send_sync_request(system, session_handle);
            set_arg32(args, 0, result.get_inner_value());
            if log_svc_sync_handle_target().is_some_and(|target| target == session_handle) {
                log::info!(
                    "svc::SendSyncRequest post handle={:#x} r0={:#010x} r1={:#010x} r2={:#010x} r3={:#010x} r4={:#010x} r5={:#010x} r6={:#010x} r7={:#010x}",
                    session_handle,
                    get_arg32(args, 0),
                    get_arg32(args, 1),
                    get_arg32(args, 2),
                    get_arg32(args, 3),
                    get_arg32(args, 4),
                    get_arg32(args, 5),
                    get_arg32(args, 6),
                    get_arg32(args, 7),
                );
            }
        }
        Some(SvcId::SendSyncRequestWithUserBuffer) => {
            let message_buffer = get_arg32(args, 0) as u64;
            let message_buffer_size = get_arg32(args, 1) as u64;
            let session_handle = get_arg32(args, 2);
            let result = svc_ipc::send_sync_request_with_user_buffer(
                system,
                message_buffer,
                message_buffer_size,
                session_handle,
            );
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::SendAsyncRequestWithUserBuffer) => {
            let message_buffer = get_arg32(args, 0) as u64;
            let message_buffer_size = get_arg32(args, 1) as u64;
            let session_handle = get_arg32(args, 2);
            let mut out_event_handle = 0;
            let result = svc_ipc::send_async_request_with_user_buffer(
                system,
                &mut out_event_handle,
                message_buffer,
                message_buffer_size,
                session_handle,
            );
            set_arg32(args, 0, result.get_inner_value());
            set_arg32(args, 1, out_event_handle);
        }
        Some(SvcId::ReplyAndReceiveLight) => {
            svc_light_ipc::svc_wrap_reply_and_receive_light(system, args);
        }
        Some(SvcId::ReplyAndReceive) => {
            let handles = get_arg32(args, 0) as u64;
            let num_handles = get_arg32(args, 1) as i32;
            let reply_target = get_arg32(args, 2);
            let timeout_ns = ((get_arg32(args, 4) as u64) << 32 | get_arg32(args, 3) as u64) as i64;
            let mut out_index = 0;
            let result = svc_ipc::reply_and_receive(
                system,
                &mut out_index,
                handles,
                num_handles,
                reply_target,
                timeout_ns,
            );
            set_arg32(args, 0, result.get_inner_value());
            set_arg32(args, 1, out_index as u32);
        }
        Some(SvcId::ReplyAndReceiveWithUserBuffer) => {
            let message_buffer = get_arg32(args, 0) as u64;
            let message_buffer_size = get_arg32(args, 1) as u64;
            let handles = get_arg32(args, 2) as u64;
            let num_handles = get_arg32(args, 3) as i32;
            let reply_target = get_arg32(args, 4);
            let timeout_ns = ((get_arg32(args, 6) as u64) << 32 | get_arg32(args, 5) as u64) as i64;
            let mut out_index = 0;
            let result = svc_ipc::reply_and_receive_with_user_buffer(
                system,
                &mut out_index,
                message_buffer,
                message_buffer_size,
                handles,
                num_handles,
                reply_target,
                timeout_ns,
            );
            set_arg32(args, 0, result.get_inner_value());
            set_arg32(args, 1, out_index as u32);
        }
        Some(SvcId::CreateSession) => {
            // OUT: ret=arg32[0], server=arg32[1], client=arg32[2]
            set_arg32(args, 0, STUB_SUCCESS);
            set_arg32(args, 1, alloc_stub_handle(system));
            set_arg32(args, 2, alloc_stub_handle(system));
        }
        Some(SvcId::AcceptSession) => {
            // OUT: ret=arg32[0], handle=arg32[1]
            set_arg32(args, 0, STUB_SUCCESS);
            set_arg32(args, 1, alloc_stub_handle(system));
        }
        Some(SvcId::CreatePort) => {
            // OUT: ret=arg32[0], server=arg32[1], client=arg32[2]
            set_arg32(args, 0, STUB_SUCCESS);
            set_arg32(args, 1, alloc_stub_handle(system));
            set_arg32(args, 2, alloc_stub_handle(system));
        }
        Some(SvcId::ManageNamedPort) => {
            // OUT: ret=arg32[0], server=arg32[1]
            set_arg32(args, 0, STUB_SUCCESS);
            set_arg32(args, 1, alloc_stub_handle(system));
        }
        Some(SvcId::ConnectToPort) => {
            // OUT: ret=arg32[0], handle=arg32[1]
            set_arg32(args, 0, STUB_SUCCESS);
            set_arg32(args, 1, alloc_stub_handle(system));
        }

        // =====================================================================
        // Shared / Transfer memory
        // =====================================================================
        Some(SvcId::MapSharedMemory) => {
            // IN: handle=arg32[0], address=arg32[1], size=arg32[2], perm=arg32[3]; OUT: ret=arg32[0]
            let handle = get_arg32(args, 0);
            let address = get_arg32(args, 1) as u64;
            let size = get_arg32(args, 2) as u64;
            let perm = get_arg32(args, 3);
            let map_perm = unsafe {
                std::mem::transmute::<u32, crate::hle::kernel::svc::svc_types::MemoryPermission>(
                    perm,
                )
            };
            let result =
                svc_shared_memory::map_shared_memory(system, handle, address, size, map_perm);
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::UnmapSharedMemory) => {
            // IN: handle=arg32[0], address=arg32[1], size=arg32[2]; OUT: ret=arg32[0]
            let handle = get_arg32(args, 0);
            let address = get_arg32(args, 1) as u64;
            let size = get_arg32(args, 2) as u64;
            let result = svc_shared_memory::unmap_shared_memory(system, handle, address, size);
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::CreateTransferMemory) => {
            // IN: address=arg32[1], size=arg32[2], perm=arg32[3]; OUT: ret=arg32[0], handle=arg32[1]
            let address = get_arg32(args, 1) as u64;
            let size = get_arg32(args, 2) as u64;
            let map_perm = unsafe { std::mem::transmute(get_arg32(args, 3)) };
            let mut handle = 0;
            let result = svc_transfer_memory::create_transfer_memory(
                system,
                &mut handle,
                address,
                size,
                map_perm,
            );
            set_arg32(args, 0, result.get_inner_value());
            set_arg32(args, 1, handle);
        }
        Some(SvcId::CreateSharedMemory) => {
            // IN: size=arg32[1], owner_perm=arg32[2], remote_perm=arg32[3]
            // OUT: ret=arg32[0], handle=arg32[1]
            let mut handle = 0;
            let size = get_arg32(args, 1) as u64;
            let owner_perm = unsafe { std::mem::transmute(get_arg32(args, 2)) };
            let remote_perm = unsafe { std::mem::transmute(get_arg32(args, 3)) };
            let result =
                svc_shared_memory::create_shared_memory(&mut handle, size, owner_perm, remote_perm);
            set_arg32(args, 0, result.get_inner_value());
            set_arg32(args, 1, handle);
        }
        Some(SvcId::MapTransferMemory) => {
            // IN: handle=arg32[1], address=arg32[2], size=arg32[3], perm=arg32[4]; OUT: ret=arg32[0]
            let handle = get_arg32(args, 1);
            let address = get_arg32(args, 2) as u64;
            let size = get_arg32(args, 3) as u64;
            let map_perm = unsafe { std::mem::transmute(get_arg32(args, 4)) };
            let result =
                svc_transfer_memory::map_transfer_memory(system, handle, address, size, map_perm);
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::UnmapTransferMemory) => {
            // IN: handle=arg32[1], address=arg32[2], size=arg32[3]; OUT: ret=arg32[0]
            let handle = get_arg32(args, 1);
            let address = get_arg32(args, 2) as u64;
            let size = get_arg32(args, 3) as u64;
            let result = svc_transfer_memory::unmap_transfer_memory(system, handle, address, size);
            set_arg32(args, 0, result.get_inner_value());
        }

        // =====================================================================
        // Debug / Exception
        // =====================================================================
        Some(SvcId::Break) => {
            // IN: reason=arg32[0], arg=arg32[1], size=arg32[2]; OUT: (none)
            let reason = get_arg32(args, 0);
            let info1 = get_arg32(args, 1) as u64;
            let info2 = get_arg32(args, 2) as u64;
            svc_exception::break64_from_32(system, reason, info1 as u32, info2 as u32, args);
        }
        Some(SvcId::OutputDebugString) => {
            // IN: str=arg32[0], len=arg32[1]; OUT: ret=arg32[0]
            let str_ptr = get_arg32(args, 0) as u64;
            let str_len = get_arg32(args, 1) as u64;
            let result = svc_debug_string::output_debug_string(system, str_ptr, str_len);
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::ReturnFromException) => {
            // IN: result=arg32[0]; OUT: (none)
        }

        // =====================================================================
        // Info
        // =====================================================================
        Some(SvcId::GetInfo) => {
            // IN: info_type=arg32[1], handle=arg32[2], info_subtype=gather64[0,3]
            // OUT: ret=arg32[0], out=scatter64[1,2]
            let info_type_raw = get_arg32(args, 1);
            let handle = get_arg32(args, 2);
            let info_subtype = gather64(args, 0, 3);
            let info_type = crate::hle::kernel::svc::svc_types::InfoType::from_u32(info_type_raw);
            let mut value: u64 = 0;
            let result = svc_info::get_info(system, &mut value, info_type, handle, info_subtype);
            set_arg32(args, 0, result.get_inner_value());
            scatter64(args, 1, 2, value);
        }
        Some(SvcId::GetSystemInfo) => {
            let mut value: u64 = 0;
            let result = match crate::hle::kernel::svc::svc_types::SystemInfoType::from_u32(
                get_arg32(args, 1),
            ) {
                Some(info_type) => svc_info::get_system_info(
                    system,
                    &mut value,
                    info_type,
                    get_arg32(args, 2),
                    gather64(args, 0, 3),
                ),
                None => crate::hle::kernel::svc::svc_results::RESULT_INVALID_ENUM_VALUE,
            };
            set_arg32(args, 0, result.get_inner_value());
            scatter64(args, 1, 2, value);
        }

        // =====================================================================
        // Resource limits
        // =====================================================================
        Some(SvcId::GetResourceLimitLimitValue) => {
            // IN: handle=arg32[1], which=arg32[2]; OUT: ret=arg32[0], value=scatter64[1,2]
            set_arg32(args, 0, STUB_SUCCESS);
            scatter64(args, 1, 2, 0x1_0000_0000); // 4 GiB
        }
        Some(SvcId::GetResourceLimitCurrentValue) => {
            set_arg32(args, 0, STUB_SUCCESS);
            scatter64(args, 1, 2, 0);
        }
        Some(SvcId::GetResourceLimitPeakValue) => {
            set_arg32(args, 0, STUB_SUCCESS);
            scatter64(args, 1, 2, 0);
        }
        Some(SvcId::CreateResourceLimit) => {
            // OUT: ret=arg32[0], handle=arg32[1]
            set_arg32(args, 0, STUB_SUCCESS);
            set_arg32(args, 1, alloc_stub_handle(system));
        }
        Some(SvcId::SetResourceLimitLimitValue) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }

        // =====================================================================
        // Cache
        // =====================================================================
        Some(SvcId::FlushEntireDataCache) => {
            svc_cache::flush_entire_data_cache();
        }
        Some(SvcId::FlushDataCache) => {
            let result =
                svc_cache::flush_data_cache(get_arg32(args, 0) as u64, get_arg32(args, 1) as u64);
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::InvalidateProcessDataCache) => {
            let result = svc_cache::invalidate_process_data_cache(
                get_arg32(args, 0),
                gather64(args, 2, 3),
                gather64(args, 1, 4),
            );
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::StoreProcessDataCache) => {
            let result = svc_cache::store_process_data_cache(
                get_arg32(args, 0),
                gather64(args, 2, 3),
                gather64(args, 1, 4),
            );
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::FlushProcessDataCache) => {
            let result = svc_cache::flush_process_data_cache(
                system,
                get_arg32(args, 0),
                gather64(args, 2, 3),
                gather64(args, 1, 4),
            );
            set_arg32(args, 0, result.get_inner_value());
        }

        // =====================================================================
        // Code memory
        // =====================================================================
        Some(SvcId::CreateCodeMemory) => {
            let mut handle = 0;
            let result = svc_code_memory::create_code_memory(
                system,
                &mut handle,
                get_arg32(args, 1) as u64,
                get_arg32(args, 2) as u64,
            );
            set_arg32(args, 0, result.get_inner_value());
            set_arg32(args, 1, handle);
        }
        Some(SvcId::ControlCodeMemory) => {
            let result = svc_code_memory::control_code_memory(
                system,
                get_arg32(args, 0),
                get_arg32(args, 1),
                gather64(args, 2, 3),
                gather64(args, 4, 5),
                decode_memory_permission(get_arg32(args, 6)),
            );
            set_arg32(args, 0, result.get_inner_value());
        }

        // =====================================================================
        // Debug SVCs
        // =====================================================================
        Some(SvcId::DebugActiveProcess) => {
            set_arg32(args, 0, STUB_SUCCESS);
            set_arg32(args, 1, alloc_stub_handle(system));
        }
        Some(SvcId::BreakDebugProcess) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::TerminateDebugProcess) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::GetDebugEvent) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::ContinueDebugEvent) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::GetDebugThreadContext) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::SetDebugThreadContext) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::QueryDebugProcessMemory) => {
            set_arg32(args, 0, STUB_SUCCESS);
            set_arg32(args, 1, 0);
        }
        Some(SvcId::ReadDebugProcessMemory) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::WriteDebugProcessMemory) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::SetHardwareBreakPoint) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::GetDebugThreadParam) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::GetDebugFutureThreadInfo) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::GetLastThreadInfo) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }

        // =====================================================================
        // IO / Device address space
        // =====================================================================
        Some(SvcId::CreateIoPool) => {
            set_arg32(args, 0, STUB_SUCCESS);
            set_arg32(args, 1, alloc_stub_handle(system));
        }
        Some(SvcId::CreateIoRegion) => {
            set_arg32(args, 0, STUB_SUCCESS);
            set_arg32(args, 1, alloc_stub_handle(system));
        }
        Some(SvcId::MapIoRegion) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::UnmapIoRegion) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::CreateInterruptEvent) => {
            set_arg32(args, 0, STUB_SUCCESS);
            set_arg32(args, 1, alloc_stub_handle(system));
        }
        Some(SvcId::QueryPhysicalAddress) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::QueryIoMapping) => {
            set_arg32(args, 0, STUB_SUCCESS);
            set_arg32(args, 1, 0);
            set_arg32(args, 2, 0);
        }
        Some(SvcId::CreateDeviceAddressSpace) => {
            set_arg32(args, 0, STUB_SUCCESS);
            set_arg32(args, 1, alloc_stub_handle(system));
        }
        Some(SvcId::AttachDeviceAddressSpace) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::DetachDeviceAddressSpace) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::MapDeviceAddressSpaceByForce) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::MapDeviceAddressSpaceAligned) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::UnmapDeviceAddressSpace) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }

        // =====================================================================
        // Process memory
        // =====================================================================
        Some(SvcId::SetProcessMemoryPermission) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::MapProcessMemory) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::UnmapProcessMemory) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }
        Some(SvcId::QueryProcessMemory) => {
            set_arg32(args, 0, STUB_SUCCESS);
            set_arg32(args, 1, 0);
        }
        Some(SvcId::MapProcessCodeMemory) => {
            let result = svc_process_memory::map_process_code_memory(
                system,
                get_arg32(args, 0),
                gather64(args, 2, 3),
                gather64(args, 1, 4),
                gather64(args, 5, 6),
            );
            set_arg32(args, 0, result.get_inner_value());
        }
        Some(SvcId::UnmapProcessCodeMemory) => {
            let result = svc_process_memory::unmap_process_code_memory(
                system,
                get_arg32(args, 0),
                gather64(args, 2, 3),
                gather64(args, 1, 4),
                gather64(args, 5, 6),
            );
            set_arg32(args, 0, result.get_inner_value());
        }

        // =====================================================================
        // Misc
        // =====================================================================
        Some(SvcId::KernelDebug) => {}
        Some(SvcId::ChangeKernelTraceState) => {}
        Some(SvcId::SleepSystem) => {}
        Some(SvcId::ReadWriteRegister) => {
            set_arg32(args, 0, STUB_SUCCESS);
            set_arg32(args, 1, 0);
        }
        Some(SvcId::CallSecureMonitor) => {
            set_arg32(args, 0, STUB_SUCCESS);
        }

        None => {
            log_unknown_svc_context(system, imm, false);
            set_arg32(args, 0, 0xF001); // Generic error
        }
    }
}

// =============================================================================
// 64-bit dispatch (AArch64 / LP64)
// =============================================================================

/// Dispatch a 64-bit (AArch64 / LP64) SVC.
///
/// Corresponds to upstream `Call64`. Register layouts match the auto-generated
/// SvcWrap_*64 wrappers in svc.cpp.
fn call64(system: &System, imm: u32, args: &mut SvcArgs) {
    match SvcId::from_u32(imm) {
        Some(SvcId::SetHeapSize) => {
            let size = get_arg64(args, 1);
            let mut heap_base = 0;
            let result =
                svc_physical_memory::set_heap_size_current_process(system, &mut heap_base, size);
            set_arg64(args, 0, result.get_inner_value() as u64);
            set_arg64(args, 1, heap_base);
        }
        Some(SvcId::SetMemoryPermission) => {
            let address = get_arg64(args, 0);
            let size = get_arg64(args, 1);
            let perm = get_arg64(args, 2) as u32;
            let result = svc_memory::set_memory_permission(
                system,
                address,
                size,
                decode_memory_permission(perm),
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::SetMemoryAttribute) => {
            let address = get_arg64(args, 0);
            let size = get_arg64(args, 1);
            let mask = get_arg64(args, 2) as u32;
            let attr = get_arg64(args, 3) as u32;
            let result = svc_memory::set_memory_attribute(system, address, size, mask, attr);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::MapMemory) => {
            let dst = get_arg64(args, 0);
            let src = get_arg64(args, 1);
            let size = get_arg64(args, 2);
            let result = svc_memory::map_memory(system, dst, src, size);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::UnmapMemory) => {
            let dst = get_arg64(args, 0);
            let src = get_arg64(args, 1);
            let size = get_arg64(args, 2);
            let result = svc_memory::unmap_memory(system, dst, src, size);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::QueryMemory) => {
            let mem_info_ptr = get_arg64(args, 0);
            let query_addr = get_arg64(args, 2);
            let mut page_info = crate::hle::kernel::svc::svc_types::PageInfo::default();
            let result =
                svc_query_memory::query_memory(system, mem_info_ptr, &mut page_info, query_addr);
            set_arg64(args, 0, result.get_inner_value() as u64);
            set_arg64(args, 1, page_info.flags as u64);
        }
        Some(SvcId::ExitProcess) => {
            svc_process::exit_process(system);
        }
        Some(SvcId::CreateThread) => {
            let mut out_handle = 0;
            let result = svc_thread::create_thread(
                system,
                &mut out_handle,
                get_arg64(args, 1),
                get_arg64(args, 2),
                get_arg64(args, 3),
                get_arg64(args, 4) as i32,
                get_arg64(args, 5) as i32,
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
            set_arg64(args, 1, out_handle as u64);
        }
        Some(SvcId::StartThread) => {
            let result = svc_thread::start_thread(system, get_arg64(args, 0) as u32);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::ExitThread) => {
            svc_thread::exit_thread(system);
        }
        Some(SvcId::SleepThread) => {
            let ns = get_arg64(args, 0) as i64;
            svc_thread::sleep_thread(system, ns);
        }
        Some(SvcId::GetThreadPriority) => {
            let mut out_priority = 0;
            let result = svc_thread::get_thread_priority(
                system,
                &mut out_priority,
                get_arg64(args, 1) as u32,
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
            set_arg64(args, 1, out_priority as u64);
        }
        Some(SvcId::SetThreadPriority) => {
            let result = svc_thread::set_thread_priority(
                system,
                get_arg64(args, 0) as u32,
                get_arg64(args, 1) as i32,
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::GetThreadCoreMask) => {
            let mut out_core_id = 0;
            let mut out_affinity_mask = 0;
            let result = svc_thread::get_thread_core_mask(
                system,
                &mut out_core_id,
                &mut out_affinity_mask,
                get_arg64(args, 2) as u32,
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
            set_arg64(args, 1, out_core_id as u64);
            set_arg64(args, 2, out_affinity_mask);
        }
        Some(SvcId::SetThreadCoreMask) => {
            let result = svc_thread::set_thread_core_mask(
                system,
                get_arg64(args, 0) as u32,
                get_arg64(args, 1) as i32,
                get_arg64(args, 2),
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::GetCurrentProcessorNumber) => {
            set_arg64(
                args,
                0,
                svc_processor::get_current_processor_number(system) as u64,
            );
        }
        Some(SvcId::SetThreadActivity) => {
            let result = match get_arg64(args, 1) as u32 {
                0 => svc_activity::set_thread_activity(
                    system,
                    get_arg64(args, 0) as u32,
                    crate::hle::kernel::svc::svc_types::ThreadActivity::Runnable,
                ),
                1 => svc_activity::set_thread_activity(
                    system,
                    get_arg64(args, 0) as u32,
                    crate::hle::kernel::svc::svc_types::ThreadActivity::Paused,
                ),
                _ => crate::hle::kernel::svc::svc_results::RESULT_INVALID_ENUM_VALUE,
            };
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::SignalEvent) => {
            let result = svc_event::signal_event(system, get_arg64(args, 0) as u32);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::ClearEvent) => {
            let result = svc_event::clear_event(system, get_arg64(args, 0) as u32);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::GetThreadContext3) => {
            let result = svc_thread::get_thread_context3(
                system,
                get_arg64(args, 0),
                get_arg64(args, 1) as u32,
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::CloseHandle) => {
            let result = svc_synchronization::close_handle(system, get_arg64(args, 0) as u32);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::ResetSignal) => {
            let result = svc_synchronization::reset_signal(system, get_arg64(args, 0) as u32);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::GetSystemTick) => {
            set_arg64(args, 0, svc_tick::get_system_tick(system) as u64);
        }
        // ====================================================================
        // Synchronization (A64 — uses 64-bit args directly, no gather64 needed)
        // ====================================================================
        Some(SvcId::WaitSynchronization) => {
            // IN: handles=arg[1], num=arg[2], timeout=arg[3]; OUT: ret=arg[0], index=arg[1]
            let mut out_index = -1i32;
            let result = svc_synchronization::wait_synchronization(
                system,
                &mut out_index,
                get_arg64(args, 1),
                get_arg64(args, 2) as i32,
                get_arg64(args, 3) as i64,
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
            set_arg64(args, 1, out_index as u64);
        }
        Some(SvcId::CancelSynchronization) => {
            let result =
                svc_synchronization::cancel_synchronization(system, get_arg64(args, 0) as u32);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::ArbitrateLock) => {
            let result = svc_lock::arbitrate_lock(
                system,
                get_arg64(args, 0) as u32, // thread_handle
                get_arg64(args, 1),        // address (u64 — full 64 bits)
                get_arg64(args, 2) as u32, // tag
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::ArbitrateUnlock) => {
            let result = svc_lock::arbitrate_unlock(system, get_arg64(args, 0));
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::WaitProcessWideKeyAtomic) => {
            let result = svc_condition_variable::wait_process_wide_key_atomic(
                system,
                get_arg64(args, 0),        // address
                get_arg64(args, 1),        // cv_key
                get_arg64(args, 2) as u32, // tag
                get_arg64(args, 3) as i64, // timeout
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::SignalProcessWideKey) => {
            svc_condition_variable::signal_process_wide_key(
                system,
                get_arg64(args, 0),        // cv_key
                get_arg64(args, 1) as i32, // count
            );
        }
        Some(SvcId::WaitForAddress) => {
            let result = svc_address_arbiter::wait_for_address(
                system,
                get_arg64(args, 0),
                get_arg64(args, 1) as u32,
                get_arg64(args, 2) as i32,
                get_arg64(args, 3) as i64,
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::SignalToAddress) => {
            let result = svc_address_arbiter::signal_to_address(
                system,
                get_arg64(args, 0),
                get_arg64(args, 1) as u32,
                get_arg64(args, 2) as i32,
                get_arg64(args, 3) as i32,
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::CreateEvent) => {
            let mut write_handle = 0;
            let mut read_handle = 0;
            let result = svc_event::create_event(system, &mut write_handle, &mut read_handle);
            set_arg64(args, 0, result.get_inner_value() as u64);
            set_arg64(args, 1, write_handle as u64);
            set_arg64(args, 2, read_handle as u64);
        }

        // ====================================================================
        // Shared / Transfer memory (A64)
        // ====================================================================
        Some(SvcId::MapSharedMemory) => {
            let handle = get_arg64(args, 0) as u32;
            let address = get_arg64(args, 1);
            let size = get_arg64(args, 2);
            let perm = get_arg64(args, 3) as u32;
            let map_perm = unsafe {
                std::mem::transmute::<u32, crate::hle::kernel::svc::svc_types::MemoryPermission>(
                    perm,
                )
            };
            let result =
                svc_shared_memory::map_shared_memory(system, handle, address, size, map_perm);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::UnmapSharedMemory) => {
            let handle = get_arg64(args, 0) as u32;
            let address = get_arg64(args, 1);
            let size = get_arg64(args, 2);
            let result = svc_shared_memory::unmap_shared_memory(system, handle, address, size);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }

        Some(SvcId::ConnectToNamedPort) => {
            let mut out = 0;
            let result = svc_port::connect_to_named_port(system, &mut out, get_arg64(args, 1));
            set_arg64(args, 0, result.get_inner_value() as u64);
            set_arg64(args, 1, out as u64);
        }
        Some(SvcId::SendSyncRequest) => {
            let result = svc_ipc::send_sync_request(system, get_arg64(args, 0) as u32);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::GetProcessId) => {
            set_arg64(args, 0, STUB_SUCCESS as u64);
            set_arg64(args, 1, 1); // pid=1
        }
        Some(SvcId::GetThreadId) => {
            let mut out_thread_id = 0;
            let result =
                svc_thread::get_thread_id(system, &mut out_thread_id, get_arg64(args, 1) as u32);
            set_arg64(args, 0, result.get_inner_value() as u64);
            set_arg64(args, 1, out_thread_id);
        }
        Some(SvcId::Break) => {
            let reason = get_arg64(args, 0) as u32;
            let info1 = get_arg64(args, 1);
            let info2 = get_arg64(args, 2);
            svc_exception::break64(system, reason, info1, info2);
        }
        Some(SvcId::OutputDebugString) => {
            let str_ptr = get_arg64(args, 0);
            let str_len = get_arg64(args, 1);
            let result = svc_debug_string::output_debug_string(system, str_ptr, str_len);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::GetInfo) => {
            let info_type_raw = get_arg64(args, 1) as u32;
            let handle = get_arg64(args, 2) as u32;
            let info_subtype = get_arg64(args, 3);
            let info_type = crate::hle::kernel::svc::svc_types::InfoType::from_u32(info_type_raw);
            let mut value: u64 = 0;
            let result = svc_info::get_info(system, &mut value, info_type, handle, info_subtype);
            set_arg64(args, 0, result.get_inner_value() as u64);
            set_arg64(args, 1, value);
        }
        Some(SvcId::GetSystemInfo) => {
            let mut value = 0;
            let result = match crate::hle::kernel::svc::svc_types::SystemInfoType::from_u32(
                get_arg64(args, 1) as u32,
            ) {
                Some(info_type) => svc_info::get_system_info(
                    system,
                    &mut value,
                    info_type,
                    get_arg64(args, 2) as u32,
                    get_arg64(args, 3),
                ),
                None => crate::hle::kernel::svc::svc_results::RESULT_INVALID_ENUM_VALUE,
            };
            set_arg64(args, 0, result.get_inner_value() as u64);
            set_arg64(args, 1, value);
        }
        Some(SvcId::MapPhysicalMemory) => {
            let address = get_arg64(args, 0);
            let size = get_arg64(args, 1);
            let result = svc_physical_memory::map_physical_memory(system, address, size);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::UnmapPhysicalMemory) => {
            let address = get_arg64(args, 0);
            let size = get_arg64(args, 1);
            let result = svc_physical_memory::unmap_physical_memory(system, address, size);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::MapPhysicalMemoryUnsafe) => {
            let address = get_arg64(args, 0);
            let size = get_arg64(args, 1);
            let result = svc_physical_memory::map_physical_memory_unsafe(address, size);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::UnmapPhysicalMemoryUnsafe) => {
            let address = get_arg64(args, 0);
            let size = get_arg64(args, 1);
            let result = svc_physical_memory::unmap_physical_memory_unsafe(address, size);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::SetUnsafeLimit) => {
            let limit = get_arg64(args, 0);
            let result = svc_physical_memory::set_unsafe_limit(limit);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        // ====================================================================
        // Cache (A64)
        // ====================================================================
        Some(SvcId::FlushEntireDataCache) => {
            svc_cache::flush_entire_data_cache();
        }
        Some(SvcId::FlushDataCache) => {
            let result = svc_cache::flush_data_cache(get_arg64(args, 0), get_arg64(args, 1));
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::InvalidateProcessDataCache) => {
            let result = svc_cache::invalidate_process_data_cache(
                get_arg64(args, 0) as u32,
                get_arg64(args, 1),
                get_arg64(args, 2),
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::StoreProcessDataCache) => {
            let result = svc_cache::store_process_data_cache(
                get_arg64(args, 0) as u32,
                get_arg64(args, 1),
                get_arg64(args, 2),
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::FlushProcessDataCache) => {
            let result = svc_cache::flush_process_data_cache(
                system,
                get_arg64(args, 0) as u32,
                get_arg64(args, 1),
                get_arg64(args, 2),
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::CreateCodeMemory) => {
            let mut handle = 0;
            let result = svc_code_memory::create_code_memory(
                system,
                &mut handle,
                get_arg64(args, 1),
                get_arg64(args, 2),
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
            set_arg64(args, 1, handle as u64);
        }
        Some(SvcId::ControlCodeMemory) => {
            let result = svc_code_memory::control_code_memory(
                system,
                get_arg64(args, 0) as u32,
                get_arg64(args, 1) as u32,
                get_arg64(args, 2),
                get_arg64(args, 3),
                decode_memory_permission(get_arg64(args, 4) as u32),
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        // Transfer memory — STK creates one between binder Connect and the
        // first DequeueBuffer. The 32-bit dispatch (`call32`) handles
        // these; without the parallel call64 arms STK saw a stub-success
        // with handle=0 and bailed out via Disconnect (txn=11), so no
        // frame ever reached the producer queue.
        Some(SvcId::CreateTransferMemory) => {
            // IN: address=arg64[1], size=arg64[2], perm=arg64[3]
            // OUT: ret=arg64[0], handle=arg64[1]
            let address = get_arg64(args, 1);
            let size = get_arg64(args, 2);
            let map_perm = unsafe { std::mem::transmute(get_arg64(args, 3) as u32) };
            let mut handle = 0u32;
            let result = svc_transfer_memory::create_transfer_memory(
                system,
                &mut handle,
                address,
                size,
                map_perm,
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
            set_arg64(args, 1, handle as u64);
        }
        Some(SvcId::MapTransferMemory) => {
            // IN: handle=arg64[0], address=arg64[1], size=arg64[2], perm=arg64[3]
            let handle = get_arg64(args, 0) as u32;
            let address = get_arg64(args, 1);
            let size = get_arg64(args, 2);
            let map_perm = unsafe { std::mem::transmute(get_arg64(args, 3) as u32) };
            let result =
                svc_transfer_memory::map_transfer_memory(system, handle, address, size, map_perm);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::UnmapTransferMemory) => {
            // IN: handle=arg64[0], address=arg64[1], size=arg64[2]
            let handle = get_arg64(args, 0) as u32;
            let address = get_arg64(args, 1);
            let size = get_arg64(args, 2);
            let result = svc_transfer_memory::unmap_transfer_memory(system, handle, address, size);
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::MapProcessCodeMemory) => {
            let result = svc_process_memory::map_process_code_memory(
                system,
                get_arg64(args, 0) as u32,
                get_arg64(args, 1),
                get_arg64(args, 2),
                get_arg64(args, 3),
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::UnmapProcessCodeMemory) => {
            let result = svc_process_memory::unmap_process_code_memory(
                system,
                get_arg64(args, 0) as u32,
                get_arg64(args, 1),
                get_arg64(args, 2),
                get_arg64(args, 3),
            );
            set_arg64(args, 0, result.get_inner_value() as u64);
        }
        Some(SvcId::SynchronizePreemptionState) => {
            svc_synchronization::synchronize_preemption_state(system);
        }
        Some(SvcId::GetResourceLimitLimitValue) => {
            set_arg64(args, 0, STUB_SUCCESS as u64);
            set_arg64(args, 1, 0x1_0000_0000);
        }
        Some(SvcId::GetResourceLimitCurrentValue) => {
            set_arg64(args, 0, STUB_SUCCESS as u64);
            set_arg64(args, 1, 0);
        }
        // All remaining 64-bit SVCs return stub success
        Some(svc_id) => {
            log::warn!("SVC Call64: {:?} (0x{:02X}) stub", svc_id, imm);
            set_arg64(args, 0, STUB_SUCCESS as u64);
        }
        None => {
            log_unknown_svc_context(system, imm, true);
            set_arg64(args, 0, 0xF001);
        }
    }
}

// =============================================================================
// Main entry point
// =============================================================================

/// Main SVC entry point. Called by the CPU emulation core when an SVC
/// instruction is executed.
///
/// Corresponds to upstream `Kernel::Svc::Call`.
///
/// In upstream, this function:
/// 1. Saves SVC arguments from the physical core
/// 2. Enters the SVC profile
/// 3. Dispatches to Call32 or Call64 based on process bitness
/// 4. Exits the SVC profile
/// 5. Loads SVC arguments back to the physical core
pub fn call(system: &System, imm: u32, is_64bit: bool, args: &mut SvcArgs) {
    let mut dispatch_args = *args;

    // Per-core SVC-in-progress tracker for the SIGUSR1 dumper.
    let svc_track_core = {
        let core_id = if let Some(kernel) = system.kernel() {
            kernel.current_physical_core_index() as usize
        } else {
            usize::MAX
        };
        let tid = system
            .current_thread()
            .and_then(|t| t.lock().ok().map(|g| g.get_thread_id()))
            .unwrap_or(0);

        // Env-gated per-tid svc tracer. `RUZU_TRACE_TID_SVC=N` logs every
        // svc call from thread N. `RUZU_TRACE_TID_SVC=*` (or `all`) logs all
        // threads. `RUZU_TRACE_TID_SVC=N,M,...` logs the listed thread ids.
        // Used to figure out what a "silent" game-main thread is actually
        // doing when it appears wedged but is still being scheduled.
        //
        // Each line also carries a nanosecond timestamp `t_ns=...` measured
        // from a process-lifetime anchor (first call to `race_anchor`). Use
        // it to compute relative timing between SVCs across threads when
        // analyzing parent/worker dispatch races.
        // Cache the env-var lookup. Cost was visible in perf samples on the
        // SVC dispatch hot path.
        fn trace_tid_svc_env() -> Option<&'static std::ffi::OsString> {
            use std::sync::OnceLock;
            static CACHED: OnceLock<Option<std::ffi::OsString>> = OnceLock::new();
            CACHED
                .get_or_init(|| std::env::var_os("RUZU_TRACE_TID_SVC"))
                .as_ref()
        }
        // TID_SVC tracer — routes through the non-blocking trace ring.
        // Enabled via `[svc] tid_svc = true` in trace.toml, or via the
        // legacy `RUZU_TRACE_TID_SVC` env var. The env var doubles as a
        // tid filter: `RUZU_TRACE_TID_SVC=75,86` only traces those tids;
        // `*` or `all` (or just the TOML flag) traces every tid.
        if common::trace::is_enabled(common::trace::cat::TID_SVC) {
            let want = match trace_tid_svc_env() {
                Some(target_str) => {
                    let s = target_str.to_string_lossy();
                    s == "*"
                        || s == "all"
                        || s.split(',')
                            .filter_map(|p| p.trim().parse::<u64>().ok())
                            .any(|t| t == tid)
                }
                None => true, // TOML enabled with no env filter ⇒ trace all tids
            };
            if want {
                let name = SvcId::from_u32(imm)
                    .map(|id| format!("{:?}", id))
                    .unwrap_or_else(|| format!("svc#0x{:02X}", imm));
                let name_id = common::trace::intern_svc_name(&name);
                let t_ns = super::race_anchor::elapsed_ns();
                let (pc, lr, fp, sp, word_at_fp_plus_4, word_at_fp_plus_20) =
                    if common::trace::tid_svc_pc_enabled() {
                        if let Some(kernel) = system.kernel() {
                            let core_index = kernel.current_physical_core_index() as usize;
                            if let Some(process_arc) = system.current_process_arc_opt() {
                                let process = process_arc.lock().unwrap();
                                if let Some(jit) = process.get_arm_interface(core_index) {
                                    use crate::arm::arm_interface::ThreadContext;
                                    let mut ctx = ThreadContext::default();
                                    jit.get_context(&mut ctx);
                                    let fp4 = process.read_memory_vec(ctx.fp.saturating_add(4), 4);
                                    let fp4 = fp4
                                        .get(..4)
                                        .and_then(|bytes| bytes.try_into().ok())
                                        .map(u32::from_le_bytes)
                                        .unwrap_or(0);
                                    let fp20 =
                                        process.read_memory_vec(ctx.fp.saturating_add(20), 4);
                                    let fp20 = fp20
                                        .get(..4)
                                        .and_then(|bytes| bytes.try_into().ok())
                                        .map(u32::from_le_bytes)
                                        .unwrap_or(0);
                                    (ctx.pc, ctx.lr, ctx.fp, ctx.sp, fp4 as u64, fp20 as u64)
                                } else {
                                    (0, 0, 0, 0, 0, 0)
                                }
                            } else {
                                (0, 0, 0, 0, 0, 0)
                            }
                        } else {
                            (0, 0, 0, 0, 0, 0)
                        }
                    } else {
                        (0, 0, 0, 0, 0, 0)
                    };
                common::trace::emit_raw(
                    common::trace::cat::TID_SVC,
                    &[
                        t_ns,
                        tid,
                        core_id as u64,
                        name_id as u64,
                        pc as u64,
                        lr as u64,
                        dispatch_args[0],
                        dispatch_args[1],
                        dispatch_args[2],
                        dispatch_args[3],
                        fp,
                        sp,
                        word_at_fp_plus_4,
                        word_at_fp_plus_20,
                    ],
                );
            }
        }
        if core_id < hardware_properties::NUM_CPU_CORES as usize {
            super::kernel::mark_svc_enter(core_id, tid, imm);
            core_id
        } else {
            usize::MAX
        }
    };
    struct SvcExitGuard(usize);
    impl Drop for SvcExitGuard {
        fn drop(&mut self) {
            if self.0 < hardware_properties::NUM_CPU_CORES as usize {
                super::kernel::mark_svc_exit(self.0);
            }
        }
    }
    let _svc_exit_guard = SvcExitGuard(svc_track_core);

    if let Some(kernel) = system.kernel() {
        if let Some(process_arc) = system.current_process_arc_opt() {
            let process = process_arc.lock().unwrap();
            kernel
                .current_physical_core()
                .save_svc_arguments(&process, &mut dispatch_args);
        }
    }

    // Structured SVC trace — enabled by RUZU_SVC_TRACE=1
    // Format matches zuyu trace: [  T.TTTTTT] SVC_IN  imm=0xNN core=C tid=T args=[...]
    let trace_enabled = super::trace_format::is_svc_trace_enabled();

    // Always compute tid so the PC-window tracker works even when
    // RUZU_SVC_TRACE is off.
    let (core_id, tid) = {
        if let Some(thread) = system.current_thread() {
            let t = thread.lock().unwrap();
            (t.get_current_core(), t.get_thread_id() as i64)
        } else {
            (-1, -1)
        }
    };
    record_svc_summary(tid, core_id, imm);
    // PC-window SVC counter. By default counts on tid=73 (main game thread);
    // RUZU_TRACE_PC_TID=N overrides to count a different thread (e.g. tid=102
    // for the audio worker). Increment on every SVC entry and deactivate the
    // PC trace when we enter the closing SVC (count == end).
    let svc_count_here: u64 = if tid == pc_trace_target_tid() {
        use std::sync::atomic::Ordering;
        let n = SVC_MAIN_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some((_start, end)) = pc_trace_window() {
            if n == end {
                pc_trace_active_set(false);
                eprintln!("[TRACE_PC] ---- window end at SVC #{} entry ----", n);
            }
        }
        n
    } else {
        0
    };

    // RUZU_PROFILE_WAKE: if this thread had a pending wake-emit timestamp
    // recorded by KConditionVariable::signal_impl, consume it now — the
    // thread is about to enter a fresh SVC, so it's already running again.
    if tid >= 0 {
        consume_wake_latency(tid as u64);
        // RUZU_PROFILE_GAP: time spent in JIT since this tid's last SVC exit.
        gap_on_svc_entry(tid as u64);
    }

    // Cache env-var lookups behind OnceLock — profile showed getenv at 7%+
    // of CPU when the SVC dispatch hot path re-reads on every call. Mirrors
    // the same pattern in rdynarmic's emit::emit_block (RDYNARMIC_PROFILE_OPCODES).
    fn profile_svc_enabled() -> bool {
        use std::sync::OnceLock;
        static CACHED: OnceLock<bool> = OnceLock::new();
        *CACHED.get_or_init(|| std::env::var_os("RUZU_PROFILE_SVC").is_some())
    }
    fn profile_svc_per_tid_enabled() -> bool {
        use std::sync::OnceLock;
        static CACHED: OnceLock<bool> = OnceLock::new();
        *CACHED.get_or_init(|| std::env::var_os("RUZU_PROFILE_SVC_PER_TID").is_some())
    }
    let profile_svc = profile_svc_enabled();
    let profile_svc_per_tid = profile_svc_per_tid_enabled();
    let svc_start = if profile_svc || profile_svc_per_tid {
        Some(std::time::Instant::now())
    } else {
        None
    };

    if trace_enabled {
        // Log SVC entry
        eprintln!(
            "[{:>10.6}] SVC_IN  imm={:#04x} core={} tid={} args=[{:#x},{:#x},{:#x},{:#x},{:#x},{:#x},{:#x},{:#x}]",
            super::trace_format::elapsed_secs(),
            imm,
            core_id,
            tid,
            dispatch_args[0],
            dispatch_args[1],
            dispatch_args[2],
            dispatch_args[3],
            dispatch_args[4],
            dispatch_args[5],
            dispatch_args[6],
            dispatch_args[7]
        );

        if svc_trace_full_regs_enabled() {
            dump_svc_full_regs(system, imm, tid, "REGS_IN ");
        }

        if is_64bit {
            call64(system, imm, &mut dispatch_args);
        } else {
            call32(system, imm, &mut dispatch_args);
        }

        // Log SVC exit
        eprintln!(
            "[{:>10.6}] SVC_OUT imm={:#04x} args=[{:#x},{:#x},{:#x},{:#x},{:#x},{:#x},{:#x},{:#x}]",
            super::trace_format::elapsed_secs(),
            imm,
            dispatch_args[0],
            dispatch_args[1],
            dispatch_args[2],
            dispatch_args[3],
            dispatch_args[4],
            dispatch_args[5],
            dispatch_args[6],
            dispatch_args[7]
        );

        if svc_trace_full_regs_enabled() {
            dump_svc_full_regs(system, imm, tid, "REGS_OUT");
        }
    } else {
        if is_64bit {
            call64(system, imm, &mut dispatch_args);
        } else {
            call32(system, imm, &mut dispatch_args);
        }
    }

    if let Some(start) = svc_start {
        let elapsed = start.elapsed();
        if profile_svc {
            record_svc_profile(imm, elapsed);
        }
        if profile_svc_per_tid {
            record_svc_per_tid(tid, imm, elapsed);
        }
    }

    // RUZU_PROFILE_GAP: stamp this tid's last-SVC-exit-time so the next SVC
    // entry on the same tid can compute the in-JIT gap.
    if tid >= 0 {
        gap_on_svc_exit(tid as u64);
    }

    // After the handler returns, if we just finished SVC #start on the
    // target thread (default 73, see RUZU_TRACE_PC_TID), activate PC tracing
    // so the next block boundaries get logged.
    if tid == pc_trace_target_tid() {
        if let Some((start, _end)) = pc_trace_window() {
            if svc_count_here == start {
                eprintln!(
                    "[TRACE_PC] ---- window start after SVC #{} exit (tid={}) ----",
                    svc_count_here, tid
                );
                pc_trace_active_set(true);
            }
        }
    }

    // RUZU_TRACE_SVC_ERRVAL=0xNNNN — non-blocking ring trace for every SVC
    // whose returned r0 equals the requested result code. `RUZU_TRACE_INVALID_HANDLE_SVC`
    // is a convenience alias for ResultInvalidHandle (0xE401).
    {
        fn svc_errval_target() -> Option<u64> {
            use std::sync::OnceLock;
            static CACHED: OnceLock<Option<u64>> = OnceLock::new();
            *CACHED.get_or_init(|| {
                if std::env::var_os("RUZU_TRACE_INVALID_HANDLE_SVC").is_some() {
                    return Some(
                        crate::hle::kernel::svc::svc_results::RESULT_INVALID_HANDLE
                            .get_inner_value() as u64,
                    );
                }
                std::env::var("RUZU_TRACE_SVC_ERRVAL").ok().and_then(|s| {
                    let s = s.trim();
                    let s = s
                        .strip_prefix("0x")
                        .or_else(|| s.strip_prefix("0X"))
                        .unwrap_or(s);
                    u64::from_str_radix(s, 16).ok()
                })
            })
        }
        if let Some(target) = svc_errval_target() {
            // Silent ring of handle-related SVCs (CloseHandle, SendSyncRequest,
            // WaitSynchronization, ResetSignal, SignalEvent, ClearEvent). No
            // I/O on the hot path — synchronous eprintln tracing makes the
            // boot race vanish (heisenbug).
            if matches!(imm, 0x16 | 0x21 | 0x18 | 0x12 | 0x11 | 0x13) && tid >= 0 {
                super::handle_forensics::record_svc(
                    tid as u64,
                    imm,
                    [args[0], args[1], args[2], args[3]],
                    dispatch_args[0],
                );
            }
            if dispatch_args[0] == target
                && common::trace::is_enabled(common::trace::cat::SVC_ERRVAL)
            {
                let (pc, lr) = if let Some(kernel) = system.kernel() {
                    let core_index = kernel.current_physical_core_index() as usize;
                    if let Some(process_arc) = system.current_process_arc_opt() {
                        let process = process_arc.lock().unwrap();
                        if let Some(jit) = process.get_arm_interface(core_index) {
                            use crate::arm::arm_interface::ThreadContext;
                            let mut ctx = ThreadContext::default();
                            jit.get_context(&mut ctx);
                            (ctx.pc, ctx.lr)
                        } else {
                            (0, 0)
                        }
                    } else {
                        (0, 0)
                    }
                } else {
                    (0, 0)
                };
                let name = SvcId::from_u32(imm)
                    .map(|id| format!("{:?}", id))
                    .unwrap_or_else(|| format!("svc#0x{:02X}", imm));
                let name_id = common::trace::intern_svc_name(&name);
                common::trace::emit_raw(
                    common::trace::cat::SVC_ERRVAL,
                    &[
                        target,
                        tid.max(0) as u64,
                        core_id as u64,
                        name_id as u64,
                        pc,
                        lr,
                        args[0],
                        args[1],
                        args[2],
                        args[3],
                        dispatch_args[0],
                        dispatch_args[1],
                        dispatch_args[2],
                        dispatch_args[3],
                    ],
                );
            }
        }
    }

    // RUZU_TRACE_SVC_IMM=0x16,0x21,... — eprintln (level-independent) entry
    // args + return of every SVC whose number is in the list. Companion to
    // RUZU_TRACE_SVC_ERRVAL: trace CloseHandle (0x16) to correlate which tid
    // closed the session handle that a later SendSyncRequest fails on.
    {
        fn svc_imm_targets() -> &'static [u32] {
            use std::sync::OnceLock;
            static CACHED: OnceLock<Vec<u32>> = OnceLock::new();
            CACHED.get_or_init(|| {
                std::env::var("RUZU_TRACE_SVC_IMM")
                    .map(|s| {
                        s.split(',')
                            .filter_map(|p| {
                                let p = p.trim();
                                let p = p
                                    .strip_prefix("0x")
                                    .or_else(|| p.strip_prefix("0X"))
                                    .unwrap_or(p);
                                u32::from_str_radix(p, 16).ok()
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            })
        }
        if svc_imm_targets().contains(&imm) {
            eprintln!(
                "[SVC_IMM] tid={} core={} svc=0x{:02X} args_in=[0x{:X},0x{:X},0x{:X},0x{:X}] ret0=0x{:X}",
                tid, core_id, imm,
                args[0], args[1], args[2], args[3],
                dispatch_args[0]
            );
        }
    }

    // RUZU_TRACE_TID_SVC_RET=N (or comma list, or *) — log SVC RETURN values
    // (the dispatch_args after handler completion) for matching tid(s).
    // Pairs with RUZU_TRACE_TID_SVC (which logs ENTRY args). Diffing both
    // sides between ruzu and zuyu localizes the bug per
    if let Some((all, tids)) = trace_tid_svc_ret_targets() {
        let tid_ret = system
            .current_thread()
            .and_then(|t| t.lock().ok().map(|g| g.get_thread_id()))
            .unwrap_or(0);
        if *all || tids.contains(&tid_ret) {
            // Use raw hex for SVC id to match zuyu's symmetric trace format.
            log::info!(
                "[TID_SVC_RET] tid={} svc=0x{:02X} ret=[0x{:X}, 0x{:X}, 0x{:X}, 0x{:X}]",
                tid_ret,
                imm,
                dispatch_args[0],
                dispatch_args[1],
                dispatch_args[2],
                dispatch_args[3],
            );
        }
    }

    if let Some(kernel) = system.kernel() {
        if let Some(process_arc) = system.current_process_arc_opt() {
            let mut process = process_arc.lock().unwrap();
            kernel
                .current_physical_core()
                .load_svc_arguments(&mut process, &dispatch_args);
        }
    }
    *args = dispatch_args;

    // Memory snapshot hook (RUZU_DUMP_AT_SVC=N) — runs unconditionally.
    {
        let tid: i64 = system
            .current_thread()
            .map(|t| t.lock().unwrap().get_thread_id() as i64)
            .unwrap_or(-1);
        maybe_dump_process_memory(system, tid);
    }

    // Upstream reaches the equivalent behavior when the scheduler lock is
    // released after the SVC handler. In this cooperative port, drain a
    // pending current-thread termination once per SVC return path.
    drain_current_thread_termination(system);
}

/// SVC-to-SVC gap profile: `RUZU_PROFILE_GAP=1`.
///
/// For each tid, on SVC entry compute `now - last_svc_exit_ts[tid]` — this is
/// the time the thread spent executing guest code in the JIT between
/// supervisor calls. Aggregated per-tid as count/total/max plus log2 bucket
/// histogram. Dumped via SIGUSR2 alongside the other profiles.
///
/// On the very first SVC of a tid, no `last_svc_exit_ts` is recorded yet, so
/// the gap is skipped. On SVC exit, `last_svc_exit_ts[tid]` is updated to now.
static GAP_LAST_EXIT: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<u64, std::time::Instant>>,
> = std::sync::OnceLock::new();

static GAP_AGG: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u64, GapAggEntry>>> =
    std::sync::OnceLock::new();

#[derive(Default, Clone)]
struct GapAggEntry {
    count: u64,
    total_ns: u64,
    max_ns: u64,
    buckets: [u64; 32],
}

fn gap_profile_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_PROFILE_GAP").is_some())
}

fn trace_tid_svc_ret_targets() -> Option<&'static (bool, Vec<u64>)> {
    static TARGETS: std::sync::OnceLock<Option<(bool, Vec<u64>)>> = std::sync::OnceLock::new();
    TARGETS
        .get_or_init(|| {
            let raw = std::env::var_os("RUZU_TRACE_TID_SVC_RET")?;
            let raw = raw.to_string_lossy();
            let all = raw == "*" || raw == "all";
            let tids = if all {
                Vec::new()
            } else {
                raw.split(',')
                    .filter_map(|value| value.trim().parse().ok())
                    .collect()
            };
            Some((all, tids))
        })
        .as_ref()
}

fn gap_on_svc_entry(tid: u64) {
    if !gap_profile_enabled() {
        return;
    }
    let Some(last_map) = GAP_LAST_EXIT.get() else {
        return; // first ever SVC across the process — nothing to compare yet
    };
    let last_ts = {
        let guard = last_map.lock().unwrap();
        guard.get(&tid).copied()
    };
    let Some(last) = last_ts else { return };
    let delta = last.elapsed();
    let ns = delta.as_nanos() as u64;
    let agg_map = GAP_AGG.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut guard = agg_map.lock().unwrap();
    let entry = guard.entry(tid).or_default();
    entry.count += 1;
    entry.total_ns = entry.total_ns.saturating_add(ns);
    if ns > entry.max_ns {
        entry.max_ns = ns;
    }
    let bucket = if ns == 0 {
        0
    } else {
        ((63 - ns.leading_zeros()) as usize).min(entry.buckets.len() - 1)
    };
    entry.buckets[bucket] += 1;
}

fn gap_on_svc_exit(tid: u64) {
    if !gap_profile_enabled() {
        return;
    }
    let map = GAP_LAST_EXIT.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut guard = map.lock().unwrap();
    guard.insert(tid, std::time::Instant::now());
}

pub fn dump_gap_profile() {
    let Some(agg) = GAP_AGG.get() else {
        return;
    };
    let snap: Vec<(u64, GapAggEntry)> = {
        let guard = agg.lock().unwrap();
        guard.iter().map(|(k, v)| (*k, v.clone())).collect()
    };
    let mut snap = snap;
    snap.sort_by_key(|(_, e)| std::cmp::Reverse(e.total_ns));
    eprintln!("[GAP_PROFILE] per-tid SVC-to-SVC gap (time in JIT between SVCs), top by total:");
    for (tid, e) in snap.iter().take(10) {
        eprintln!(
            "[GAP_PROFILE] tid={:<4} count={:<7} total={:>9.2}ms avg={:>9.1}us max={:>9.1}us",
            tid,
            e.count,
            e.total_ns as f64 / 1e6,
            e.total_ns as f64 / e.count as f64 / 1e3,
            e.max_ns as f64 / 1e3,
        );
        let mut printed = false;
        for (k, &c) in e.buckets.iter().enumerate() {
            if c == 0 {
                continue;
            }
            if !printed {
                eprintln!("[GAP_PROFILE]   histogram (ns range -> count):");
                printed = true;
            }
            let lo = 1u64 << k;
            let hi = if k + 1 < 64 {
                1u64 << (k + 1)
            } else {
                u64::MAX
            };
            eprintln!(
                "[GAP_PROFILE]     [{:>9}, {:>9})  {}",
                format_ns_short(lo),
                format_ns_short(hi),
                c
            );
        }
    }
}

/// Wake-latency profile: `RUZU_PROFILE_WAKE=1`.
///
/// `KConditionVariable::signal_impl` calls `record_wake_emit(tid)` right after
/// `end_wait()` on each woken thread. Then at the *next* SVC entry on that
/// thread, `consume_wake_latency(tid)` returns the elapsed `Duration` from
/// signal-emit → woken thread reaching its next supervisor call. That delta
/// captures: scheduler dispatch latency + fiber resume + JIT block execution
/// time between wake-up and the post-wait SVC return.
static WAKE_PENDING: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<u64, std::time::Instant>>,
> = std::sync::OnceLock::new();

static WAKE_LATENCY: std::sync::OnceLock<std::sync::Mutex<WakeLatencyAgg>> =
    std::sync::OnceLock::new();

/// Per-tid wake-latency aggregator. Same shape as `WAKE_LATENCY` but keyed by
/// the tid of the woken thread. Enabled by `RUZU_PROFILE_WAKE_PER_TID=1`
/// (independent of the aggregate `RUZU_PROFILE_WAKE`). Helps identify which
/// thread suffers the slow-wake tail (e.g., the 26 ~1-sec wakes seen in
static WAKE_LATENCY_PER_TID: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<u64, WakeLatencyAgg>>,
> = std::sync::OnceLock::new();

#[derive(Default, Clone)]
struct WakeLatencyAgg {
    count: u64,
    total_ns: u64,
    max_ns: u64,
    /// log2 bucket histogram. bucket[k] counts samples with `ns >> k > 0` &&
    /// `ns >> (k+1) == 0`, i.e. ns in `[2^k, 2^(k+1))`. Buckets go up to 32 bits
    /// of nanoseconds = ~4 seconds.
    buckets: [u64; 32],
}

pub fn record_wake_emit(tid: u64) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var_os("RUZU_PROFILE_WAKE").is_some()) {
        return;
    }
    let map = WAKE_PENDING.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut guard = map.lock().unwrap();
    // Last-write-wins: if multiple signals stack up on one tid, keep the
    // earliest emit so the measured latency reflects the worst (longest) wait.
    guard.entry(tid).or_insert_with(std::time::Instant::now);
}

fn consume_wake_latency(tid: u64) {
    let Some(map) = WAKE_PENDING.get() else {
        return;
    };
    let emit = {
        let mut guard = map.lock().unwrap();
        guard.remove(&tid)
    };
    let Some(emit_ts) = emit else { return };
    let delta = emit_ts.elapsed();
    let ns = delta.as_nanos() as u64;
    let bucket_idx = if ns == 0 {
        0
    } else {
        ((63 - ns.leading_zeros()) as usize).min(31)
    };

    {
        let agg = WAKE_LATENCY.get_or_init(|| std::sync::Mutex::new(WakeLatencyAgg::default()));
        let mut g = agg.lock().unwrap();
        g.count += 1;
        g.total_ns = g.total_ns.saturating_add(ns);
        if ns > g.max_ns {
            g.max_ns = ns;
        }
        g.buckets[bucket_idx] += 1;
    }

    // Per-tid breakdown (lets us find which thread suffers the slow-wake tail).
    static PER_TID_ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *PER_TID_ENABLED.get_or_init(|| std::env::var_os("RUZU_PROFILE_WAKE_PER_TID").is_some()) {
        let agg = WAKE_LATENCY_PER_TID
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
        let mut g = agg.lock().unwrap();
        let entry = g.entry(tid).or_default();
        entry.count += 1;
        entry.total_ns = entry.total_ns.saturating_add(ns);
        if ns > entry.max_ns {
            entry.max_ns = ns;
        }
        entry.buckets[bucket_idx] += 1;
    }
}

pub fn dump_wake_latency() {
    let Some(agg) = WAKE_LATENCY.get() else {
        return;
    };
    let snap = {
        let g = agg.lock().unwrap();
        (g.count, g.total_ns, g.max_ns, g.buckets)
    };
    let (count, total_ns, max_ns, buckets) = snap;
    if count == 0 {
        eprintln!("[WAKE_LATENCY] no samples (RUZU_PROFILE_WAKE=1 enabled?)");
        return;
    }
    eprintln!(
        "[WAKE_LATENCY] count={} total={:.2}ms avg={:.1}us max={:.1}us",
        count,
        total_ns as f64 / 1e6,
        total_ns as f64 / count as f64 / 1e3,
        max_ns as f64 / 1e3,
    );
    eprintln!("[WAKE_LATENCY] histogram (ns range -> count):");
    for (k, &c) in buckets.iter().enumerate() {
        if c == 0 {
            continue;
        }
        let lo = 1u64 << k;
        let hi = if k + 1 < 64 {
            1u64 << (k + 1)
        } else {
            u64::MAX
        };
        let lo_str = format_ns_short(lo);
        let hi_str = format_ns_short(hi);
        eprintln!("[WAKE_LATENCY]   [{:>9}, {:>9})  {}", lo_str, hi_str, c);
    }

    // Per-tid breakdown if RUZU_PROFILE_WAKE_PER_TID=1.
    if let Some(per_tid_agg) = WAKE_LATENCY_PER_TID.get() {
        let entries: Vec<(u64, WakeLatencyAgg)> = {
            let g = per_tid_agg.lock().unwrap();
            g.iter().map(|(k, v)| (*k, v.clone())).collect()
        };
        if !entries.is_empty() {
            let mut entries = entries;
            entries.sort_by_key(|(_, e)| std::cmp::Reverse(e.total_ns));
            eprintln!("[WAKE_LATENCY_PER_TID] top tids by total wait time:");
            for (tid, e) in entries.iter().take(10) {
                eprintln!(
                    "[WAKE_LATENCY_PER_TID] tid={:<4} count={:<5} total={:>8.2}ms avg={:>7.1}us max={:>7.1}us",
                    tid,
                    e.count,
                    e.total_ns as f64 / 1e6,
                    e.total_ns as f64 / e.count as f64 / 1e3,
                    e.max_ns as f64 / 1e3,
                );
                let mut printed = false;
                for (k, &c) in e.buckets.iter().enumerate() {
                    if c == 0 {
                        continue;
                    }
                    if !printed {
                        eprintln!("[WAKE_LATENCY_PER_TID]   histogram:");
                        printed = true;
                    }
                    let lo = 1u64 << k;
                    let hi = if k + 1 < 64 {
                        1u64 << (k + 1)
                    } else {
                        u64::MAX
                    };
                    eprintln!(
                        "[WAKE_LATENCY_PER_TID]     [{:>9}, {:>9})  {}",
                        format_ns_short(lo),
                        format_ns_short(hi),
                        c
                    );
                }
            }
        }
    }
}

fn format_ns_short(ns: u64) -> String {
    if ns >= 1_000_000_000 {
        format!("{:.1}s", ns as f64 / 1e9)
    } else if ns >= 1_000_000 {
        format!("{:.1}ms", ns as f64 / 1e6)
    } else if ns >= 1_000 {
        format!("{:.1}us", ns as f64 / 1e3)
    } else {
        format!("{}ns", ns)
    }
}

/// Per-(tid, SVC) wall-clock profile: `RUZU_PROFILE_SVC_PER_TID=1`.
/// Keyed by `(tid, imm)`. Dumped via `dump_svc_per_tid_profile()`.
///
/// Distinct from `SVC_PROFILE` (which aggregates over all tids): this one
/// answers "which thread spends most time in which SVC type". Most useful for
/// identifying which thread is starved on which event — e.g. "tid=102 spends
/// X seconds in WaitSync" pinpoints the audio worker's main blocker.
static SVC_PER_TID_PROFILE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<(i64, u32), SvcProfileEntry>>,
> = std::sync::OnceLock::new();

fn record_svc_per_tid(tid: i64, imm: u32, elapsed: std::time::Duration) {
    let map =
        SVC_PER_TID_PROFILE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let ns = elapsed.as_nanos() as u64;
    let mut guard = map.lock().unwrap();
    let entry = guard.entry((tid, imm)).or_default();
    entry.count += 1;
    entry.total_ns = entry.total_ns.saturating_add(ns);
    if ns > entry.max_ns {
        entry.max_ns = ns;
    }
}

pub fn dump_svc_per_tid_profile() {
    let Some(map) = SVC_PER_TID_PROFILE.get() else {
        return;
    };
    let snap: Vec<((i64, u32), SvcProfileEntry)> = {
        let guard = map.lock().unwrap();
        guard.iter().map(|(k, v)| (*k, v.clone())).collect()
    };
    // Group by tid, then sort each tid's rows by total_ns desc.
    let mut by_tid: std::collections::HashMap<i64, Vec<(u32, SvcProfileEntry)>> =
        std::collections::HashMap::new();
    for ((tid, imm), entry) in snap {
        by_tid.entry(tid).or_default().push((imm, entry));
    }
    let mut tids: Vec<i64> = by_tid.keys().copied().collect();
    tids.sort_by_key(|t| {
        std::cmp::Reverse(
            by_tid
                .get(t)
                .map(|v| v.iter().map(|(_, e)| e.total_ns).sum::<u64>())
                .unwrap_or(0),
        )
    });
    eprintln!("[SVC_PER_TID] top tids (each sorted by total SVC time):");
    for tid in tids.iter().take(10) {
        let mut rows = by_tid.remove(tid).unwrap();
        rows.sort_by_key(|(_, e)| std::cmp::Reverse(e.total_ns));
        let total_per_tid: u64 = rows.iter().map(|(_, e)| e.total_ns).sum();
        eprintln!(
            "[SVC_PER_TID] tid={:<4} sum_total={:.2}ms",
            tid,
            total_per_tid as f64 / 1e6
        );
        for (imm, e) in rows.iter().take(6) {
            let name = SvcId::from_u32(*imm)
                .map(|id| format!("{:?}", id))
                .unwrap_or_else(|| format!("svc#0x{:02X}", imm));
            eprintln!(
                "[SVC_PER_TID]   imm=0x{:02X} {:>26}  count={:<6} total={:>9.2}ms  avg={:>9.1}us  max={:>9.1}us",
                imm,
                name,
                e.count,
                e.total_ns as f64 / 1e6,
                e.total_ns as f64 / e.count as f64 / 1e3,
                e.max_ns as f64 / 1e3,
            );
        }
    }
}

/// Per-SVC wall-clock profile. Populated when `RUZU_PROFILE_SVC=1`.
/// Keyed by SVC `imm` (0x00..0x7F). Dumped via `dump_svc_profile()`.
static SVC_PROFILE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<u32, SvcProfileEntry>>,
> = std::sync::OnceLock::new();

#[derive(Default, Clone)]
struct SvcProfileEntry {
    count: u64,
    total_ns: u64,
    max_ns: u64,
}

fn record_svc_profile(imm: u32, elapsed: std::time::Duration) {
    let map = SVC_PROFILE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let ns = elapsed.as_nanos() as u64;
    let mut guard = map.lock().unwrap();
    let entry = guard.entry(imm).or_default();
    entry.count += 1;
    entry.total_ns += ns;
    if ns > entry.max_ns {
        entry.max_ns = ns;
    }
}

pub fn dump_svc_profile() {
    let Some(map) = SVC_PROFILE.get() else {
        return;
    };
    let snap: Vec<(u32, SvcProfileEntry)> = {
        let guard = map.lock().unwrap();
        guard.iter().map(|(k, v)| (*k, v.clone())).collect()
    };
    let mut snap = snap;
    snap.sort_by_key(|(_, e)| std::cmp::Reverse(e.total_ns));
    eprintln!("[SVC_PROFILE] top SVCs by total time:");
    for (imm, e) in snap.iter().take(20) {
        let name = SvcId::from_u32(*imm)
            .map(|id| format!("{:?}", id))
            .unwrap_or_else(|| format!("svc#0x{:02X}", imm));
        eprintln!(
            "[SVC_PROFILE]   imm=0x{:02X} {:>26}  count={:<7}  total={:>9.2}ms  avg={:>9.1}us  max={:>9.1}us",
            imm,
            name,
            e.count,
            e.total_ns as f64 / 1e6,
            e.total_ns as f64 / e.count as f64 / 1e3,
            e.max_ns as f64 / 1e3,
        );
    }
}

const SVC_SUMMARY_MAX_TID: usize = 4096;
const SVC_SUMMARY_MAX_IMM: usize = 128;

/// Low-perturbation SVC census for side-by-side ruzu/zuyu investigations.
///
/// `RUZU_PROFILE_SVC_SUMMARY=1` records only in-memory atomic counters on the
/// hot path and dumps via `dump_svc_summary_profile()`. This is intentionally
/// timing enough to create false scheduler/audio conclusions.
struct SvcSummaryProfile {
    start: std::time::Instant,
    total: std::sync::atomic::AtomicU64,
    tid_total: Vec<std::sync::atomic::AtomicU64>,
    tid_first_ns: Vec<std::sync::atomic::AtomicU64>,
    tid_last_ns: Vec<std::sync::atomic::AtomicU64>,
    tid_last_core: Vec<std::sync::atomic::AtomicI32>,
    tid_last_imm: Vec<std::sync::atomic::AtomicU32>,
    tid_imm_counts: Vec<std::sync::atomic::AtomicU64>,
}

static SVC_SUMMARY_PROFILE: std::sync::OnceLock<SvcSummaryProfile> = std::sync::OnceLock::new();

fn svc_summary_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_PROFILE_SVC_SUMMARY").is_some())
}

fn svc_summary_profile() -> &'static SvcSummaryProfile {
    SVC_SUMMARY_PROFILE.get_or_init(|| SvcSummaryProfile {
        start: std::time::Instant::now(),
        total: std::sync::atomic::AtomicU64::new(0),
        tid_total: (0..SVC_SUMMARY_MAX_TID)
            .map(|_| std::sync::atomic::AtomicU64::new(0))
            .collect(),
        tid_first_ns: (0..SVC_SUMMARY_MAX_TID)
            .map(|_| std::sync::atomic::AtomicU64::new(0))
            .collect(),
        tid_last_ns: (0..SVC_SUMMARY_MAX_TID)
            .map(|_| std::sync::atomic::AtomicU64::new(0))
            .collect(),
        tid_last_core: (0..SVC_SUMMARY_MAX_TID)
            .map(|_| std::sync::atomic::AtomicI32::new(-1))
            .collect(),
        tid_last_imm: (0..SVC_SUMMARY_MAX_TID)
            .map(|_| std::sync::atomic::AtomicU32::new(u32::MAX))
            .collect(),
        tid_imm_counts: (0..SVC_SUMMARY_MAX_TID * SVC_SUMMARY_MAX_IMM)
            .map(|_| std::sync::atomic::AtomicU64::new(0))
            .collect(),
    })
}

fn record_svc_summary(tid: i64, core_id: i32, imm: u32) {
    if !svc_summary_enabled() {
        return;
    }
    if tid < 0 || tid as usize >= SVC_SUMMARY_MAX_TID || imm as usize >= SVC_SUMMARY_MAX_IMM {
        return;
    }

    use std::sync::atomic::Ordering;

    let profile = svc_summary_profile();
    let tid = tid as usize;
    let imm = imm as usize;
    let now_ns = profile.start.elapsed().as_nanos() as u64;

    profile.total.fetch_add(1, Ordering::Relaxed);
    profile.tid_total[tid].fetch_add(1, Ordering::Relaxed);
    let _ = profile.tid_first_ns[tid].compare_exchange(
        0,
        now_ns.max(1),
        Ordering::Relaxed,
        Ordering::Relaxed,
    );
    profile.tid_last_ns[tid].store(now_ns, Ordering::Relaxed);
    profile.tid_last_core[tid].store(core_id, Ordering::Relaxed);
    profile.tid_last_imm[tid].store(imm as u32, Ordering::Relaxed);
    profile.tid_imm_counts[tid * SVC_SUMMARY_MAX_IMM + imm].fetch_add(1, Ordering::Relaxed);
}

pub fn dump_svc_summary_profile() {
    let Some(profile) = SVC_SUMMARY_PROFILE.get() else {
        return;
    };

    use std::sync::atomic::Ordering;

    let total = profile.total.load(Ordering::Relaxed);
    if total == 0 {
        return;
    }

    eprintln!(
        "[SVC_SUMMARY] total={} elapsed_ms={:.2}",
        total,
        profile.start.elapsed().as_secs_f64() * 1e3
    );

    let mut rows = Vec::new();
    for tid in 0..SVC_SUMMARY_MAX_TID {
        let count = profile.tid_total[tid].load(Ordering::Relaxed);
        if count == 0 {
            continue;
        }
        let first_ns = profile.tid_first_ns[tid].load(Ordering::Relaxed);
        let last_ns = profile.tid_last_ns[tid].load(Ordering::Relaxed);
        rows.push((tid, count, first_ns, last_ns));
    }
    rows.sort_by_key(|(_, _, first_ns, _)| *first_ns);

    eprintln!("[SVC_SUMMARY] threads_seen={}", rows.len());
    for (tid, count, first_ns, last_ns) in rows {
        let last_core = profile.tid_last_core[tid].load(Ordering::Relaxed);
        let last_imm = profile.tid_last_imm[tid].load(Ordering::Relaxed);
        let mut top = Vec::new();
        for imm in 0..SVC_SUMMARY_MAX_IMM {
            let n = profile.tid_imm_counts[tid * SVC_SUMMARY_MAX_IMM + imm].load(Ordering::Relaxed);
            if n != 0 {
                top.push((imm as u32, n));
            }
        }
        top.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        let top_desc = top
            .iter()
            .take(8)
            .map(|(imm, n)| {
                let name = SvcId::from_u32(*imm)
                    .map(|id| format!("{:?}", id))
                    .unwrap_or_else(|| format!("svc#0x{:02X}", imm));
                format!("0x{:02X}:{}={}", imm, name, n)
            })
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!(
            "[SVC_SUMMARY] tid={:<4} count={:<8} first_ms={:>9.3} last_ms={:>9.3} last_core={:<2} last_imm=0x{:02X} top=[{}]",
            tid,
            count,
            first_ns as f64 / 1e6,
            last_ns as f64 / 1e6,
            last_core,
            last_imm,
            top_desc
        );
    }
}

const SVC_RING_DEFAULT_CAPACITY: usize = 131_072;

struct SvcRingSlot {
    seq: std::sync::atomic::AtomicU64,
    time_us: std::sync::atomic::AtomicU64,
    tid: std::sync::atomic::AtomicU64,
    priority: std::sync::atomic::AtomicI32,
    core_imm: std::sync::atomic::AtomicU64,
    pc: std::sync::atomic::AtomicU64,
    lr: std::sync::atomic::AtomicU64,
    sp: std::sync::atomic::AtomicU64,
    args: [std::sync::atomic::AtomicU64; 8],
}

struct SvcRingProfile {
    start: std::time::Instant,
    next: std::sync::atomic::AtomicU64,
    slots: Vec<SvcRingSlot>,
}

static SVC_RING_PROFILE: std::sync::OnceLock<SvcRingProfile> = std::sync::OnceLock::new();

fn svc_ring_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_PROFILE_SVC_RING").is_some())
}

fn svc_ring_capacity() -> usize {
    static CAPACITY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CAPACITY.get_or_init(|| {
        std::env::var("RUZU_PROFILE_SVC_RING_CAP")
            .ok()
            .and_then(|raw| raw.parse::<usize>().ok())
            .filter(|cap| *cap > 0)
            .unwrap_or(SVC_RING_DEFAULT_CAPACITY)
    })
}

fn svc_ring_profile() -> &'static SvcRingProfile {
    SVC_RING_PROFILE.get_or_init(|| {
        let slots = (0..svc_ring_capacity())
            .map(|_| SvcRingSlot {
                seq: std::sync::atomic::AtomicU64::new(0),
                time_us: std::sync::atomic::AtomicU64::new(0),
                tid: std::sync::atomic::AtomicU64::new(0),
                priority: std::sync::atomic::AtomicI32::new(0),
                core_imm: std::sync::atomic::AtomicU64::new(0),
                pc: std::sync::atomic::AtomicU64::new(0),
                lr: std::sync::atomic::AtomicU64::new(0),
                sp: std::sync::atomic::AtomicU64::new(0),
                args: std::array::from_fn(|_| std::sync::atomic::AtomicU64::new(0)),
            })
            .collect();
        SvcRingProfile {
            start: std::time::Instant::now(),
            next: std::sync::atomic::AtomicU64::new(0),
            slots,
        }
    })
}

pub fn record_svc_ring(
    tid: u64,
    priority: i32,
    core_id: i32,
    imm: u32,
    pc: u64,
    lr: u64,
    sp: u64,
    args: &[u64; 8],
) {
    if !svc_ring_enabled() {
        return;
    }

    use std::sync::atomic::Ordering;

    let profile = svc_ring_profile();
    let seq = profile.next.fetch_add(1, Ordering::Relaxed) + 1;
    let slot = &profile.slots[(seq as usize - 1) % profile.slots.len()];
    slot.time_us.store(
        profile.start.elapsed().as_micros() as u64,
        Ordering::Relaxed,
    );
    slot.tid.store(tid, Ordering::Relaxed);
    slot.priority.store(priority, Ordering::Relaxed);
    slot.core_imm.store(
        ((core_id as u32 as u64) << 32) | imm as u64,
        Ordering::Relaxed,
    );
    slot.pc.store(pc, Ordering::Relaxed);
    slot.lr.store(lr, Ordering::Relaxed);
    slot.sp.store(sp, Ordering::Relaxed);
    for (dst, src) in slot.args.iter().zip(args.iter()) {
        dst.store(*src, Ordering::Relaxed);
    }
    slot.seq.store(seq, Ordering::Release);
}

pub fn dump_svc_ring_profile() {
    let Some(profile) = SVC_RING_PROFILE.get() else {
        return;
    };

    use std::sync::atomic::Ordering;

    let next = profile.next.load(Ordering::Acquire);
    if next == 0 {
        return;
    }

    let cap = profile.slots.len() as u64;
    let first = next.saturating_sub(cap).saturating_add(1);
    eprintln!(
        "[SVC_RING] total={} dumping_seq={}..{} cap={}",
        next, first, next, cap
    );
    for seq in first..=next {
        let slot = &profile.slots[(seq as usize - 1) % profile.slots.len()];
        if slot.seq.load(Ordering::Acquire) != seq {
            continue;
        }
        let core_imm = slot.core_imm.load(Ordering::Relaxed);
        let core_id = (core_imm >> 32) as u32;
        let imm = core_imm as u32;
        let args = slot
            .args
            .iter()
            .map(|arg| arg.load(Ordering::Relaxed))
            .collect::<Vec<_>>();
        eprintln!(
            "[SVC_RING] seq={} t_us={} tid={} prio={} core={} imm=0x{:02X} pc=0x{:08X} lr=0x{:08X} sp=0x{:08X} args=[0x{:X},0x{:X},0x{:X},0x{:X},0x{:X},0x{:X},0x{:X},0x{:X}]",
            seq,
            slot.time_us.load(Ordering::Relaxed),
            slot.tid.load(Ordering::Relaxed),
            slot.priority.load(Ordering::Relaxed),
            core_id,
            imm,
            slot.pc.load(Ordering::Relaxed),
            slot.lr.load(Ordering::Relaxed),
            slot.sp.load(Ordering::Relaxed),
            args[0],
            args[1],
            args[2],
            args[3],
            args[4],
            args[5],
            args[6],
            args[7],
        );
    }
}

/// Per-tid SVC counter for the memory snapshot hook (RUZU_DUMP_AT_SVC=N).
static SVC_COUNTER_TID73: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Rate-limit unknown-SVC context dumps. Unknown SVCs usually mean guest state
/// is already corrupt, and rescheduling the same faulting thread can otherwise
/// flood stderr before the scheduler parks it.
static UNKNOWN_SVC_CONTEXT_COUNT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Global SVC counter on the main thread (tid=73). Always increments on every
/// SVC entry from tid=73 regardless of env-var state. Used to drive the
/// PC-window trace (`RUZU_TRACE_PC_WINDOW=start,end`) that samples guest PCs
/// between SVC #start exit and SVC #end entry.
static SVC_MAIN_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Access the shared PC_TRACE_ACTIVE flag that rdynarmic's block-lookup
/// trampoline checks. Exported by rdynarmic::jit::PC_TRACE_ACTIVE.
fn pc_trace_active_set(v: bool) {
    rdynarmic::jit::PC_TRACE_ACTIVE.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// Parse `RUZU_TRACE_PC_TID=<tid>` once. Defaults to 73 (main game thread).
/// Set to e.g. 102 to PC-trace the audio worker thread instead.
fn pc_trace_target_tid() -> i64 {
    use std::sync::OnceLock;
    static TID: OnceLock<i64> = OnceLock::new();
    *TID.get_or_init(|| {
        std::env::var("RUZU_TRACE_PC_TID")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(73)
    })
}

/// Parse `RUZU_TRACE_PC_WINDOW=<start>,<end>` once. `start` is the SVC index
/// whose EXIT activates tracing; `end` is the SVC index whose ENTRY
/// deactivates it. Both are 1-based on the target thread (see
/// `RUZU_TRACE_PC_TID`).
fn pc_trace_window() -> Option<(u64, u64)> {
    use std::sync::OnceLock;
    static WINDOW: OnceLock<Option<(u64, u64)>> = OnceLock::new();
    *WINDOW.get_or_init(|| {
        let raw = std::env::var("RUZU_TRACE_PC_WINDOW").ok()?;
        let (a, b) = raw.split_once(',')?;
        let start = a.trim().parse::<u64>().ok()?;
        let end = b.trim().parse::<u64>().ok()?;
        if end <= start {
            return None;
        }
        Some((start, end))
    })
}

/// Cached decision for `RUZU_SVC_TRACE_REGS=1` — dumps full r0..r14 + CPSR
/// (ARM32) / x0..x30 + PSTATE (ARM64) alongside each SVC_IN/OUT trace line.
/// Gated on `RUZU_SVC_TRACE_REGS` only (not `RUZU_SVC_TRACE`) because the
/// extra lines would break existing diff tooling that expects SVC_IN/OUT only.
fn svc_trace_full_regs_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_SVC_TRACE_REGS").is_some())
}

/// Dump all 16 ARM32 GPRs + CPSR (or 31 ARM64 GPRs + PSTATE) from the current
/// core's JIT. Env-gated on `RUZU_SVC_TRACE_REGS` — used to pinpoint
/// divergences invisible in the r0-r7 SVC args, especially in callee-saved
/// registers (r4-r11) the game carries across SVC boundaries.
///
/// `label` is "REGS_IN" or "REGS_OUT" to pair with the SVC_IN/OUT line.
fn dump_svc_full_regs(system: &System, imm: u32, tid: i64, label: &str) {
    use crate::arm::arm_interface::ThreadContext;
    let kernel = match system.kernel() {
        Some(k) => k,
        None => return,
    };
    let core_index = kernel.current_physical_core_index() as usize;
    let process_arc = match system.current_process_arc_opt() {
        Some(p) => p,
        None => return,
    };
    let process = process_arc.lock().unwrap();
    let jit = match process.get_arm_interface(core_index) {
        Some(j) => j,
        None => return,
    };
    let mut ctx = ThreadContext::default();
    jit.get_context(&mut ctx);
    // For ARM32, ctx.r[0..16] hold r0..r15; ctx.pstate holds CPSR.
    // For ARM64, ctx.r[0..29] hold x0..x28, plus fp/lr/sp/pc fields.
    // We print both shapes identically — callers read whichever slice matches
    // their guest bitness.
    eprintln!(
        "[{:>10.6}] {} imm={:#04x} tid={} r0={:#010x} r1={:#010x} r2={:#010x} r3={:#010x} r4={:#010x} r5={:#010x} r6={:#010x} r7={:#010x} r8={:#010x} r9={:#010x} r10={:#010x} r11={:#010x} r12={:#010x} sp={:#010x} lr={:#010x} pc={:#010x} cpsr={:#010x}",
        super::trace_format::elapsed_secs(),
        label,
        imm,
        tid,
        ctx.r[0] as u32, ctx.r[1] as u32, ctx.r[2] as u32, ctx.r[3] as u32,
        ctx.r[4] as u32, ctx.r[5] as u32, ctx.r[6] as u32, ctx.r[7] as u32,
        ctx.r[8] as u32, ctx.r[9] as u32, ctx.r[10] as u32, ctx.r[11] as u32,
        ctx.r[12] as u32, ctx.r[13] as u32, ctx.r[14] as u32, ctx.r[15] as u32,
        ctx.pstate,
    );
    // Optional: dump 16 words of stack at sp to surface saved-LR chain.
    // Gated by RUZU_SVC_TRACE_STACK to keep RUZU_SVC_TRACE_REGS output clean
    // when stack contents aren't needed.
    if common::env_flag!("RUZU_SVC_TRACE_STACK") {
        let sp = ctx.r[13] as u64;
        let mem = process.process_memory.read().unwrap();
        let mut hex = String::with_capacity(64 * 9);
        for i in 0..64u64 {
            let addr = sp + i * 4;
            let w = if mem.is_valid_range(addr, 4) {
                mem.read_32(addr)
            } else {
                0xDEADBEEF
            };
            use std::fmt::Write;
            let _ = write!(hex, "{:08x} ", w);
        }
        eprintln!(
            "[{:>10.6}] STACK {} sp={:#010x} {}",
            super::trace_format::elapsed_secs(),
            label,
            sp as u32,
            hex.trim()
        );
    }
}

fn maybe_dump_process_memory(system: &System, tid: i64) {
    if tid != 73 {
        return;
    }
    static CONFIG: std::sync::OnceLock<Option<(u64, String)>> = std::sync::OnceLock::new();
    let Some((target, path)) = CONFIG
        .get_or_init(|| {
            let target = std::env::var("RUZU_DUMP_AT_SVC").ok()?.parse().ok()?;
            let path = std::env::var("RUZU_DUMP_PATH")
                .unwrap_or_else(|_| format!("/tmp/ruzu-mem-svc{}.bin", target));
            Some((target, path))
        })
        .as_ref()
    else {
        return;
    };
    let count = SVC_COUNTER_TID73.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    if count != *target {
        return;
    }

    eprintln!(
        "[DUMP] writing process memory snapshot at SVC #{} to {}",
        target, path
    );

    let memory = match system.memory_shared() {
        Some(m) => m,
        None => {
            eprintln!("[DUMP] no shared memory; skipping");
            return;
        }
    };
    let process_arc = match system.current_process_arc_opt() {
        Some(p) => p,
        None => {
            eprintln!("[DUMP] no current process; skipping");
            return;
        }
    };

    use std::io::Write;
    let mut file = match std::fs::File::create(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[DUMP] failed to create {}: {}", path, e);
            return;
        }
    };

    // Walk every memory block and dump (addr, size, state, perm, bytes).
    let process = process_arc.lock().unwrap();
    let pt = &process.page_table;
    let mut total_bytes = 0u64;
    let mut block_count = 0u32;
    for block in pt.iter_blocks() {
        let info: super::k_memory_block::KMemoryInfo = block.get_memory_info();
        // Skip free regions (no point dumping unmapped space).
        if info.m_state == super::k_memory_block::KMemoryState::FREE {
            continue;
        }
        let addr = info.m_address;
        let size = info.m_size;
        if size == 0 {
            continue;
        }
        // Header: addr (u64), size (u64), state (u32), perm (u32) = 24 bytes
        let mut hdr = [0u8; 24];
        hdr[0..8].copy_from_slice(&(addr as u64).to_le_bytes());
        hdr[8..16].copy_from_slice(&(size as u64).to_le_bytes());
        hdr[16..20].copy_from_slice(&info.m_state.bits().to_le_bytes());
        hdr[20..24].copy_from_slice(&(info.m_permission.bits() as u32).to_le_bytes());
        if file.write_all(&hdr).is_err() {
            break;
        }
        // Read the block contents in 4KB chunks.
        let mem = memory.lock().unwrap();
        let mut buf = vec![0u8; 4096];
        let mut off = 0usize;
        while off < size {
            let chunk = std::cmp::min(4096, size - off);
            buf.resize(chunk, 0);
            let _ = mem.read_block((addr + off) as u64, &mut buf);
            if file.write_all(&buf).is_err() {
                break;
            }
            off += chunk;
        }
        drop(mem);
        total_bytes += size as u64;
        block_count += 1;
    }
    eprintln!(
        "[DUMP] complete: {} blocks, {} bytes",
        block_count, total_bytes
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::kernel::k_process::{KProcess, ProcessLock};
    use crate::hle::kernel::k_scheduler::KScheduler;
    use crate::hle::kernel::k_thread::{KThread, KThreadLock, ThreadState};
    use crate::hle::kernel::k_worker_task_manager::KWorkerTaskManager;
    use crate::hle::result::RESULT_SUCCESS;
    use std::sync::{Arc, Mutex};

    fn test_system() -> System {
        let mut system = System::new_for_test();

        let mut process = KProcess::new();
        process.capabilities.core_mask = 0xF;
        process.capabilities.priority_mask = u64::MAX;
        process.flags = 0;
        process.allocate_code_memory(0x200000, 0x20000);
        process.initialize_handle_table();
        process.initialize_thread_local_region_base(0x240000);

        let process = Arc::new(ProcessLock::from_value(process));
        let current_thread = Arc::new(KThreadLock::new(KThread::new()));
        let scheduler = Arc::new(Mutex::new(KScheduler::new(0)));
        {
            let mut thread = current_thread.lock().unwrap();
            thread.initialize_main_thread(0x200000, 0x250000, 0, 0x23f000, &process, 1, 1, false);
            thread.set_priority(44);
            thread.set_base_priority(44);
        }
        process
            .lock()
            .unwrap()
            .register_thread_object(current_thread.clone());
        process.lock().unwrap().push_back_to_priority_queue(1);
        process.lock().unwrap().state = crate::hle::kernel::k_process::ProcessState::Running;
        scheduler.lock().unwrap().initialize(1, 0, 0);

        let shared_memory = process.lock().unwrap().get_shared_memory();
        system.set_current_process_arc(process);
        system.set_scheduler_arc(scheduler);
        system.set_shared_process_memory(shared_memory);
        system.set_runtime_program_id(1);
        system.set_runtime_64bit(false);
        system
    }

    #[test]
    fn test_svc_id_roundtrip() {
        assert_eq!(SvcId::from_u32(0x01), Some(SvcId::SetHeapSize));
        assert_eq!(SvcId::from_u32(0x07), Some(SvcId::ExitProcess));
        assert_eq!(SvcId::from_u32(0x7F), Some(SvcId::CallSecureMonitor));
        assert_eq!(SvcId::from_u32(0x90), Some(SvcId::MapInsecureMemory));
        assert_eq!(SvcId::from_u32(0x91), Some(SvcId::UnmapInsecureMemory));
        // Gaps in the enum
        assert_eq!(SvcId::from_u32(0x38), None);
        assert_eq!(SvcId::from_u32(0x3B), None);
        assert_eq!(SvcId::from_u32(0x5B), None);
        assert_eq!(SvcId::from_u32(0x6E), None);
        // Unknown
        assert_eq!(SvcId::from_u32(0x00), None);
        assert_eq!(SvcId::from_u32(0xFF), None);
    }

    #[test]
    fn code_memory_dispatches_use_generated_argument_layouts() {
        let system = test_system();

        let mut args64 = [0; 8];
        args64[1] = 1;
        args64[2] = 0x1000;
        call64(&system, SvcId::CreateCodeMemory as u32, &mut args64);
        assert_eq!(
            args64[0] as u32,
            crate::hle::kernel::svc::svc_results::RESULT_INVALID_ADDRESS.get_inner_value()
        );
        assert_eq!(args64[1], 0);

        let mut args32 = [0; 8];
        set_arg32(&mut args32, 1, 1);
        set_arg32(&mut args32, 2, 0x1000);
        call32(&system, SvcId::CreateCodeMemory as u32, &mut args32);
        assert_eq!(
            get_arg32(&args32, 0),
            crate::hle::kernel::svc::svc_results::RESULT_INVALID_ADDRESS.get_inner_value()
        );
        assert_eq!(get_arg32(&args32, 1), 0);
    }

    #[test]
    fn test_arg_helpers() {
        let mut args: SvcArgs = [0; 8];
        set_arg32(&mut args, 0, 0xDEADBEEF);
        assert_eq!(get_arg32(&args, 0), 0xDEADBEEF);
        assert_eq!(get_arg64(&args, 0), 0xDEADBEEF);

        set_arg64(&mut args, 1, 0x1234_5678_9ABC_DEF0);
        assert_eq!(get_arg64(&args, 1), 0x1234_5678_9ABC_DEF0);
        assert_eq!(get_arg32(&args, 1), 0x9ABC_DEF0);
    }

    #[test]
    fn test_gather_scatter_64() {
        let mut args: SvcArgs = [0; 8];
        args[0] = 0xDEADBEEF;
        args[3] = 0x12345678;
        assert_eq!(gather64(&args, 0, 3), 0x12345678_DEADBEEF);

        scatter64(&mut args, 1, 2, 0xAAAABBBB_CCCCDDDD);
        assert_eq!(args[1], 0xCCCCDDDD);
        assert_eq!(args[2], 0xAAAABBBB);
    }

    #[test]
    fn call64_routes_address_arbiter_svcs() {
        let system = test_system();

        for svc in [SvcId::WaitForAddress, SvcId::SignalToAddress] {
            let mut args: SvcArgs = [0; 8];
            args[0] = 0x20_0000;
            args[1] = u32::MAX as u64;
            call64(&system, svc as u32, &mut args);
            assert_eq!(
                args[0],
                crate::hle::kernel::svc::svc_results::RESULT_INVALID_ENUM_VALUE.get_inner_value()
                    as u64,
                "{svc:?} must be dispatched instead of returning stub success"
            );
        }
    }

    #[test]
    fn call64_routes_unmap_physical_memory() {
        let system = test_system();
        let mut args: SvcArgs = [0; 8];
        args[0] = 1;
        args[1] = 0x1000;

        call64(&system, SvcId::UnmapPhysicalMemory as u32, &mut args);

        assert_eq!(
            args[0],
            crate::hle::kernel::svc::svc_results::RESULT_INVALID_ADDRESS.get_inner_value() as u64,
            "UnmapPhysicalMemory must reach its AArch64 handler instead of returning stub success"
        );
    }

    #[test]
    fn call64_routes_get_system_info() {
        use crate::hle::kernel::k_memory_block::PAGE_SIZE;
        use crate::hle::kernel::k_memory_manager::Pool;

        let mut system = test_system();
        system
            .kernel_mut()
            .unwrap()
            .memory_manager_mut()
            .initialize_pool(Pool::Application, 0x1_0000_0000, 16 * PAGE_SIZE);

        let mut args: SvcArgs = [0; 8];
        args[1] =
            crate::hle::kernel::svc::svc_types::SystemInfoType::TotalPhysicalMemorySize as u64;
        args[3] = Pool::Application as u64;
        call64(&system, SvcId::GetSystemInfo as u32, &mut args);

        assert_eq!(args[0], RESULT_SUCCESS.get_inner_value() as u64);
        assert_eq!(args[1], (16 * PAGE_SIZE) as u64);

        args = [0; 8];
        args[1] = u32::MAX as u64;
        call64(&system, SvcId::GetSystemInfo as u32, &mut args);
        assert_eq!(
            args[0],
            crate::hle::kernel::svc::svc_results::RESULT_INVALID_ENUM_VALUE.get_inner_value()
                as u64
        );
    }

    #[test]
    fn call64_routes_event_create_signal_and_clear() {
        let system = test_system();
        let mut args: SvcArgs = [0; 8];

        call64(&system, SvcId::CreateEvent as u32, &mut args);
        assert_eq!(args[0], RESULT_SUCCESS.get_inner_value() as u64);
        let write_handle = args[1] as u32;
        let read_handle = args[2] as u32;
        assert_ne!(write_handle, 0);
        assert_ne!(read_handle, 0);

        args = [0; 8];
        args[0] = write_handle as u64;
        call64(&system, SvcId::SignalEvent as u32, &mut args);
        assert_eq!(args[0], RESULT_SUCCESS.get_inner_value() as u64);

        {
            let process_arc = system.current_process_arc();
            let process = process_arc.lock().unwrap();
            let object_id = process.handle_table.get_object(read_handle).unwrap();
            let event = process.get_readable_event_by_object_id(object_id).unwrap();
            assert!(event.lock().unwrap().is_signaled());
        }

        args = [0; 8];
        args[0] = read_handle as u64;
        call64(&system, SvcId::ClearEvent as u32, &mut args);
        assert_eq!(args[0], RESULT_SUCCESS.get_inner_value() as u64);

        let process_arc = system.current_process_arc();
        let process = process_arc.lock().unwrap();
        let object_id = process.handle_table.get_object(read_handle).unwrap();
        let event = process.get_readable_event_by_object_id(object_id).unwrap();
        assert!(!event.lock().unwrap().is_signaled());
    }

    #[test]
    fn call32_exit_process_notifies_the_frontend() {
        let mut system = test_system();
        let exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback_exited = Arc::clone(&exited);
        system.register_exit_callback(Box::new(move || {
            callback_exited.store(true, std::sync::atomic::Ordering::SeqCst);
        }));

        let mut args: SvcArgs = [0; 8];
        call32(&system, SvcId::ExitProcess as u32, &mut args);

        assert!(exited.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn call_drains_current_thread_termination_on_svc_return() {
        let system = test_system();
        let current_thread = system.current_thread().expect("main thread must exist");
        current_thread.lock().unwrap().request_terminate();

        let mut args: SvcArgs = [0; 8];
        call(&system, SvcId::GetSystemTick as u32, true, &mut args);
        KWorkerTaskManager::wait_for_global_idle();

        let thread = current_thread.lock().unwrap();
        assert_eq!(thread.get_state(), ThreadState::TERMINATED);
        assert!(thread.is_signaled());
    }
}
