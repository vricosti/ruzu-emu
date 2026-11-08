// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Opaque GPU-core bridge used by `core` owners that must talk to the
//! frontend-provided GPU implementation without depending on `video_core`.

use std::any::Any;
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
