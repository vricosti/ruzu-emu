// SPDX-FileCopyrightText: Copyright 2014 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/arm/arm_interface.h and arm_interface.cpp
//! ArmInterface abstract base class (register access, step, run, etc.)

use std::sync::{
    atomic::{AtomicPtr, Ordering},
    Arc,
};

use crate::hardware_properties;
pub use crate::hle::kernel::k_process::{DebugWatchpoint, DebugWatchpointType};

use bitflags::bitflags;

// Forward-declared opaque types matching C++ forward declarations.
// These serve the same purpose as `class KThread;` / `class KProcess;`
// in upstream headers. The ArmInterface trait uses these as opaque
// pointers in run_thread/step_thread/signal_interrupt signatures.
// At runtime, real `hle::kernel::k_thread::KThread` /
// `hle::kernel::k_process::KProcess` instances are transmuted through
// these types. We cannot use the real types directly because that would
// create a circular dependency: KThread uses ThreadContext from this
// module, and this module would need KThread.

/// Opaque type representing Kernel::KThread (forward declaration).
pub struct KThread {
    _private: (),
}

/// Opaque type representing Kernel::KProcess (forward declaration).
pub struct KProcess {
    _private: (),
}

/// Opaque type representing Kernel::Svc::ThreadContext
/// Matches upstream: 29 GPRs (x0-x28 / r0-r28), fp, lr, sp, pc, pstate, padding,
/// 32 vector regs, fpcr, fpsr, tpidr
#[derive(Clone, Default)]
#[repr(C)]
pub struct ThreadContext {
    pub r: [u64; 29],
    pub fp: u64,
    pub lr: u64,
    pub sp: u64,
    pub pc: u64,
    pub pstate: u32,
    pub padding: u32,
    pub v: [u128; 32],
    pub fpcr: u32,
    pub fpsr: u32,
    pub tpidr: u64,
}

/// Array of watchpoints, matching Core::WatchpointArray
pub type WatchpointArray = [DebugWatchpoint; hardware_properties::NUM_WATCHPOINTS as usize];

/// Shared access to the process-owned watchpoint array. Rust callbacks are
/// moved into the JIT, so they share the same pointer slot as their parent ARM
/// interface instead of retaining Eden's direct parent reference.
pub(crate) type SharedWatchpointArray = Arc<AtomicPtr<WatchpointArray>>;

// NOTE: these values match the HaltReason enum in Dynarmic
bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct HaltReason: u64 {
        const STEP_THREAD           = 0x00000001;
        const DATA_ABORT            = 0x00000004;
        const BREAK_LOOP            = 0x02000000;
        const SUPERVISOR_CALL       = 0x04000000;
        const INSTRUCTION_BREAKPOINT = 0x08000000;
        const PREFETCH_ABORT        = 0x20000000;
    }
}

/// CPU architecture mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Architecture {
    AArch64,
    AArch32,
}

/// Generic ARMv8 CPU interface
///
/// Corresponds to upstream `Core::ArmInterface`.
pub trait ArmInterface: Send {
    /// Perform any backend-specific initialization.
    fn initialize(&mut self) {}

    /// Runs the CPU until an event happens.
    fn run_thread(&mut self, thread: &mut KThread) -> HaltReason;

    /// Runs the CPU for one instruction or until an event happens.
    fn step_thread(&mut self, thread: &mut KThread) -> HaltReason;

    /// Admits a backend-specific mechanism to lock the thread context.
    fn lock_thread(&mut self, _thread: &mut KThread) {}

    /// Admits a backend-specific mechanism to unlock the thread context.
    fn unlock_thread(&mut self, _thread: &mut KThread) {}

    /// Clear the entire instruction cache for this CPU.
    fn clear_instruction_cache(&mut self);

    /// Clear a range of the instruction cache for this CPU.
    fn invalidate_cache_range(&mut self, addr: u64, size: usize);

    /// Diagnostic: dump the JIT's emitted-block map (host entry -> guest
    /// location) to `path` for host-profiler attribution. Default: no-op.
    fn dump_jit_block_map(&mut self, _path: &str) {}

    /// Get the current architecture.
    /// Returns AArch64 when PSTATE.nRW == 0 and AArch32 when PSTATE.nRW == 1.
    fn get_architecture(&self) -> Architecture;

    /// Context accessors. Should not be called if the CPU is running.
    fn get_context(&self, ctx: &mut ThreadContext);
    fn set_context(&mut self, ctx: &ThreadContext);
    fn set_tpidrro_el0(&mut self, value: u64);
    fn get_tpidrro_el0(&self) -> u64 {
        0
    }

    fn get_svc_arguments(&self, args: &mut [u64; 8]);
    fn set_svc_arguments(&mut self, args: &[u64; 8]);
    fn get_svc_number(&self) -> u32;
    fn get_last_exception_address(&self) -> Option<u64> {
        None
    }

    fn set_watchpoint_array(&mut self, watchpoints: *const WatchpointArray);

    /// Signal an interrupt for execution to halt as soon as possible.
    /// It is safe to call this if the CPU is not running.
    fn signal_interrupt(&mut self, thread: &mut KThread);

    /// Debug functionality.
    fn halted_watchpoint(&self) -> Option<DebugWatchpoint>;
    fn rewind_breakpoint_instruction(&mut self);
}

/// Base state shared by all ArmInterface implementations.
pub struct ArmInterfaceBase {
    watchpoints: SharedWatchpointArray,
    pub uses_wall_clock: bool,
}

impl ArmInterfaceBase {
    pub fn new(uses_wall_clock: bool) -> Self {
        Self {
            watchpoints: Arc::new(AtomicPtr::new(std::ptr::null_mut())),
            uses_wall_clock,
        }
    }

    pub fn set_watchpoint_array(&mut self, watchpoints: *const WatchpointArray) {
        self.watchpoints
            .store(watchpoints.cast_mut(), Ordering::Release);
    }

    pub(crate) fn shared_watchpoint_array(&self) -> SharedWatchpointArray {
        Arc::clone(&self.watchpoints)
    }

    /// Stack trace generation.
    /// Corresponds to upstream `ArmInterface::LogBacktrace`.
    pub fn log_backtrace(
        &self,
        process: &crate::hle::kernel::k_process::KProcess,
        ctx: &ThreadContext,
    ) {
        log::error!("Backtrace, sp={:016X}, pc={:016X}", ctx.sp, ctx.pc);
        log::error!(
            "{:20}{:20}{:20}{:20}{}",
            "Module Name",
            "Address",
            "Original Address",
            "Offset",
            "Symbol"
        );
        log::error!("");
        let entries = crate::arm::debug::get_backtrace_from_context(process, ctx);
        for entry in &entries {
            log::error!(
                "{:20}{:#20X}{:#20X}{:#20X}{}",
                entry.module,
                entry.address,
                entry.original_address,
                entry.offset,
                entry.name,
            );
        }
    }

    /// Matches upstream `ArmInterface::MatchingWatchpoint`.
    pub fn matching_watchpoint(
        &self,
        addr: u64,
        size: u64,
        access_type: DebugWatchpointType,
    ) -> Option<DebugWatchpoint> {
        matching_watchpoint(&self.watchpoints, addr, size, access_type)
    }
}

/// Rust callback counterpart of calling `m_parent.MatchingWatchpoint(...)` in
/// Eden. The shared pointer slot is owned by `ArmInterfaceBase` and updated by
/// `PhysicalCore::load_context` before the thread runs.
pub(crate) fn matching_watchpoint(
    watchpoint_array: &SharedWatchpointArray,
    addr: u64,
    size: u64,
    access_type: DebugWatchpointType,
) -> Option<DebugWatchpoint> {
    let watchpoints = watchpoint_array.load(Ordering::Acquire);
    if watchpoints.is_null() {
        return None;
    }
    let watchpoints = unsafe { &*watchpoints };

    let start_address = addr;
    let end_address = addr.wrapping_add(size);

    for watch in watchpoints.iter() {
        if end_address <= watch.start_address.get() {
            continue;
        }
        if start_address >= watch.end_address.get() {
            continue;
        }
        if !(access_type & watch.type_).is_empty() {
            return Some(*watch);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::kernel::k_typed_address::KProcessAddress;

    #[test]
    fn matching_watchpoint_uses_half_open_ranges_and_access_flags() {
        let mut watchpoints =
            [DebugWatchpoint::default(); hardware_properties::NUM_WATCHPOINTS as usize];
        watchpoints[0] = DebugWatchpoint {
            start_address: KProcessAddress::new(0x1000),
            end_address: KProcessAddress::new(0x1010),
            type_: DebugWatchpointType::READ,
        };
        let mut interface = ArmInterfaceBase::new(false);
        interface.set_watchpoint_array(&watchpoints);

        assert_eq!(
            interface
                .matching_watchpoint(0x1008, 4, DebugWatchpointType::READ)
                .map(|watchpoint| watchpoint.start_address.get()),
            Some(0x1000)
        );
        assert!(interface
            .matching_watchpoint(0x1010, 4, DebugWatchpointType::READ)
            .is_none());
        assert!(interface
            .matching_watchpoint(0x1008, 4, DebugWatchpointType::WRITE)
            .is_none());
    }
}
