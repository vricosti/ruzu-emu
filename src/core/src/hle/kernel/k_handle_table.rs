//! Port of zuyu/src/core/hle/kernel/k_handle_table.h / k_handle_table.cpp
//! Status: Partial (structural port, core logic preserved)
//! Derniere synchro: 2026-03-11
//!
//! KHandleTable: manages a fixed-size table of kernel object handles.

use super::svc_common::Handle;

/// Rust diagnostic switch `RUZU_HANDLE_MISS_TRACE`, read once: lookups of
/// pseudo or stale handles are frequent on the guest WaitSynchronization path.
fn handle_miss_trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_HANDLE_MISS_TRACE").is_some())
}

/// Maximum table size.
pub const MAX_TABLE_SIZE: usize = 1024;

/// Minimum linear ID.
pub const MIN_LINEAR_ID: u16 = 1;
/// Maximum linear ID.
pub const MAX_LINEAR_ID: u16 = 0x7FFF;

/// Entry info — either a linear id (when occupied) or a next-free index (when free).
#[derive(Clone, Copy)]
pub union EntryInfo {
    pub linear_id: u16,
    pub next_free_index: i16,
}

impl Default for EntryInfo {
    fn default() -> Self {
        Self { linear_id: 0 }
    }
}

/// Encode a handle from index and linear id.
/// Matches upstream `KHandleTable::EncodeHandle`.
pub fn encode_handle(index: u16, linear_id: u16) -> Handle {
    ((linear_id as u32) << 15) | (index as u32)
}

/// Decode a handle into (index, linear_id, reserved).
/// Matches upstream `KHandleTable::HandlePack`.
pub fn decode_handle(handle: Handle) -> (u16, u16, u8) {
    let index = (handle & 0x7FFF) as u16;
    let linear_id = ((handle >> 15) & 0x7FFF) as u16;
    let reserved = ((handle >> 30) & 0x3) as u8;
    (index, linear_id, reserved)
}

/// The handle table.
/// Matches upstream `KHandleTable` class (k_handle_table.h).
///
/// In upstream, entries store `KAutoObject*`. We use opaque u64 object IDs.
pub struct KHandleTable {
    pub entry_infos: [EntryInfo; MAX_TABLE_SIZE],
    pub objects: [u64; MAX_TABLE_SIZE], // 0 = null/empty
    pub free_head_index: i32,
    pub table_size: u16,
    pub max_count: u16,
    pub next_linear_id: u16,
    pub count: u16,
}

impl KHandleTable {
    pub fn new() -> Self {
        Self {
            entry_infos: [EntryInfo { linear_id: 0 }; MAX_TABLE_SIZE],
            objects: [0u64; MAX_TABLE_SIZE],
            free_head_index: -1,
            table_size: 0,
            max_count: 0,
            next_linear_id: MIN_LINEAR_ID,
            count: 0,
        }
    }

    /// Initialize the handle table with the given size.
    /// Matches upstream `KHandleTable::Initialize`.
    pub fn initialize(&mut self, size: i32) -> u32 {
        if size > MAX_TABLE_SIZE as i32 {
            return 1; // ResultOutOfMemory
        }

        self.max_count = 0;
        self.table_size = if size <= 0 {
            MAX_TABLE_SIZE as u16
        } else {
            size as u16
        };
        self.next_linear_id = MIN_LINEAR_ID;
        self.count = 0;
        self.free_head_index = -1;

        for i in 0..self.table_size as usize {
            self.objects[i] = 0;
            self.entry_infos[i] = EntryInfo {
                next_free_index: (i as i16) - 1,
            };
            self.free_head_index = i as i32;
        }

        0 // ResultSuccess
    }

    pub fn get_table_size(&self) -> usize {
        self.table_size as usize
    }

    pub fn get_count(&self) -> usize {
        self.count as usize
    }

    pub fn get_max_count(&self) -> usize {
        self.max_count as usize
    }

    /// Allocate a free entry index.
    fn allocate_entry(&mut self) -> i32 {
        assert!((self.count as usize) < (self.table_size as usize));

        let index = self.free_head_index;
        self.free_head_index = unsafe { self.entry_infos[index as usize].next_free_index as i32 };
        self.count += 1;
        if self.count > self.max_count {
            self.max_count = self.count;
        }

        index
    }

    /// Free an entry by index.
    fn free_entry(&mut self, index: i32) {
        assert!(self.count > 0);

        self.objects[index as usize] = 0;
        self.entry_infos[index as usize] = EntryInfo {
            next_free_index: self.free_head_index as i16,
        };
        self.free_head_index = index;
        self.count -= 1;
    }

    /// Allocate the next linear ID.
    fn allocate_linear_id(&mut self) -> u16 {
        let id = self.next_linear_id;
        self.next_linear_id += 1;
        if self.next_linear_id > MAX_LINEAR_ID {
            self.next_linear_id = MIN_LINEAR_ID;
        }
        id
    }

    /// Check if a handle is valid.
    pub fn is_valid_handle(&self, handle: Handle) -> bool {
        let (index, linear_id, reserved) = decode_handle(handle);
        if reserved != 0 || handle == 0 || linear_id == 0 {
            return false;
        }
        if index as usize >= self.table_size as usize {
            return false;
        }
        if self.objects[index as usize] == 0 {
            return false;
        }
        let stored_id = unsafe { self.entry_infos[index as usize].linear_id };
        stored_id == linear_id
    }

    /// Add an object to the table and return a handle.
    /// The object_id is an opaque identifier for the kernel object.
    pub fn add(&mut self, object_id: u64) -> Result<Handle, u32> {
        if self.count >= self.table_size {
            return Err(1); // ResultOutOfMemory
        }

        let index = self.allocate_entry();
        let linear_id = self.allocate_linear_id();

        self.entry_infos[index as usize] = EntryInfo { linear_id };
        self.objects[index as usize] = object_id;

        Ok(encode_handle(index as u16, linear_id))
    }

    /// Register an object into a previously reserved handle slot.
    ///
    /// Matches the ownership of upstream `KHandleTable::Register(handle, object)`.
    /// The slot must have been allocated via `reserve()` and still be empty.
    pub fn register(&mut self, handle: Handle, object_id: u64) -> bool {
        let (index, linear_id, reserved) = decode_handle(handle);
        if reserved != 0 || handle == 0 || linear_id == 0 {
            return false;
        }
        if index as usize >= self.table_size as usize {
            return false;
        }
        if self.objects[index as usize] != 0 {
            return false;
        }
        let stored_linear_id = unsafe { self.entry_infos[index as usize].linear_id };
        if stored_linear_id != linear_id {
            return false;
        }

        self.objects[index as usize] = object_id;
        true
    }

    /// Remove a handle from the table.
    pub fn remove(&mut self, handle: Handle) -> bool {
        let (index, _linear_id, _reserved) = decode_handle(handle);
        if !self.is_valid_handle(handle) {
            return false;
        }
        self.free_entry(index as i32);
        true
    }

    /// Look up an object by handle.
    pub fn get_object(&self, handle: Handle) -> Option<u64> {
        let (index, linear_id, reserved) = decode_handle(handle);
        if reserved != 0 {
            // RUZU_HANDLE_MISS_TRACE=1 — log every reserved!=0 handle lookup.
            // Useful to surface lookups where the value is architecturally
            // invalid (e.g., 0x1 → linear_id=0 → reserved=0 but linear_id=0
            // also fails is_valid_handle).
            if handle_miss_trace_enabled() {
                eprintln!(
                    "[HANDLE_MISS] handle=0x{:08X} reserved={} (architecturally invalid)",
                    handle, reserved
                );
            }
            return None;
        }
        if self.is_valid_handle(handle) {
            Some(self.objects[index as usize])
        } else {
            // RUZU_HANDLE_MISS_TRACE=1 — log misses with full diagnostic. The
            // common failure mode for STK was `handle=0x1` (linear_id=0)
            // surfacing here because libnx parsed `0x1` from an IPC reply
            // slot that should have contained a handle but actually held a
            // domain object ID or raw payload byte.
            if handle_miss_trace_enabled() {
                let stored_obj = if (index as usize) < self.objects.len() {
                    self.objects[index as usize]
                } else {
                    0
                };
                let stored_lin = if (index as usize) < self.entry_infos.len() {
                    unsafe { self.entry_infos[index as usize].linear_id }
                } else {
                    0
                };
                eprintln!(
                    "[HANDLE_MISS] handle=0x{:08X} index={} linear_id={} stored_obj={} stored_linear_id={} table_size={} count={}",
                    handle, index, linear_id, stored_obj, stored_lin, self.table_size, self.count
                );
            }
            None
        }
    }

    /// Whether any remaining valid handle still references the given object.
    pub fn contains_object_id(&self, object_id: u64) -> bool {
        if object_id == 0 {
            return false;
        }

        for index in 0..self.table_size as usize {
            if self.objects[index] == object_id {
                let linear_id = unsafe { self.entry_infos[index].linear_id };
                let handle = encode_handle(index as u16, linear_id);
                if self.is_valid_handle(handle) {
                    return true;
                }
            }
        }

        false
    }

    /// Finalize the handle table (close all entries).
    /// Port of upstream `KHandleTable::Finalize`.
    pub fn finalize(&mut self) -> u32 {
        // Save and clear the table size.
        let saved_table_size = self.table_size;
        self.table_size = 0;

        // Close all entries.
        for i in 0..saved_table_size as usize {
            if self.objects[i] != 0 {
                // Upstream: obj->Close(). Object handles are opaque u64 here;
                // actual reference counting is managed by the object system.
                self.objects[i] = 0;
            }
        }
        self.count = 0;
        0 // R_SUCCEED
    }

    /// Reserve a handle slot without associating an object.
    pub fn reserve(&mut self) -> Result<Handle, u32> {
        if self.count >= self.table_size {
            return Err(1);
        }
        let index = self.allocate_entry();
        let linear_id = self.allocate_linear_id();
        self.entry_infos[index as usize] = EntryInfo { linear_id };
        Ok(encode_handle(index as u16, linear_id))
    }

    /// Unreserve a previously reserved handle.
    pub fn unreserve(&mut self, handle: Handle) {
        let (index, _linear_id, _reserved) = decode_handle(handle);
        self.free_entry(index as i32);
    }
}

impl Default for KHandleTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_handle() {
        let handle = encode_handle(42, 100);
        let (index, linear_id, reserved) = decode_handle(handle);
        assert_eq!(index, 42);
        assert_eq!(linear_id, 100);
        assert_eq!(reserved, 0);
    }

    #[test]
    fn test_handle_table_init_and_add() {
        let mut table = KHandleTable::new();
        assert_eq!(table.initialize(16), 0);
        assert_eq!(table.get_table_size(), 16);
        assert_eq!(table.get_count(), 0);

        let handle = table.add(0xDEAD).unwrap();
        assert_eq!(table.get_count(), 1);
        assert_eq!(table.get_object(handle), Some(0xDEAD));

        assert!(table.remove(handle));
        assert_eq!(table.get_count(), 0);
        assert_eq!(table.get_object(handle), None);
    }

    #[test]
    fn contains_object_id_tracks_remaining_live_handles() {
        let mut table = KHandleTable::new();
        assert_eq!(table.initialize(16), 0);

        let first = table.add(0xDEAD).unwrap();
        let second = table.add(0xDEAD).unwrap();
        assert!(table.contains_object_id(0xDEAD));

        assert!(table.remove(first));
        assert!(table.contains_object_id(0xDEAD));

        assert!(table.remove(second));
        assert!(!table.contains_object_id(0xDEAD));
    }
}
