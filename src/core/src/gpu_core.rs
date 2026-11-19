// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Opaque GPU-core bridge used by `core` owners that must talk to the
//! frontend-provided GPU implementation without depending on `video_core`.

use std::any::Any;
use std::sync::atomic::{fence, AtomicU64, Ordering};
use std::sync::Arc;

use common::math_util::Rectangle;

use crate::hle::service::nvdrv::nvdata::NvFence;
use crate::hle::service::nvnflinger::buffer_transform_flags::BufferTransformFlags;
use crate::hle::service::nvnflinger::pixel_format::PixelFormat;

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct GpuCommandListHeader {
    pub raw: u64,
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct GpuCommandHeader {
    pub raw: u32,
}

#[derive(Debug, Clone, Default)]
pub struct GpuCommandList {
    pub command_lists: Vec<GpuCommandListHeader>,
    pub prefetch_command_list: Vec<GpuCommandHeader>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendMode {
    #[default]
    Opaque,
    Premultiplied,
    Coverage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramebufferConfig {
    pub address: u64,
    pub offset: u32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_format: PixelFormat,
    pub transform_flags: BufferTransformFlags,
    pub crop_rect: Rectangle<i32>,
    pub blending: BlendMode,
    pub native_tiling: bool,
    // Mirrors Tegra::FramebufferConfig across the core/video_core crate boundary.
    pub layer_stack_mask: u32,
}

impl Default for FramebufferConfig {
    fn default() -> Self {
        Self {
            address: 0,
            offset: 0,
            width: 0,
            height: 0,
            stride: 0,
            pixel_format: PixelFormat::default(),
            transform_flags: BufferTransformFlags::default(),
            crop_rect: Rectangle::default(),
            blending: BlendMode::default(),
            native_tiling: false,
            layer_stack_mask: crate::hle::service::nvnflinger::hwc_layer::DEFAULT_LAYER_STACK_MASK,
        }
    }
}

/// Mirrors upstream `VideoCore::RasterizerDownloadArea` across the `core` /
/// `video_core` crate boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RasterizerDownloadArea {
    pub start_address: u64,
    pub end_address: u64,
    pub preemptive: bool,
}

/// What [`DownloadLanding`] publishes.
///
/// Lichii `VideoCore::DownloadLandingState`
/// (`src/video_core/rasterizer_download_area.h:44-51`). Lichii declares it in
/// `video_core`, and `core/memory.cpp` includes that header; `core` cannot
/// depend on the `video_core` crate here, so it lives in this cross-crate
/// bridge and `video_core::rasterizer_download_area` re-exports it.
pub struct DownloadLandingState {
    /// Odd while a landing is published.
    sequence: AtomicU64,
    begin: AtomicU64,
    end: AtomicU64,
    waits: AtomicU64,
}

/// Lichii `DownloadLanding::state` (one per process, as the C++ static).
static DOWNLOAD_LANDING_STATE: DownloadLandingState = DownloadLandingState {
    sequence: AtomicU64::new(0),
    begin: AtomicU64::new(0),
    end: AtomicU64::new(0),
    waits: AtomicU64::new(0),
};

/// The buffer cache writing downloaded GPU bytes to guest memory `[begin, end)`.
///
/// Lichii `VideoCore::DownloadLanding` (`rasterizer_download_area.h:53-116`).
/// A download keeps the bytes of the guest CPU writes collected and not yet
/// applied when it looks at them (`BufferCache::pending_cpu_write_ranges`). A
/// guest core collects a write (`GpuDirtyMemoryManager::collect`) and stores
/// its bytes right after, without the buffer cache's lock: a write collected
/// after the download looked would have its bytes written over with the GPU's
/// older ones. Downloads look only inside a landing, and a write collected in
/// the landing's range stores its bytes once the landing has ended
/// ([`DownloadLanding::r#await`], from `Memory`'s rasterizer write handling).
///
/// Each side fences between its store (the landing, the collected write) and
/// its load (the collected writes, the landing): either the download finds the
/// write, or the write finds the landing. Downloads land under the buffer
/// cache's lock, so one landing is published at a time.
pub struct DownloadLanding {
    _published: (),
}

impl DownloadLanding {
    pub fn new(begin: u64, end: u64) -> Self {
        let state = &DOWNLOAD_LANDING_STATE;
        state.begin.store(begin, Ordering::Relaxed);
        state.end.store(end, Ordering::Relaxed);
        state.sequence.fetch_add(1, Ordering::SeqCst);
        fence(Ordering::SeqCst);
        Self { _published: () }
    }

    /// A guest CPU write of `[addr, addr + size)` was collected: returns once
    /// its bytes may be stored. Lichii `DownloadLanding::Await` (`await` is a
    /// Rust keyword, hence the raw identifier).
    pub fn r#await(addr: u64, size: u64) {
        fence(Ordering::SeqCst);
        let sequence = DOWNLOAD_LANDING_STATE.sequence.load(Ordering::Acquire);
        if (sequence & 1) != 0 {
            Self::wait(sequence, addr, size);
        }
    }

    /// Writes that waited for a landing so far.
    pub fn waits() -> u64 {
        DOWNLOAD_LANDING_STATE.waits.load(Ordering::Relaxed)
    }

    fn wait(sequence: u64, addr: u64, size: u64) {
        let state = &DOWNLOAD_LANDING_STATE;
        let begin = state.begin.load(Ordering::Relaxed);
        let end = state.end.load(Ordering::Relaxed);
        fence(Ordering::Acquire);
        // Ended (its writes come first, and the landings after it found this
        // write), or it writes elsewhere.
        if state.sequence.load(Ordering::Acquire) != sequence
            || addr >= end
            || addr.wrapping_add(size) <= begin
        {
            return;
        }
        state.waits.fetch_add(1, Ordering::Relaxed);
        while state.sequence.load(Ordering::Acquire) == sequence {
            std::thread::yield_now();
        }
    }
}

impl Drop for DownloadLanding {
    fn drop(&mut self) {
        DOWNLOAD_LANDING_STATE.sequence.fetch_add(1, Ordering::Release);
    }
}

/// Opaque GPU memory-manager state handle.
pub trait GpuMemoryManagerHandle: Any + Send + Sync {
    fn as_any(&self) -> &(dyn Any + Send + Sync);

    fn set_native_syncpoint_shim(&self, address: u64);
    /// Bridge the constant owned by video_core::MemoryManager.
    fn num_shim_syncpoints(&self) -> u32;

    /// Mirrors upstream `Tegra::MemoryManager::Map(...)`.
    fn map(&self, gpu_addr: u64, device_addr: u64, size: u64, kind: u32, is_big_pages: bool);

    /// Mirrors upstream `Tegra::MemoryManager::MapSparse(...)`.
    fn map_sparse(&self, gpu_addr: u64, size: u64, is_big_pages: bool);

    /// Mirrors upstream `Tegra::MemoryManager::Unmap(...)`.
    fn unmap(&self, gpu_addr: u64, size: u64);
}

/// Opaque per-channel GPU state handle.
///
/// This bridges the upstream ownership path
/// `system.GPU().AllocateChannel() -> ChannelState` into the Rust split-crate
/// layout where `core` cannot name `video_core` types directly.
pub trait GpuChannelHandle: Send + Sync {
    /// Mirrors the upstream `channel_state->memory_manager = gmmu` write.
    fn bind_memory_manager(&self, memory_manager: Arc<dyn GpuMemoryManagerHandle>);

    /// Mirrors the upstream `system.GPU().InitChannel(*channel_state, program_id)`.
    fn init_channel(&self, program_id: u64);

    /// Mirrors the upstream `channel_state->bind_id`.
    fn bind_id(&self) -> i32;
}

/// Opaque GPU implementation owned by [`crate::core::System`].
pub trait GpuCoreInterface: Any + Send {
    /// Mirrors upstream GPU::RunInOrder; callback ownership moves to the GPU queue.
    fn run_in_order(&self, func: Box<dyn FnOnce() + Send>);

    /// GPU::RunOnGpuThread: enqueue between command lists, returning its sync fence.
    fn run_on_gpu_thread(&self, action: Box<dyn FnOnce() + Send>) -> u64;

    /// GPU::Renderer().ReadRasterizer()->FlushRegion, for callbacks already
    /// executing on the GPU thread; does not enqueue or wait for another task.
    fn renderer_flush_region(&self, addr: u64, size: u64);

    /// Bridges the free functions in video_core/nx2.h without moving their state.
    /// Bridge the upstream global assignment; state remains in video_core/nx2.rs.
    fn set_nx2_ampere(&self, enabled: bool);

    fn nx2_add_semaphore_region(&self, start: u64);
    fn nx2_remove_semaphore_region(&self, start: u64);

    fn as_any(&self) -> &(dyn Any + Send);

    /// Mirrors the upstream `GPU::AllocateChannel()`.
    fn allocate_channel_handle(&self) -> Arc<dyn GpuChannelHandle>;

    /// Mirrors the upstream `std::make_shared<Tegra::MemoryManager>(...)` owner
    /// created by `nvhost_as_gpu`.
    fn allocate_memory_manager_handle(
        &self,
        address_space_bits: u64,
        split_address: u64,
        big_page_bits: u64,
        page_bits: u64,
    ) -> Arc<dyn GpuMemoryManagerHandle>;

    /// Mirrors the upstream `GPU::InitAddressSpace(Tegra::MemoryManager&)`.
    fn init_address_space(&self, memory_manager: Arc<dyn GpuMemoryManagerHandle>);

    /// Mirrors the upstream `GPU::PushGPUEntries(s32, CommandList&&)`.
    fn push_gpu_entries(&self, channel_id: i32, entries: GpuCommandList);

    /// Mirrors the upstream `GPU::RequestComposite(layers, fences)`.
    fn request_composite(&self, layers: Vec<FramebufferConfig>, fences: Vec<NvFence>);

    /// Bridges GPU::Renderer().RequestScreenshot across the crate boundary.
    /// The renderer owns the pixels until completion and returns them to the caller.
    fn request_screenshot(
        &self,
        pixels: Vec<u8>,
        callback: Box<dyn FnOnce(Vec<u8>, bool) + Send>,
        layout: crate::frontend::framebuffer_layout::FramebufferLayout,
        layer_stack: crate::hle::service::nvnflinger::hwc_layer::LayerStackId,
    );

    /// Mirrors the upstream `GPU::WaitForComposite()`.
    fn wait_for_composite(&self);

    /// Mirrors the upstream `GPU::NotifyShutdown()`.
    fn notify_shutdown(&self) {}

    /// Mirrors the upstream `GPU::ObtainContext()` call made by the
    /// synchronous single-core CPU thread.
    fn obtain_context(&self) {}

    /// Mirrors upstream `GPU::ReleaseContext()` for frontend teardown.
    fn release_context(&self) {}

    /// Bridges `GPU::Renderer().GetDeviceVendor()` across the split crates.
    fn get_device_vendor(&self) -> String {
        String::new()
    }

    /// Mirrors the upstream `GPU::OnCPUWrite(DAddr, u64)`.
    fn on_cpu_write(&self, addr: u64, size: u64) -> bool;

    /// Mirrors the upstream `GPU::OnCPURead(DAddr, u64)`.
    fn on_cpu_read(&self, addr: u64, size: u64) -> RasterizerDownloadArea;

    /// Mirrors the upstream `GPU::FlushRegion(DAddr, u64)`.
    fn flush_region(&self, addr: u64, size: u64);

    /// Mirrors the upstream `GPU::InvalidateRegion(DAddr, u64)`.
    fn invalidate_region(&self, _addr: u64, _size: u64) {}

    /// Mirrors the upstream `GPU::GetAppletCaptureBuffer()`.
    fn get_applet_capture_buffer(&self) -> Vec<u8> {
        Vec::new()
    }
}
