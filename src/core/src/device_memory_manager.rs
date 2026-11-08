// SPDX-FileCopyrightText: Copyright 2024 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Constants owned by upstream `core/device_memory_manager.h`.
//!
//! Ruzu's active Maxwell instantiation currently lives in
//! `video_core/host1x/gpu_device_memory_manager.rs`. Keeping a second, unused
//! generic implementation here caused that implementation to drift from the
//! runtime path while retaining substantial dead state. The active manager
//! must eventually move back to this module when the `core`/`video_core` crate
//! boundary can represent upstream's traits without introducing a dependency
//! cycle.

/// Device page size constants matching upstream.
pub const DEVICE_PAGEBITS: usize = 12;
pub const DEVICE_PAGESIZE: usize = 1 << DEVICE_PAGEBITS;
pub const DEVICE_PAGEMASK: usize = DEVICE_PAGESIZE - 1;

/// Upstream template physical limits. The extended limit includes the NX2
/// graphics server's backing in addition to the application's 12 GiB layout.
pub const PHYSICAL_MIN_BITS: usize = 32;
pub const PHYSICAL_MAX_BITS: usize = 35;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_page_constants_match_upstream() {
        assert_eq!(DEVICE_PAGEBITS, 12);
        assert_eq!(DEVICE_PAGESIZE, 0x1000);
        assert_eq!(DEVICE_PAGEMASK, 0x0fff);
    }
}
