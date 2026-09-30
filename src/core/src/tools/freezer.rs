// SPDX-FileCopyrightText: Copyright 2019 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/tools/freezer.h and freezer.cpp
//! Memory freezer that prevents games from writing new values to certain addresses.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};


/// Type alias for virtual addresses.
pub type VAddr = u64;

/// Callback interval for the memory freezer (60 Hz).
///
/// Corresponds to upstream `memory_freezer_ns`.
pub const MEMORY_FREEZER_NS: u64 = 1_000_000_000 / 60;

/// A single frozen memory entry.
///
/// Corresponds to upstream `Tools::Freezer::Entry`.
#[derive(Debug, Clone)]
pub struct Entry {
    pub address: VAddr,
    pub width: u32,
    pub value: u64,
}

/// Memory freezer that prevents an application from writing new values to certain
/// memory locations.
///
/// Corresponds to upstream `Tools::Freezer`.
/// Upstream stores `CoreTiming&` and `Memory&`. We store `Arc<crate::memory::memory::SharedMemory>`.
/// CoreTiming event scheduling is replaced with a simple active flag — the caller
/// (typically the debug UI) triggers frame_callback periodically.
pub struct Freezer {
    active: AtomicBool,
    entries: Mutex<Vec<Entry>>,
    memory: Option<Arc<crate::memory::memory::SharedMemory>>,
}

impl Freezer {
    /// Create a new Freezer.
    ///
    /// Corresponds to upstream `Freezer::Freezer(CoreTiming&, Memory&)`.
    pub fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            entries: Mutex::new(Vec::new()),
            memory: None,
        }
    }

    /// Create a new Freezer with a memory reference.
    pub fn with_memory(memory: Arc<crate::memory::memory::SharedMemory>) -> Self {
        Self {
            active: AtomicBool::new(false),
            entries: Mutex::new(Vec::new()),
            memory: Some(memory),
        }
    }

    /// Set the memory reference.
    pub fn set_memory(&mut self, memory: Arc<crate::memory::memory::SharedMemory>) {
        self.memory = Some(memory);
    }

    /// Enables or disables the entire memory freezer.
    ///
    /// Corresponds to upstream `Freezer::SetActive`.
    pub fn set_active(&self, is_active: bool) {
        if !self.active.swap(is_active, Ordering::SeqCst) {
            // Was not previously active, now activating
            if is_active {
                self.fill_entry_reads();
                // Upstream: core_timing.schedule_event(MEMORY_FREEZER_NS, event);
                // In our model, the caller triggers frame_callback periodically.
                log::debug!("Memory freezer activated!");
            }
        } else {
            log::debug!("Memory freezer deactivated!");
        }
    }

    /// Returns whether or not the freezer is active.
    ///
    /// Corresponds to upstream `Freezer::IsActive`.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    /// Removes all entries from the freezer.
    ///
    /// Corresponds to upstream `Freezer::Clear`.
    pub fn clear(&self) {
        let mut entries = self.entries.lock().unwrap();
        log::debug!("Clearing all frozen memory values.");
        entries.clear();
    }

    /// Freezes a value to its current memory address.
    /// Width can be 1, 2, 4, or 8 (in bytes).
    ///
    /// Corresponds to upstream `Freezer::Freeze`.
    pub fn freeze(&self, address: VAddr, width: u32) -> u64 {
        let mut entries = self.entries.lock().unwrap();

        let current_value = self.memory_read_width(width, address);

        entries.push(Entry {
            address,
            width,
            value: current_value,
        });

        log::debug!(
            "Freezing memory for address={:016X}, width={:02X}, current_value={:016X}",
            address,
            width,
            current_value
        );

        current_value
    }

    /// Unfreezes the memory value at address.
    ///
    /// Corresponds to upstream `Freezer::Unfreeze`.
    pub fn unfreeze(&self, address: VAddr) {
        let mut entries = self.entries.lock().unwrap();
        log::debug!("Unfreezing memory for address={:016X}", address);
        entries.retain(|entry| entry.address != address);
    }

    /// Returns whether or not the address is frozen.
    ///
    /// Corresponds to upstream `Freezer::IsFrozen`.
    pub fn is_frozen(&self, address: VAddr) -> bool {
        let entries = self.entries.lock().unwrap();
        entries.iter().any(|entry| entry.address == address)
    }

    /// Sets the value that address should be frozen to.
    ///
    /// Corresponds to upstream `Freezer::SetFrozenValue`.
    pub fn set_frozen_value(&self, address: VAddr, value: u64) {
        let mut entries = self.entries.lock().unwrap();

        if let Some(entry) = entries.iter_mut().find(|e| e.address == address) {
            log::debug!(
                "Manually overridden freeze value for address={:016X}, width={:02X} to value={:016X}",
                entry.address,
                entry.width,
                value
            );
            entry.value = value;
        } else {
            log::error!(
                "Tried to set freeze value for address={:016X} that is not frozen!",
                address
            );
        }
    }

    /// Returns the entry corresponding to the address if frozen.
    ///
    /// Corresponds to upstream `Freezer::GetEntry`.
    pub fn get_entry(&self, address: VAddr) -> Option<Entry> {
        let entries = self.entries.lock().unwrap();
        entries.iter().find(|e| e.address == address).cloned()
    }

    /// Returns all entries in the freezer.
    ///
    /// Corresponds to upstream `Freezer::GetEntries`.
    pub fn get_entries(&self) -> Vec<Entry> {
        let entries = self.entries.lock().unwrap();
        entries.clone()
    }

    /// Frame callback: enforce frozen values by writing them back to memory.
    ///
    /// Corresponds to upstream `Freezer::FrameCallback`.
    pub fn frame_callback(&self) {
        if !self.is_active() {
            return;
        }

        let entries = self.entries.lock().unwrap();
        for entry in entries.iter() {
            log::trace!(
                "Enforcing memory freeze at address={:016X}, value={:016X}, width={:02X}",
                entry.address,
                entry.value,
                entry.width
            );
            self.memory_write_width(entry.width, entry.address, entry.value);
        }
    }

    /// Update all entry values from memory.
    ///
    /// Corresponds to upstream `Freezer::FillEntryReads`.
    fn fill_entry_reads(&self) {
        let mut entries = self.entries.lock().unwrap();
        log::debug!("Updating memory freeze entries to current values.");
        for entry in entries.iter_mut() {
            entry.value = self.memory_read_width(entry.width, entry.address);
        }
    }

    /// Read memory at a given address with the specified width.
    /// Port of upstream anonymous `MemoryReadWidth`.
    fn memory_read_width(&self, width: u32, addr: VAddr) -> u64 {
        let Some(ref memory) = self.memory else {
            return 0;
        };
        let mem = memory.access().unwrap();
        match width {
            1 => mem.read_8(addr) as u64,
            2 => mem.read_16(addr) as u64,
            4 => mem.read_32(addr) as u64,
            8 => mem.read_64(addr),
            _ => {
                log::error!("Invalid memory width: {}", width);
                0
            }
        }
    }

    /// Write memory at a given address with the specified width.
    /// Port of upstream anonymous `MemoryWriteWidth`.
    fn memory_write_width(&self, width: u32, addr: VAddr, value: u64) {
        let Some(ref memory) = self.memory else {
            return;
        };
        let mem = memory.access().unwrap();
        match width {
            1 => mem.write_8(addr, value as u8),
            2 => mem.write_16(addr, value as u16),
            4 => mem.write_32(addr, value as u32),
            8 => mem.write_64(addr, value),
            _ => log::error!("Invalid memory width: {}", width),
        }
    }
}

impl Default for Freezer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_freezer_basic() {
        let freezer = Freezer::new();
        assert!(!freezer.is_active());
        assert!(freezer.get_entries().is_empty());
    }

    #[test]
    fn test_freeze_unfreeze() {
        let freezer = Freezer::new();
        freezer.freeze(0x1000, 4);
        assert!(freezer.is_frozen(0x1000));
        assert!(!freezer.is_frozen(0x2000));

        freezer.unfreeze(0x1000);
        assert!(!freezer.is_frozen(0x1000));
    }

    #[test]
    fn test_set_frozen_value() {
        let freezer = Freezer::new();
        freezer.freeze(0x1000, 4);
        freezer.set_frozen_value(0x1000, 42);

        let entry = freezer.get_entry(0x1000).unwrap();
        assert_eq!(entry.value, 42);
        assert_eq!(entry.width, 4);
    }

    #[test]
    fn test_clear() {
        let freezer = Freezer::new();
        freezer.freeze(0x1000, 4);
        freezer.freeze(0x2000, 8);
        assert_eq!(freezer.get_entries().len(), 2);
        freezer.clear();
        assert!(freezer.get_entries().is_empty());
    }
}
