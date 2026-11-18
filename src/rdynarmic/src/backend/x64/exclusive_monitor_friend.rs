//! Friend accessors from upstream `backend/x64/exclusive_monitor_friend.h`.

use crate::interface::exclusive_monitor::{ExclusiveMonitor, Vector};

/// Returns the four-byte lock storage burned into x64 emitted code.
///
/// # Safety
///
/// `monitor` must point to a live, uniquely owned `ExclusiveMonitor` whose address remains stable
/// for the lifetime of the generated code.
pub unsafe fn get_exclusive_monitor_lock_pointer(monitor: *mut ExclusiveMonitor) -> *mut u32 {
    unsafe { &raw mut (*monitor).lock.locked as *mut u32 }
}

/// Returns the configured processor count.
///
/// # Safety
///
/// `monitor` must point to a live `ExclusiveMonitor`.
pub unsafe fn get_exclusive_monitor_processor_count(monitor: *mut ExclusiveMonitor) -> usize {
    unsafe { (*monitor).reservations.len() }
}

/// Returns the selected reservation-address slot.
///
/// # Safety
///
/// `monitor` must point to a live `ExclusiveMonitor` and `index` must be in range.
pub unsafe fn get_exclusive_monitor_address_pointer(
    monitor: *mut ExclusiveMonitor,
    index: usize,
) -> *mut u64 {
    unsafe {
        let reservations = &mut (*monitor).reservations;
        &mut reservations[index].address as *mut u64
    }
}

/// Returns the selected reserved-value slot.
///
/// # Safety
///
/// `monitor` must point to a live `ExclusiveMonitor` and `index` must be in range.
pub unsafe fn get_exclusive_monitor_value_pointer(
    monitor: *mut ExclusiveMonitor,
    index: usize,
) -> *mut Vector {
    unsafe {
        let reservations = &mut (*monitor).reservations;
        &mut reservations[index].value as *mut Vector
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessors_expose_the_matching_monitor_storage() {
        let mut monitor = ExclusiveMonitor::new(2);
        let monitor_ptr = &mut monitor as *mut ExclusiveMonitor;

        unsafe {
            assert_eq!(get_exclusive_monitor_processor_count(monitor_ptr), 2);
            get_exclusive_monitor_address_pointer(monitor_ptr, 1).write(0x1234);
            get_exclusive_monitor_value_pointer(monitor_ptr, 1).write([0x55, 0xaa]);
            assert_eq!(monitor.reservations[1].address, 0x1234);
            assert_eq!(monitor.reservations[1].value, [0x55, 0xaa]);
            // Lichii fd9c0133df: each processor's reservation on its own cache line.
            assert_eq!(get_exclusive_monitor_address_pointer(monitor_ptr, 1) as usize % 64, 0);
            assert_eq!(
                get_exclusive_monitor_address_pointer(monitor_ptr, 1) as usize
                    - get_exclusive_monitor_address_pointer(monitor_ptr, 0) as usize,
                64
            );
            assert!(!get_exclusive_monitor_lock_pointer(monitor_ptr).is_null());
        }
    }
}
