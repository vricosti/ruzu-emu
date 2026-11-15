// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/rasterizer_download_area.h
//!
//! Describes a region of GPU memory to be read back to the CPU.
//!
//! The asynchronous read-back declarations (`in_flight`, `G_READ_DOWNLOADS`,
//! `may_hold_gpu_writes`, `read_downloads_may_cover`, `DownloadLanding`) are
//! ported from Lichii's version of this header
//! (`lichii/src/video_core/rasterizer_download_area.h`, GPL-3.0-or-later), a
//! divergence from Eden accepted by the user on 2026-11-14 (see DIFF.md).

use std::sync::atomic::{AtomicU32, Ordering};

/// Lichii `VideoCore::DownloadLanding`. It lives in `core::gpu_core` because
/// `core` (guest memory writes) must reach it and cannot depend on
/// `video_core`.
pub use ruzu_core::gpu_core::DownloadLanding;

/// Virtual address type.
pub type VAddr = u64;

/// Describes a download (read-back) region.
#[derive(Debug, Clone, Copy)]
pub struct RasterizerDownloadArea {
    pub start_address: VAddr,
    pub end_address: VAddr,
    /// When true, this area was preemptively flushed.
    pub preemtive: bool,
    /// What guest memory lacks there is on its way for an earlier read:
    /// nothing to flush, only to wait for (`RasterizerInterface::finish_read`).
    ///
    /// Lichii `RasterizerDownloadArea::in_flight`
    /// (`rasterizer_download_area.h:20-23`).
    pub in_flight: bool,
}

impl Default for RasterizerDownloadArea {
    fn default() -> Self {
        Self {
            start_address: 0,
            end_address: 0,
            preemtive: false,
            in_flight: false,
        }
    }
}

/// Downloads for guest CPU reads that are submitted but not yet written to
/// guest memory (`RasterizerInterface::flush_region_for_read`). While there
/// are any, a write to guest memory from the GPU's side first finishes those
/// it overlaps (`finish_read`), so none lands over it later.
///
/// Lichii `VideoCore::g_read_downloads` (`rasterizer_download_area.h:26-29`).
pub static G_READ_DOWNLOADS: AtomicU32 = AtomicU32::new(0);

/// Lichii `VideoCore::MayHoldGpuWrites` (defined in Lichii `gpu.cpp:670-675`):
/// `!Tegra::g_nx2_ampere || Tegra::Nx2MayBeGpuWritten(addr, size)`.
///
/// ruzu has no `Nx2MayBeGpuWritten` page filter, so every page may hold GPU
/// writes, which is Lichii's answer for titles without that filter. This only
/// makes more GPU-side writes take the buffer cache's lock while a read
/// download is pending; it never lets one through that Lichii would stop.
pub fn may_hold_gpu_writes(_addr: u64, _size: u64) -> bool {
    true
}

/// Whether such a download may still land over `[addr, addr + size)`: what a
/// write to guest memory from the GPU's side asks before `finish_read`, on any
/// thread, without the buffer cache's lock.
///
/// Lichii `VideoCore::ReadDownloadsMayCover` (`rasterizer_download_area.h:35-42`).
pub fn read_downloads_may_cover(addr: u64, size: u64) -> bool {
    G_READ_DOWNLOADS.load(Ordering::Acquire) != 0 && may_hold_gpu_writes(addr, size)
}