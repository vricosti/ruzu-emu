// SPDX-FileCopyrightText: Copyright 2020 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/arm/dynarmic/dynarmic_exclusive_monitor.h and .cpp
//! Dynarmic exclusive monitor implementation wrapping rdynarmic::ExclusiveMonitor.

use crate::arm::exclusive_monitor::{ExclusiveMonitor, VAddr};
use std::sync::Arc;

/// Dynarmic-backed exclusive monitor.
///
/// Corresponds to upstream `Core::DynarmicExclusiveMonitor`.
/// Wraps `rdynarmic::ExclusiveMonitor` and delegates to `Memory` for
/// the actual memory operations in the read/write callbacks.
pub struct DynarmicExclusiveMonitor {
    monitor: rdynarmic::ExclusiveMonitor,
    memory: Arc<crate::memory::memory::SharedMemory>,
}

impl DynarmicExclusiveMonitor {
    /// Create a new DynarmicExclusiveMonitor.
    ///
    /// Upstream: `DynarmicExclusiveMonitor(Memory::Memory& memory, size_t core_count)`
    pub fn new(memory: Arc<crate::memory::memory::SharedMemory>, core_count: usize) -> Self {
        Self {
            monitor: rdynarmic::ExclusiveMonitor::new(core_count),
            memory,
        }
    }

    /// Get the underlying rdynarmic ExclusiveMonitor (for JIT backend integration).
    pub fn get_monitor(&mut self) -> &mut rdynarmic::ExclusiveMonitor {
        &mut self.monitor
    }
}

impl ExclusiveMonitor for DynarmicExclusiveMonitor {
    fn exclusive_read8(&mut self, core_index: usize, addr: VAddr) -> u8 {
        let mem = self.memory.clone();
        self.monitor
            .read_and_mark(core_index, addr, || mem.access().unwrap().read_8(addr))
    }

    fn exclusive_read16(&mut self, core_index: usize, addr: VAddr) -> u16 {
        let mem = self.memory.clone();
        self.monitor
            .read_and_mark(core_index, addr, || mem.access().unwrap().read_16(addr))
    }

    fn exclusive_read32(&mut self, core_index: usize, addr: VAddr) -> u32 {
        let mem = self.memory.clone();
        self.monitor
            .read_and_mark(core_index, addr, || mem.access().unwrap().read_32(addr))
    }

    fn exclusive_read64(&mut self, core_index: usize, addr: VAddr) -> u64 {
        let mem = self.memory.clone();
        self.monitor
            .read_and_mark(core_index, addr, || mem.access().unwrap().read_64(addr))
    }

    fn exclusive_read128(&mut self, core_index: usize, addr: VAddr) -> u128 {
        let mem = self.memory.clone();
        self.monitor.read_and_mark(core_index, addr, || {
            let m = mem.access().unwrap();
            let lo = m.read_64(addr) as u128;
            let hi = m.read_64(addr + 8) as u128;
            (hi << 64) | lo
        })
    }

    fn clear_exclusive(&mut self, core_index: usize) {
        self.monitor.clear_processor(core_index);
    }

    fn exclusive_write8(&mut self, core_index: usize, vaddr: VAddr, value: u8) -> bool {
        let mem = self.memory.clone();
        self.monitor
            .do_exclusive_operation(core_index, vaddr, |expected: u8| {
                mem.access()
                    .unwrap()
                    .write_exclusive_8(vaddr, value, expected)
            })
    }

    fn exclusive_write16(&mut self, core_index: usize, vaddr: VAddr, value: u16) -> bool {
        let mem = self.memory.clone();
        self.monitor
            .do_exclusive_operation(core_index, vaddr, |expected: u16| {
                mem.access()
                    .unwrap()
                    .write_exclusive_16(vaddr, value, expected)
            })
    }

    fn exclusive_write32(&mut self, core_index: usize, vaddr: VAddr, value: u32) -> bool {
        let mem = self.memory.clone();
        self.monitor
            .do_exclusive_operation(core_index, vaddr, |expected: u32| {
                mem.access()
                    .unwrap()
                    .write_exclusive_32(vaddr, value, expected)
            })
    }

    fn exclusive_write64(&mut self, core_index: usize, vaddr: VAddr, value: u64) -> bool {
        let mem = self.memory.clone();
        self.monitor
            .do_exclusive_operation(core_index, vaddr, |expected: u64| {
                mem.access()
                    .unwrap()
                    .write_exclusive_64(vaddr, value, expected)
            })
    }

    fn exclusive_write128(&mut self, core_index: usize, vaddr: VAddr, value: u128) -> bool {
        let mem = self.memory.clone();
        self.monitor
            .do_exclusive_operation(core_index, vaddr, |expected: u128| {
                let value_lo = value as u64;
                let value_hi = (value >> 64) as u64;
                let expected_lo = expected as u64;
                let expected_hi = (expected >> 64) as u64;
                mem.access().unwrap().write_exclusive_128(
                    vaddr,
                    value_lo,
                    value_hi,
                    expected_lo,
                    expected_hi,
                )
            })
    }
}
