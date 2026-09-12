// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/psc/time/shared_memory.h/.cpp
//!
//! SharedMemory: manages lock-free atomic access to time-related data
//! in a shared memory region accessible by both the kernel and userspace.

use std::sync::atomic::{fence, Ordering};
use std::sync::Arc;

use super::common::{
    ClockSourceId, ContinuousAdjustmentTimePoint, SteadyClockTimePoint, SystemClockContext,
};

// =========================================================================
// Lock-free atomic types (matching upstream LockFreeAtomicType<T>)
// =========================================================================

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct LockFreeAtomicSteadyClockTimePoint {
    pub counter: u32,
    pub _padding: u32,
    pub value: [SteadyClockTimePoint; 2],
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct LockFreeAtomicSystemClockContext {
    pub counter: u32,
    pub _padding: u32,
    pub value: [SystemClockContext; 2],
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct LockFreeAtomicBool {
    pub counter: u32,
    pub value: [bool; 2],
    pub _pad: [u8; 2],
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct LockFreeAtomicContinuousAdjustment {
    pub counter: u32,
    pub _padding: u32,
    pub value: [ContinuousAdjustmentTimePoint; 2],
}

/// SharedMemoryStruct layout matching upstream.
///
/// This is the raw struct mapped into the kernel shared memory region.
/// Guest code reads from this via lock-free atomic reads.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct SharedMemoryStruct {
    pub steady_time_points: LockFreeAtomicSteadyClockTimePoint, // 0x000
    pub local_system_clock_contexts: LockFreeAtomicSystemClockContext, // 0x038
    pub network_system_clock_contexts: LockFreeAtomicSystemClockContext, // 0x080
    pub automatic_corrections: LockFreeAtomicBool,              // 0x0C8
    pub continuous_adjustment_time_points: LockFreeAtomicContinuousAdjustment, // 0x0D0
    pub _pad0148: [u8; 0xEB8],                                  // 0x148 (pad to 0x1000)
}

// Upstream offset assertions
const _: () = assert!(core::mem::offset_of!(SharedMemoryStruct, steady_time_points) == 0x0);
const _: () =
    assert!(core::mem::offset_of!(SharedMemoryStruct, local_system_clock_contexts) == 0x38);
const _: () =
    assert!(core::mem::offset_of!(SharedMemoryStruct, network_system_clock_contexts) == 0x80);
const _: () = assert!(core::mem::offset_of!(SharedMemoryStruct, automatic_corrections) == 0xC8);
const _: () =
    assert!(core::mem::offset_of!(SharedMemoryStruct, continuous_adjustment_time_points) == 0xD0);
const _: () = assert!(core::mem::size_of::<SharedMemoryStruct>() == 0x1000);

// =========================================================================
// Lock-free read/write helpers
// =========================================================================

/// Read a value from a lock-free atomic double-buffered slot.
///
/// Corresponds to `ReadFromLockFreeAtomicType` in upstream shared_memory.cpp.
fn read_lock_free<T: Copy>(counter: &u32, values: &[T; 2]) -> T {
    loop {
        let c = *counter;
        let value = values[(c % 2) as usize];
        fence(Ordering::Acquire);
        if c == *counter {
            return value;
        }
    }
}

/// Write a value to a lock-free atomic double-buffered slot.
///
/// Corresponds to `WriteToLockFreeAtomicType` in upstream shared_memory.cpp.
fn write_lock_free<T: Copy>(counter: &mut u32, values: &mut [T; 2], value: T) {
    let c = counter.wrapping_add(1);
    values[(c % 2) as usize] = value;
    fence(Ordering::Release);
    *counter = c;
}

// =========================================================================
// SharedMemory service class
// =========================================================================

/// SharedMemory manages a SharedMemoryStruct region and provides setter
/// methods matching the upstream `SharedMemory` class.
///
/// Upstream wraps a kernel `KSharedMemory` and writes directly to the
/// mapped pointer via `m_k_shared_memory.GetPointer()`. We do the same
/// using KSharedMemory's DeviceMemory-backed physical pages.
pub struct SharedMemory {
    /// The kernel shared memory object backing this region.
    /// Matches upstream `Kernel::KSharedMemory& m_k_shared_memory`.
    k_shared_memory: Arc<crate::hle::kernel::k_shared_memory::KSharedMemory>,
    /// Cached pointer to the SharedMemoryStruct within the KSharedMemory backing.
    /// Matches upstream `SharedMemoryStruct* m_shared_memory_ptr`.
    shared_memory_ptr: *mut SharedMemoryStruct,
}

// SAFETY: SharedMemory is used behind Arc<Mutex<>> or single-threaded.
// The raw pointer points into DeviceMemory which outlives all services.
unsafe impl Send for SharedMemory {}
unsafe impl Sync for SharedMemory {}

/// Copyable view of the mapped time shared-memory page, used by context writers.
///
/// Eden's `LocalSystemClockContextWriter` / `NetworkSystemClockContextWriter`
/// hold `SharedMemory&` and write through `m_shared_memory_ptr`. The mapped
/// address is stable for the lifetime of `SharedMemory`.
#[derive(Clone, Copy)]
pub struct SharedMemoryWriterHandle {
    ptr: *mut SharedMemoryStruct,
}

// SAFETY: writes use the same lock-free protocol as `SharedMemory`.
unsafe impl Send for SharedMemoryWriterHandle {}
unsafe impl Sync for SharedMemoryWriterHandle {}

impl SharedMemoryWriterHandle {
    pub fn set_local_system_context(&self, context: &SystemClockContext) {
        let s = unsafe { &mut *self.ptr };
        write_lock_free(
            &mut s.local_system_clock_contexts.counter,
            &mut s.local_system_clock_contexts.value,
            *context,
        );
    }

    pub fn set_network_system_context(&self, context: &SystemClockContext) {
        let s = unsafe { &mut *self.ptr };
        write_lock_free(
            &mut s.network_system_clock_contexts.counter,
            &mut s.network_system_clock_contexts.value,
            *context,
        );
    }
}

impl SharedMemory {
    /// Create a new SharedMemory backed by DeviceMemory.
    ///
    /// Corresponds to `SharedMemory::SharedMemory(Core::System&)` in upstream.
    /// Upstream: `m_k_shared_memory{m_system.Kernel().GetTimeSharedMem()}`
    /// then `m_shared_memory_ptr = reinterpret_cast<SharedMemoryStruct*>(m_k_shared_memory.GetPointer())`.
    /// Create with DeviceMemory backing (production path).
    pub fn new(
        device_memory: &crate::device_memory::DeviceMemory,
        memory_manager: &mut crate::hle::kernel::k_memory_manager::KMemoryManager,
    ) -> Self {
        use crate::hle::kernel::k_shared_memory::{KSharedMemory, MemoryPermission};

        let mut k_shared_memory = KSharedMemory::new();
        k_shared_memory.initialize(
            device_memory,
            memory_manager,
            MemoryPermission::None,
            MemoryPermission::Read,
            core::mem::size_of::<SharedMemoryStruct>(),
        );
        let k_shared_memory = Arc::new(k_shared_memory);
        let ptr = k_shared_memory.get_pointer_mut(0) as *mut SharedMemoryStruct;
        Self {
            k_shared_memory,
            shared_memory_ptr: ptr,
        }
    }

    /// Create with heap-backed memory (test/legacy path when DeviceMemory
    /// is not available).
    pub fn new_for_test() -> Self {
        use crate::hle::kernel::k_shared_memory::KSharedMemory;

        // Use a heap allocation as fallback.
        let k_shared_memory = Arc::new(KSharedMemory::new());
        // Allocate a zeroed SharedMemoryStruct on the heap.
        let boxed = Box::new(SharedMemoryStruct {
            steady_time_points: Default::default(),
            local_system_clock_contexts: Default::default(),
            network_system_clock_contexts: Default::default(),
            automatic_corrections: Default::default(),
            continuous_adjustment_time_points: Default::default(),
            _pad0148: [0u8; 0xEB8],
        });
        let ptr = Box::into_raw(boxed);
        Self {
            k_shared_memory,
            shared_memory_ptr: ptr,
        }
    }

    /// Get a reference to the underlying KSharedMemory.
    /// Matches upstream `GetKSharedMemory()`.
    pub fn get_k_shared_memory(&self) -> &crate::hle::kernel::k_shared_memory::KSharedMemory {
        self.k_shared_memory.as_ref()
    }

    /// Get the shared owner for the underlying KSharedMemory.
    pub fn get_k_shared_memory_arc(
        &self,
    ) -> Arc<crate::hle::kernel::k_shared_memory::KSharedMemory> {
        Arc::clone(&self.k_shared_memory)
    }

    /// Get a pointer to the raw shared memory struct for external mapping.
    pub fn get_shared_memory_ptr(&self) -> *const SharedMemoryStruct {
        self.shared_memory_ptr as *const SharedMemoryStruct
    }

    /// Get a mutable pointer to the raw shared memory struct.
    pub fn get_shared_memory_ptr_mut(&mut self) -> *mut SharedMemoryStruct {
        self.shared_memory_ptr
    }

    /// Copyable handle for context writers. Eden's writers hold `SharedMemory&`;
    /// the mapped pointer is stable for the lifetime of this object.
    pub fn writer_handle(&self) -> SharedMemoryWriterHandle {
        SharedMemoryWriterHandle {
            ptr: self.shared_memory_ptr,
        }
    }

    /// Internal helper: get a safe reference to the shared memory struct.
    fn shared(&self) -> &SharedMemoryStruct {
        unsafe { &*self.shared_memory_ptr }
    }

    /// Internal helper: get a safe mutable reference to the shared memory struct.
    ///
    /// Writes go through the lock-free double-buffer protocol, so `&self` is
    /// enough — matching upstream's `m_shared_memory_ptr` mutation.
    fn shared_mut(&self) -> &mut SharedMemoryStruct {
        unsafe { &mut *self.shared_memory_ptr }
    }

    /// SetLocalSystemContext.
    ///
    /// Corresponds to `SharedMemory::SetLocalSystemContext` in upstream.
    pub fn set_local_system_context(&self, context: &SystemClockContext) {
        let s = self.shared_mut();
        write_lock_free(
            &mut s.local_system_clock_contexts.counter,
            &mut s.local_system_clock_contexts.value,
            *context,
        );
    }

    /// SetNetworkSystemContext.
    ///
    /// Corresponds to `SharedMemory::SetNetworkSystemContext` in upstream.
    pub fn set_network_system_context(&self, context: &SystemClockContext) {
        let s = self.shared_mut();
        write_lock_free(
            &mut s.network_system_clock_contexts.counter,
            &mut s.network_system_clock_contexts.value,
            *context,
        );
    }

    /// SetSteadyClockTimePoint.
    ///
    /// Corresponds to `SharedMemory::SetSteadyClockTimePoint` in upstream.
    pub fn set_steady_clock_time_point(&self, clock_source_id: ClockSourceId, time_point: i64) {
        let s = self.shared_mut();
        write_lock_free(
            &mut s.steady_time_points.counter,
            &mut s.steady_time_points.value,
            SteadyClockTimePoint {
                time_point,
                clock_source_id,
            },
        );
    }

    /// SetContinuousAdjustment.
    ///
    /// Corresponds to `SharedMemory::SetContinuousAdjustment` in upstream.
    pub fn set_continuous_adjustment(&self, time_point: &ContinuousAdjustmentTimePoint) {
        let s = self.shared_mut();
        write_lock_free(
            &mut s.continuous_adjustment_time_points.counter,
            &mut s.continuous_adjustment_time_points.value,
            *time_point,
        );
    }

    /// SetAutomaticCorrection.
    ///
    /// Corresponds to `SharedMemory::SetAutomaticCorrection` in upstream.
    pub fn set_automatic_correction(&self, automatic_correction: bool) {
        let s = self.shared_mut();
        write_lock_free(
            &mut s.automatic_corrections.counter,
            &mut s.automatic_corrections.value,
            automatic_correction,
        );
    }

    /// UpdateBaseTime.
    ///
    /// Corresponds to `SharedMemory::UpdateBaseTime` in upstream.
    /// Reads the current steady clock time point, updates its time_point field,
    /// and writes it back.
    pub fn update_base_time(&self, time: i64) {
        let s = self.shared_mut();
        let mut time_point =
            read_lock_free(&s.steady_time_points.counter, &s.steady_time_points.value);
        time_point.time_point = time;
        write_lock_free(
            &mut s.steady_time_points.counter,
            &mut s.steady_time_points.value,
            time_point,
        );
    }

    // =====================================================================
    // Read helpers (for testing / internal use)
    // =====================================================================

    /// Read the current local system clock context from shared memory.
    pub fn get_local_system_context(&self) -> SystemClockContext {
        read_lock_free(
            &self.shared().local_system_clock_contexts.counter,
            &self.shared().local_system_clock_contexts.value,
        )
    }

    /// Read the current network system clock context from shared memory.
    pub fn get_network_system_context(&self) -> SystemClockContext {
        read_lock_free(
            &self.shared().network_system_clock_contexts.counter,
            &self.shared().network_system_clock_contexts.value,
        )
    }

    /// Read the current steady clock time point from shared memory.
    pub fn get_steady_clock_time_point(&self) -> SteadyClockTimePoint {
        read_lock_free(
            &self.shared().steady_time_points.counter,
            &self.shared().steady_time_points.value,
        )
    }

    /// Read the current automatic correction setting from shared memory.
    pub fn get_automatic_correction(&self) -> bool {
        read_lock_free(
            &self.shared().automatic_corrections.counter,
            &self.shared().automatic_corrections.value,
        )
    }

    /// Read the current continuous adjustment time point from shared memory.
    pub fn get_continuous_adjustment(&self) -> ContinuousAdjustmentTimePoint {
        read_lock_free(
            &self.shared().continuous_adjustment_time_points.counter,
            &self.shared().continuous_adjustment_time_points.value,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_memory_struct_size() {
        assert_eq!(core::mem::size_of::<SharedMemoryStruct>(), 0x1000);
    }

    #[test]
    fn new_for_test_returns_stable_shared_memory_owner() {
        let sm = SharedMemory::new_for_test();
        let a = sm.get_k_shared_memory_arc();
        let b = sm.get_k_shared_memory_arc();
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn write_and_read_local_system_context() {
        let sm = SharedMemory::new_for_test();
        let ctx = SystemClockContext {
            offset: 12345,
            steady_time_point: SteadyClockTimePoint {
                time_point: 67890,
                clock_source_id: [1u8; 16],
            },
        };
        sm.set_local_system_context(&ctx);
        let read_ctx = sm.get_local_system_context();
        assert_eq!(read_ctx.offset, 12345);
        assert_eq!(read_ctx.steady_time_point.time_point, 67890);
    }

    #[test]
    fn write_and_read_automatic_correction() {
        let sm = SharedMemory::new_for_test();
        assert!(!sm.get_automatic_correction());
        sm.set_automatic_correction(true);
        assert!(sm.get_automatic_correction());
        sm.set_automatic_correction(false);
        assert!(!sm.get_automatic_correction());
    }

    #[test]
    fn update_base_time_preserves_clock_source_id() {
        let sm = SharedMemory::new_for_test();
        let source_id = [42u8; 16];
        sm.set_steady_clock_time_point(source_id, 100);

        sm.update_base_time(200);

        let tp = sm.get_steady_clock_time_point();
        assert_eq!(tp.time_point, 200);
        assert_eq!(tp.clock_source_id, source_id);
    }
}
