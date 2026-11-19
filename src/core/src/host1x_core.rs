// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Opaque Host1x bridge used by `core` owners that must talk to the
//! frontend-provided Host1x implementation without depending on `video_core`.

use std::any::Any;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host1xChannelType {
    MsEnc = 0,
    Vic = 1,
    Gpu = 2,
    NvDec = 3,
    Display = 4,
    NvJpg = 5,
    TSec = 6,
    Max = 7,
}

/// Cross-crate view of video_core/host1x/nx2_video_decoder.h::Nx2Picture.
/// The opaque frame remains owned by the native FFmpeg implementation.
pub struct Nx2Picture {
    pub width: u32,
    pub height: u32,
    pub pts: u64,
    pub frame: Arc<dyn Any + Send + Sync>,
}

/// Bridge mirror; the upstream-owned enum remains in video_core's decoder file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Nx2PictureFormat {
    Nv12,
    Rgba8,
    Bgra8,
}

/// Created and used on the decode worker, as upstream's single-thread decoder.
pub trait Nx2VideoDecoderHandle {
    fn is_open(&self) -> bool;
    fn name(&self) -> &str;
    fn decode(&mut self, data: &[u8], pts: u64, pictures: &mut Vec<Nx2Picture>);
    fn drain(&mut self, pictures: &mut Vec<Nx2Picture>);
    fn reset(&mut self);
    fn render(
        &mut self,
        picture: &Nx2Picture,
        format: Nx2PictureFormat,
        width: u32,
        height: u32,
        pitch: u64,
        chroma_offset: u64,
        out: &mut [u8],
    );
}

pub trait Host1xCoreInterface: Any + Send + Sync {
    fn create_nx2_video_decoder(&self, mime: &str) -> Box<dyn Nx2VideoDecoderHandle>;
    fn as_any(&self) -> &(dyn Any + Send + Sync);
    fn get_host_syncpoint_value(&self, id: u32) -> u32;
    fn get_guest_syncpoint_value(&self, id: u32) -> u32;
    /// SyncpointManager::IncrementGuest, publishing completed video frames.
    fn increment_guest(&self, id: u32);
    /// SyncpointManager::IncrementHost, for work that is complete when it is
    /// published (upstream pairs it with IncrementGuest in such producers).
    fn increment_host(&self, id: u32);
    /// Direct bridge to SyncpointManager::IsReadyGuest (acquire load, unsigned >=).
    fn is_ready_guest(&self, id: u32, expected_value: u32) -> bool;
    fn wait_host(&self, id: u32, expected_value: u32);
    fn register_guest_action(
        &self,
        id: u32,
        expected_value: u32,
        action: Box<dyn FnOnce() + Send>,
    ) -> Option<u64>;
    fn register_host_action(
        &self,
        id: u32,
        expected_value: u32,
        action: Box<dyn FnOnce() + Send>,
    ) -> Option<u64>;
    fn deregister_host_action(&self, id: u32, handle: u64);
    fn deregister_guest_action(&self, id: u32, handle: u64);

    /// Allocate `size` bytes of device-virtual (SMMU) address space and
    /// return the device address. Mirrors upstream
    /// `host1x.MemoryManager().Allocate(size)`.
    fn smmu_allocate(&self, size: usize) -> u64;

    /// Register an nvdrv process memory interface with the Host1x SMMU and
    /// return an ASID.
    ///
    /// Upstream stores a `Core::Memory::Memory*` in
    /// `DeviceMemoryManager<Traits>::registered_processes`. Rust passes the
    /// process `Memory` owner through the opaque Host1x bridge so video_core
    /// can keep the same per-ASID memory association.
    fn smmu_register_process(
        &self,
        memory: Option<Arc<crate::memory::memory::SharedMemory>>,
    ) -> u32;

    /// Unregister a process ASID previously returned by
    /// `smmu_register_process`, mirroring upstream
    /// `DeviceMemoryManager<Traits>::UnregisterProcess`.
    fn smmu_unregister_process(&self, asid: u32);

    /// Free a previously allocated SMMU range. Mirrors upstream
    /// `DeviceMemoryManager<Traits>::Free`.
    fn smmu_free(&self, d_address: u64, size: usize);

    /// Install an SMMU mapping by resolving CPU backing through the process
    /// registered for `asid`.
    ///
    /// Mirrors upstream
    /// `DeviceMemoryManager<Traits>::Map(address, virtual_address, size, asid, track)`.
    fn smmu_map(&self, d_address: u64, virtual_address: u64, size: usize, asid: u32, track: bool);

    /// Recompute SMMU continuity metadata for `[d_address, d_address + size)`.
    ///
    /// Mirrors the `TrackContinuityImpl(...)` side effect performed after
    /// upstream `DeviceMemoryManager::Map(..., track=true)`. Some Rust bridge
    /// callers map one page at a time because they already resolved host
    /// pointers in `core`, then call this once for the whole range to preserve
    /// upstream continuity tracking.
    fn smmu_track_continuity(&self, d_address: u64, size: usize);

    /// Recompute continuity from the CPU backing registered for `asid`.
    ///
    /// Mirrors upstream `DeviceMemoryManager::TrackContinuity(address,
    /// virtual_address, size, asid)`.
    fn smmu_track_continuity_registered(
        &self,
        d_address: u64,
        virtual_address: u64,
        size: usize,
        asid: u32,
    );

    /// Remove SMMU page-table mappings covering `[d_address, d_address +
    /// size)`. Mirrors upstream `DeviceMemoryManager<Traits>::Unmap`.
    fn smmu_unmap(&self, d_address: u64, size: usize);

    /// Look up the host pointer for a device address via the SMMU page
    /// table. Returns 0 if no mapping exists.
    fn smmu_lookup(&self, d_address: u64) -> usize;

    /// Host1x::MemoryManager().ReadBlock, including its pre-read cache flush.
    fn smmu_read_block(&self, d_address: u64, output: &mut [u8]) -> bool;

    /// Apply `operation` to every device address aliasing `host_ptr`.
    ///
    /// Mirrors upstream `DeviceMemoryManager<Traits>::ApplyOpOnPointer` for
    /// core-side users that have a host pointer and need to invalidate the
    /// corresponding Host1x device-memory aliases.
    fn smmu_apply_op_on_host_pointer(
        &self,
        host_ptr: usize,
        scratch: &mut common::scratch_buffer::ScratchBuffer<u32>,
        operation: &mut dyn FnMut(u64),
    ) -> usize;

    /// Bind the callback used after Host1x device-memory writes.
    ///
    /// Upstream `GPU::Impl::BindRenderer` calls
    /// `host1x.MemoryManager().BindInterface(rasterizer)`, and
    /// `DeviceMemoryManager::WriteBlock` then invalidates the written device
    /// range through that rasterizer. The trait keeps this dependency opaque
    /// so `core` does not depend on `video_core`.
    fn bind_device_memory_invalidator(&self, callback: Box<dyn Fn(u64, usize) + Send + Sync>);

    /// Bind the callback used before safe Host1x device-memory reads.
    ///
    /// Upstream `DeviceMemoryManager::ReadBlock` calls
    /// `device_inter->FlushRegion(address, size)` before copying from device
    /// memory. This is the flush half of the same opaque device-interface
    /// ownership represented by `bind_device_memory_invalidator`.
    fn bind_device_memory_flusher(&self, callback: Box<dyn Fn(u64, usize) + Send + Sync>);

    /// Allocate a low 32-bit Host1x GMMU address and map it to an existing
    /// SMMU/device address. Mirrors the `NvMap::PinHandle(low_area_pin)`
    /// path:
    /// `host1x.Allocator().Allocate(size); host1x.GMMU().Map(addr, daddr, size)`.
    fn host1x_gmmu_map_low(&self, d_address: u64, size: usize) -> u32;

    /// Undo `host1x_gmmu_map_low`, mirroring `NvMap::UnmapHandle`:
    /// `host1x.GMMU().Unmap(addr, size); host1x.Allocator().Free(addr, size)`.
    fn host1x_gmmu_unmap_low(&self, gpu_address: u32, size: usize);

    fn start_device(&self, fd: i32, channel_type: Host1xChannelType, syncpt: u32);
    fn stop_device(&self, fd: i32, channel_type: Host1xChannelType);
    fn push_entries(&self, fd: i32, entries: Vec<u32>);
}
