// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/arm/debug.h and debug.cpp
//! Debug utilities (thread naming, backtrace, module enumeration).

use crate::arm::arm_interface::ThreadContext;
use crate::hardware_properties;
use crate::hle::kernel::k_memory_block::{KMemoryPermission, KMemoryState};
use crate::hle::kernel::k_process::KProcess;
use crate::hle::kernel::k_thread::KThread;

/// Module map: base address -> module name
pub use crate::loader::loader::Modules;

/// Backtrace entry, matching upstream `Core::BacktraceEntry`.
#[derive(Debug, Clone, Default)]
pub struct BacktraceEntry {
    pub module: String,
    pub address: u64,
    pub original_address: u64,
    pub offset: u64,
    pub name: String,
}

/// Segment base addresses for 32-bit and 64-bit modes.
/// Upstream: `constexpr std::array<u64, 2> SegmentBases{0x60000000ULL, 0x7100000000ULL};`
const SEGMENT_BASES: [u64; 2] = [0x6000_0000, 0x71_0000_0000];

fn can_walk_frame_record<F>(fp: u64, record_size: u64, is_valid_range: F) -> bool
where
    F: FnOnce(u64, u64) -> bool,
{
    fp != 0 && (fp % 4 == 0) && is_valid_range(fp, record_size)
}

/// Get the name of a thread from its nnsdk thread type structure.
///
/// Corresponds to upstream `Core::GetThreadName` (debug.cpp).
/// Reads from TLS to find the nnsdk thread type and extract its name.
pub fn get_thread_name(thread: &KThread) -> Option<String> {
    let parent = thread.parent.as_ref()?.upgrade()?;
    let process = parent.lock().unwrap();
    let is_64bit = process.is_64bit();
    let tls_addr = thread.tls_address.get();
    if tls_addr == 0 {
        return None;
    }
    let memory = process.get_memory()?;
    let mem = memory.lock().unwrap();

    // Upstream: reads thread type pointer from TLS+0x1F8 (64-bit) or TLS+0x1FC (32-bit)
    // then reads version and name pointer from the thread type struct.
    if is_64bit {
        let thread_type_addr = mem.read_64(tls_addr + 0x1F8);
        let argument_thread_type = thread.get_argument() as u64;
        if argument_thread_type != 0 && thread_type_addr != argument_thread_type {
            return None;
        }
        if thread_type_addr == 0 {
            return None;
        }
        let version = mem.read_16(thread_type_addr + 0x46);
        let name_pointer = if version == 1 {
            mem.read_64(thread_type_addr + 0x1A0)
        } else {
            mem.read_64(thread_type_addr + 0x1A8)
        };
        if name_pointer == 0 {
            return None;
        }
        // Read C string from name_pointer, max 256 bytes.
        let mut name = Vec::new();
        for i in 0..256u64 {
            let b = mem.read_8(name_pointer + i);
            if b == 0 {
                break;
            }
            name.push(b);
        }
        Some(String::from_utf8_lossy(&name).to_string())
    } else {
        let thread_type_addr = mem.read_32(tls_addr + 0x1FC) as u64;
        let argument_thread_type = thread.get_argument() as u64;
        if argument_thread_type != 0 && thread_type_addr != argument_thread_type {
            return None;
        }
        if thread_type_addr == 0 {
            return None;
        }
        let version = mem.read_16(thread_type_addr + 0x26);
        let name_pointer = if version == 1 {
            mem.read_32(thread_type_addr + 0xE4) as u64
        } else {
            mem.read_32(thread_type_addr + 0xE8) as u64
        };
        if name_pointer == 0 {
            return None;
        }
        let mut name = Vec::new();
        for i in 0..256u64 {
            let b = mem.read_8(name_pointer + i);
            if b == 0 {
                break;
            }
            name.push(b);
        }
        Some(String::from_utf8_lossy(&name).to_string())
    }
}

/// Get the wait reason string for a thread.
/// Corresponds to upstream `Core::GetThreadWaitReason`.
pub fn get_thread_wait_reason(thread: &KThread) -> &'static str {
    use crate::hle::kernel::k_thread::ThreadWaitReasonForDebugging;
    match thread.wait_reason_for_debugging {
        ThreadWaitReasonForDebugging::Sleep => "Sleep",
        ThreadWaitReasonForDebugging::Ipc => "IPC",
        ThreadWaitReasonForDebugging::Synchronization => "Synchronization",
        ThreadWaitReasonForDebugging::ConditionVar => "ConditionVar",
        ThreadWaitReasonForDebugging::Arbitration => "Arbitration",
        ThreadWaitReasonForDebugging::Suspended => "Suspended",
        _ => "Unknown",
    }
}

/// Get the state string for a thread.
/// Corresponds to upstream `Core::GetThreadState`.
pub fn get_thread_state(thread: &KThread) -> String {
    use crate::hle::kernel::k_thread::ThreadState;
    let state = thread.get_state();
    match state {
        ThreadState::INITIALIZED => "Initialized".to_string(),
        ThreadState::WAITING => {
            format!("Waiting ({})", get_thread_wait_reason(thread))
        }
        ThreadState::RUNNABLE => "Runnable".to_string(),
        ThreadState::TERMINATED => "Terminated".to_string(),
        _ => "Unknown".to_string(),
    }
}

/// Find loaded modules in a process's address space.
/// Corresponds to upstream `Core::FindModules` (debug.cpp).
///
/// Walks the page table looking for executable Code sections, reads MOD0
/// headers to extract module path names.
pub fn find_modules(process: &KProcess) -> Modules {
    const PATH_LENGTH_MAX: usize = 0x200;
    const MODULE_PATH_SIZE: usize = 8 + PATH_LENGTH_MAX;

    let mut modules = Modules::new();
    let mut current_address = 0usize;

    loop {
        let memory_info = process
            .page_table
            .query_info(current_address)
            .expect("process page-table query must succeed");
        let base_address = memory_info.get_address();
        let size = memory_info.get_size();

        if memory_info.get_permission() == KMemoryPermission::USER_READ_EXECUTE
            && matches!(
                memory_info.get_state(),
                KMemoryState::CODE | KMemoryState::ALIAS_CODE
            )
        {
            let mut module_path = [0u8; MODULE_PATH_SIZE];
            let path_address = base_address.wrapping_add(size) as u64;
            if read_process_memory(process, path_address, &mut module_path) {
                let zero = u32::from_le_bytes(module_path[0..4].try_into().unwrap());
                let path_length = i32::from_le_bytes(module_path[4..8].try_into().unwrap());
                if zero == 0 && path_length > 0 {
                    let path = &mut module_path[8..];
                    path[PATH_LENGTH_MAX - 1] = 0;
                    let path_end = usize::min(PATH_LENGTH_MAX, path_length as usize);
                    let mut path_start = 0;
                    for (index, byte) in path[..path_end].iter().copied().enumerate() {
                        if byte == 0 {
                            break;
                        }
                        if byte == b'/' || byte == b'\\' {
                            path_start = index + 1;
                        }
                    }
                    modules.insert(
                        base_address as u64,
                        String::from_utf8_lossy(&path[path_start..path_end]).into_owned(),
                    );
                }
            }
        }

        let next_address = base_address.wrapping_add(size);
        if next_address <= current_address {
            break;
        }
        current_address = next_address;
    }

    modules
}

/// Get the end address of a module starting at `base`.
/// Corresponds to upstream `Core::GetModuleEnd`.
pub fn get_module_end(process: &KProcess, base: u64) -> u64 {
    let mut current_address = base as usize;

    let text = process
        .page_table
        .query_info(current_address)
        .expect("module text page-table query must succeed");
    current_address = text.get_address().wrapping_add(text.get_size());
    if text.get_state() != KMemoryState::CODE
        || text.get_permission() != KMemoryPermission::USER_READ_EXECUTE
    {
        return current_address.wrapping_sub(1) as u64;
    }

    let rodata = process
        .page_table
        .query_info(current_address)
        .expect("module rodata page-table query must succeed");
    current_address = rodata.get_address().wrapping_add(rodata.get_size());
    if rodata.get_state() != KMemoryState::CODE
        || rodata.get_permission() != KMemoryPermission::USER_READ
    {
        return current_address.wrapping_sub(1) as u64;
    }

    let data = process
        .page_table
        .query_info(current_address)
        .expect("module data page-table query must succeed");
    data.get_address()
        .wrapping_add(data.get_size())
        .wrapping_sub(1) as u64
}

/// Find the entrypoint of the main module.
/// Corresponds to upstream `Core::FindMainModuleEntrypoint`.
pub fn find_main_module_entrypoint(process: &KProcess) -> u64 {
    let modules = find_modules(process);
    if modules.len() >= 2 {
        // Second module is main (first is rtld).
        *modules.keys().nth(1).unwrap()
    } else if modules.len() == 1 {
        *modules.keys().next().unwrap()
    } else {
        // Upstream: falls back to code region start.
        process.page_table.get_code_region_start().get()
    }
}

fn read_process_memory(process: &KProcess, address: u64, output: &mut [u8]) -> bool {
    if let Some(memory) = process.get_memory() {
        return memory.lock().unwrap().read_block(address, output);
    }

    let memory = process.get_shared_memory();
    let memory = memory.read().unwrap();
    if !memory.is_valid_range(address, output.len()) {
        return false;
    }
    output.copy_from_slice(&memory.read_bytes(address, output.len()));
    true
}

/// Invalidate instruction cache range across all CPU cores.
/// Corresponds to upstream `Core::InvalidateInstructionCacheRange`.
pub fn invalidate_instruction_cache_range(
    process: &mut crate::hle::kernel::k_process::KProcess,
    address: u64,
    size: u64,
) {
    for core_index in 0..hardware_properties::NUM_CPU_CORES as usize {
        if let Some(interface) = process.get_arm_interface_mut(core_index) {
            interface.invalidate_cache_range(address, size as usize);
        }
    }
}

/// Get a backtrace from a thread context.
/// Corresponds to upstream `Core::GetBacktraceFromContext` (debug.cpp).
pub fn get_backtrace_from_context(
    process: &crate::hle::kernel::k_process::KProcess,
    ctx: &ThreadContext,
) -> Vec<BacktraceEntry> {
    let is_64bit = process.is_64bit();
    let memory = process.get_memory();
    let mem = memory.as_ref().map(|memory| memory.lock().unwrap());

    let mut entries = Vec::new();
    let pc = ctx.pc;
    let mut fp = ctx.fp;
    let mut lr = ctx.lr;

    entries.push(BacktraceEntry {
        module: String::new(),
        address: pc,
        original_address: pc,
        offset: 0,
        name: String::new(),
    });

    // Walk frame pointer chain.
    // AArch64: fp+0 = prev fp, fp+8 = return address
    // AArch32: fp+0 = prev fp, fp+4 = return address
    for _ in 0..256 {
        entries.push(BacktraceEntry {
            module: String::new(),
            address: lr,
            original_address: lr,
            offset: 0,
            name: String::new(),
        });

        let Some(mem) = mem.as_ref() else {
            break;
        };
        if is_64bit {
            if !can_walk_frame_record(fp, 16, |base, size| {
                base.checked_add(size).is_some() && mem.is_valid_virtual_address_range(base, size)
            }) {
                break;
            }
            lr = mem.read_64(fp + 8);
            fp = mem.read_64(fp);
        } else {
            if !can_walk_frame_record(fp, 8, |base, size| {
                base.checked_add(size).is_some() && mem.is_valid_virtual_address_range(base, size)
            }) {
                break;
            }
            lr = mem.read_32(fp + 4) as u64;
            fp = mem.read_32(fp) as u64;
        }
    }

    // Module discovery reads the same process memory; release its non-reentrant lock.
    drop(mem);
    symbolicate_backtrace(process, &mut entries, is_64bit);
    entries
}

#[cfg(test)]
mod tests {
    use super::{can_walk_frame_record, find_main_module_entrypoint, find_modules, get_module_end};
    use crate::hle::kernel::k_memory_block::{
        KMemoryAttribute, KMemoryBlockDisableMergeAttribute, KMemoryPermission, KMemoryState,
    };
    use crate::hle::kernel::k_process::KProcess;
    use crate::hle::kernel::k_typed_address::KProcessAddress;

    #[test]
    fn can_walk_frame_record_rejects_zero_and_misaligned_pointers() {
        assert!(!can_walk_frame_record(0, 8, |_, _| true));
        assert!(!can_walk_frame_record(3, 8, |_, _| true));
        assert!(can_walk_frame_record(4, 8, |_, _| true));
    }

    #[test]
    fn can_walk_frame_record_requires_valid_range() {
        assert!(!can_walk_frame_record(0x1000, 8, |_, _| false));
        assert!(can_walk_frame_record(0x1000, 8, |base, size| base
            == 0x1000
            && size == 8));
    }

    #[test]
    fn backtrace_reads_live_process_pages_in_both_execution_modes() {
        use crate::core::SystemRef;
        use crate::device_memory::{dram_memory_map, DeviceMemory};
        use crate::memory::memory::Memory;
        use common::host_memory::MemoryPermission;
        use common::page_table::PageTable;
        use std::sync::{Arc, Mutex};

        // Keep the backing allocations stable and alive until Memory is dropped.
        let device = Box::new(DeviceMemory::with_size(0x20_000));
        let mut pages = Box::new(PageTable::new());
        pages.resize(32, 12);
        let mut memory = unsafe { Memory::new(SystemRef::null(), &*device, &device.buffer) };
        memory.set_current_page_table(&mut *pages, false);
        memory.map_memory_region(
            &mut pages,
            0x4000,
            0x2000,
            dram_memory_map::BASE + 0x2000,
            MemoryPermission::READ_WRITE,
            false,
        );
        let mut process = KProcess::new();
        process
            .page_table
            .configure_address_space(KProcessAddress::new(0), 0x1_0000_0000, 32);
        process.memory = Some(Arc::new(Mutex::new(memory)));

        for is_64bit in [false, true] {
            process.flags = u32::from(is_64bit);
            let memory = process.get_memory().unwrap();
            let mem = memory.lock().unwrap();
            // The first frame straddles two live pages; the legacy shadow stays zero.
            if is_64bit {
                mem.write_64(0x4ffc, 0x5040);
                mem.write_64(0x5004, 0x1234_5678_9abc_def0);
                mem.write_64(0x5040, 0);
                mem.write_64(0x5048, 0x2345_6789_abcd_ef00);
            } else {
                mem.write_32(0x4ffc, 0x5040);
                mem.write_32(0x5000, 0x9abc_def0);
                mem.write_32(0x5040, 0);
                mem.write_32(0x5044, 0xabcd_ef00);
            }
            drop(mem);
            assert_eq!(
                process.get_shared_memory().read().unwrap().read_64(0x4ffc),
                0
            );
            let mut context = crate::arm::arm_interface::ThreadContext {
                pc: 0x1000,
                lr: 0x2000,
                fp: 0x4ffc,
                ..Default::default()
            };
            let entries = super::get_backtrace_from_context(&process, &context);
            let expected = if is_64bit {
                [0x1000, 0x2000, 0x1234_5678_9abc_def0, 0x2345_6789_abcd_ef00]
            } else {
                [0x1000, 0x2000, 0x9abc_def0, 0xabcd_ef00]
            };
            assert_eq!(
                entries
                    .iter()
                    .map(|entry| entry.original_address)
                    .collect::<Vec<_>>(),
                expected
            );

            for invalid_fp in [0, 3, 0x6000, u64::MAX - 3] {
                context.fp = invalid_fp;
                assert_eq!(
                    super::get_backtrace_from_context(&process, &context).len(),
                    2
                );
            }
        }

        let process = Arc::new(crate::hle::kernel::k_process::ProcessLock::new(process));
        let mut thread = crate::hle::kernel::k_thread::KThread::new();
        thread.parent = Some(Arc::downgrade(&process));
        thread.tls_address = KProcessAddress::new(0x4000);
        thread.argument = 0x4400;
        for is_64bit in [false, true] {
            for version in [1, 2] {
                let mut owner = process.lock().unwrap();
                owner.flags = u32::from(is_64bit);
                let memory = owner.get_memory().unwrap();
                let mem = memory.lock().unwrap();
                assert!(mem.write_block(0x4800, b"worker\0"));
                if is_64bit {
                    mem.write_64(0x41f8, 0x4400);
                    mem.write_16(0x4446, version);
                    mem.write_64(0x4400 + if version == 1 { 0x1a0 } else { 0x1a8 }, 0x4800);
                } else {
                    mem.write_32(0x41fc, 0x4400);
                    mem.write_16(0x4426, version);
                    mem.write_32(0x4400 + if version == 1 { 0xe4 } else { 0xe8 }, 0x4800);
                }
                drop(mem);
                drop(owner);
                assert_eq!(super::get_thread_name(&thread).as_deref(), Some("worker"));
                thread.argument = 0x4500;
                assert_eq!(super::get_thread_name(&thread), None);
                thread.argument = 0x4400;
            }
        }
    }

    fn module_process() -> KProcess {
        const BASE: usize = 0x20_0000;
        let mut process = KProcess::new();
        process.allocate_code_memory(BASE as u64, 0x10_000);

        let manager = process
            .page_table
            .get_base_mut()
            .get_memory_block_manager_mut();
        manager.update(
            BASE,
            1,
            KMemoryState::CODE,
            KMemoryPermission::USER_READ_EXECUTE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NORMAL,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        manager.update(
            BASE + 0x1000,
            1,
            KMemoryState::CODE,
            KMemoryPermission::USER_READ,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NORMAL,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        manager.update(
            BASE + 0x2000,
            1,
            KMemoryState::CODE_DATA,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NORMAL,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        let path = b"sdmc:/switch/freebrick/freebrick.nro";
        let mut module_path = [0u8; 0x208];
        module_path[4..8].copy_from_slice(&(path.len() as i32).to_le_bytes());
        module_path[8..8 + path.len()].copy_from_slice(path);
        process
            .process_memory
            .write()
            .unwrap()
            .write_block((BASE + 0x1000) as u64, &module_path);

        process
    }

    #[test]
    fn module_discovery_reads_name_after_executable_region() {
        let process = module_process();

        assert_eq!(
            find_modules(&process).get(&0x20_0000).map(String::as_str),
            Some("freebrick.nro")
        );
        assert_eq!(get_module_end(&process, 0x20_0000), 0x20_2fff);
        assert_eq!(find_main_module_entrypoint(&process), 0x20_0000);
    }

    #[test]
    fn main_module_entrypoint_falls_back_to_code_region_start() {
        let mut process = KProcess::new();
        process
            .page_table
            .configure_address_space(KProcessAddress::new(0), 0x1_0000_0000, 32);
        process
            .page_table
            .set_code_region(KProcessAddress::new(0x60_0000), 0x10_000);

        assert_eq!(find_main_module_entrypoint(&process), 0x60_0000);
    }
}

/// Get a backtrace from a thread.
/// Corresponds to upstream `Core::GetBacktrace`.
pub fn get_backtrace(thread: &KThread) -> Vec<BacktraceEntry> {
    let ctx = &thread.thread_context;
    let arm_ctx = ThreadContext {
        r: ctx.r,
        fp: ctx.fp,
        lr: ctx.lr,
        sp: ctx.sp,
        pc: ctx.pc,
        pstate: ctx.pstate,
        padding: ctx.padding,
        v: ctx.v,
        fpcr: ctx.fpcr,
        fpsr: ctx.fpsr,
        tpidr: ctx.tpidr,
    };
    if let Some(parent) = thread.parent.as_ref().and_then(|w| w.upgrade()) {
        let process = parent.lock().unwrap();
        get_backtrace_from_context(&process, &arm_ctx)
    } else {
        Vec::new()
    }
}

/// Symbolicate a backtrace by resolving module names and symbol names.
/// Corresponds to upstream anonymous `SymbolicateBacktrace` (debug.cpp).
fn symbolicate_backtrace(
    process: &crate::hle::kernel::k_process::KProcess,
    out: &mut Vec<BacktraceEntry>,
    is_64: bool,
) {
    let modules = find_modules(process);
    let segment_base = SEGMENT_BASES[is_64 as usize];

    for entry in out.iter_mut() {
        // Find the module containing this address (reverse iteration).
        let mut found_module = None;
        for (base, name) in modules.iter().rev() {
            if entry.original_address >= *base {
                found_module = Some((*base, name.clone()));
                break;
            }
        }

        if let Some((base, name)) = found_module {
            entry.module = name;
            entry.offset = entry.original_address - base;
            entry.address = segment_base + entry.offset;
        }

        // Symbol lookup would go here via symbols::get_symbol_name.
    }
}
