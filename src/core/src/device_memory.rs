//! Port of zuyu/src/core/device_memory.h and zuyu/src/core/device_memory.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-05
//!
//! Device memory allocation. Allocates a large block of host memory for the
//! emulated Switch DRAM, using common::host_memory::HostMemory.

use common::host_memory::HostMemory;

/// DRAM memory map constants, matching C++ Core::DramMemoryMap.
pub mod dram_memory_map {
    pub const BASE: u64 = 0x8000_0000;
    pub const KERNEL_RESERVE_BASE: u64 = BASE + 0x60000;
    pub const SLAB_HEAP_BASE: u64 = KERNEL_RESERVE_BASE + 0x85000;
}

/// Virtual reserve size for the host memory mapping.
const VIRTUAL_RESERVE_SIZE: usize = 1 << 39; // 512 GiB

/// Represents the Switch's device DRAM, backed by a host memory allocation.
pub struct DeviceMemory {
    pub buffer: HostMemory,
}

impl DeviceMemory {
    /// Creates a new DeviceMemory with the default intended memory size.
    pub fn new() -> Self {
        Self::with_size(crate::hle::kernel::board::k_system_control::init::get_intended_memory_size())
    }

    /// Creates a new DeviceMemory with a specific backing size.
    pub fn with_size(backing_size: usize) -> Self {
        let buffer = HostMemory::new(backing_size, VIRTUAL_RESERVE_SIZE);
        Self { buffer }
    }

    /// Gets the physical address for a pointer within the device memory.
    /// The physical address is offset by DramMemoryMap::Base.
    ///
    /// # Safety
    /// The pointer must be within the backing memory range.
    pub unsafe fn get_physical_addr(&self, ptr: *const u8) -> u64 {
        self.get_physical_addr_uintptr(ptr as usize)
    }

    /// Upstream `GetPhysicalAddr(uintptr_t ptr)` overload (Eden 5f142c7926):
    /// the integer form used by the page-table traversal, where the host
    /// pointer comes packed out of a `PageEntryData`.
    pub fn get_physical_addr_uintptr(&self, ptr: usize) -> u64 {
        (ptr.wrapping_sub(self.buffer.backing_base_pointer() as usize)) as u64
            + dram_memory_map::BASE
    }

    /// Gets the raw physical address (without DramMemoryMap::Base offset).
    ///
    /// # Safety
    /// The pointer must be within the backing memory range.
    pub unsafe fn get_raw_physical_addr(&self, ptr: *const u8) -> u64 {
        (ptr as usize - self.buffer.backing_base_pointer() as usize) as u64
    }

    /// Gets a mutable pointer to a physical address within device memory.
    ///
    /// # Safety
    /// The address must be a valid physical address within the DRAM range.
    pub unsafe fn get_pointer(&self, addr: u64) -> *mut u8 {
        self.buffer
            .backing_base_pointer()
            .add((addr - dram_memory_map::BASE) as usize)
    }

    /// Gets a const pointer to a physical address within device memory.
    ///
    /// # Safety
    /// The address must be a valid physical address within the DRAM range.
    pub unsafe fn get_pointer_const(&self, addr: u64) -> *const u8 {
        self.buffer
            .backing_base_pointer()
            .add((addr - dram_memory_map::BASE) as usize) as *const u8
    }

    /// Gets a mutable pointer from a raw physical address (no base offset).
    ///
    /// # Safety
    /// The raw address must be within the backing memory range.
    pub unsafe fn get_pointer_from_raw(&self, addr: u64) -> *mut u8 {
        self.buffer.backing_base_pointer().add(addr as usize)
    }

    /// Gets a const pointer from a raw physical address (no base offset).
    ///
    /// # Safety
    /// The raw address must be within the backing memory range.
    pub unsafe fn get_pointer_from_raw_const(&self, addr: u64) -> *const u8 {
        self.buffer.backing_base_pointer().add(addr as usize) as *const u8
    }
}

impl Default for DeviceMemory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_backing_reaches_last_byte_after_layout_changes() {
        use common::settings::MemoryLayout;
        const CHILD: &str = "RUZU_TEST_CONFIGURED_DRAM_BACKING";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "device_memory::tests::configured_backing_reaches_last_byte_after_layout_changes"])
                .env(CHILD, "1").status().unwrap();
            assert!(status.success());
            return;
        }
        for (mode, gib) in [
            (MemoryLayout::Memory4Gb, 4usize),
            (MemoryLayout::Memory6Gb, 6),
            (MemoryLayout::Memory8Gb, 8),
            (MemoryLayout::Memory10Gb, 10),
            (MemoryLayout::Memory12Gb, 12),
            (MemoryLayout::Memory4Gb, 4),
        ] {
            {
                let mut settings = common::settings::values_mut();
                settings.memory_layout_mode.set_global(true);
                settings.memory_layout_mode.set_value(mode);
            }
            let memory = DeviceMemory::new();
            assert_eq!(memory.buffer.backing_size(), gib << 30);
            // Sparse backing: touch only the boundary pages, not all guest RAM.
            unsafe {
                let first = memory.get_pointer(dram_memory_map::BASE);
                let last = memory.get_pointer(dram_memory_map::BASE + (gib << 30) as u64 - 1);
                first.write_volatile(0x12);
                last.write_volatile(0x34);
                assert_eq!(first.read_volatile(), 0x12);
                assert_eq!(last.read_volatile(), 0x34);
            }
        }
    }
}
