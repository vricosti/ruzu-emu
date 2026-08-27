// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/video_core/rasterizer_interface.h
//! Status: COMPLET
//!
//! Abstract interface for GPU rasterizer backends. Each renderer
//! (Null, OpenGL, Vulkan) provides its own implementation.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::cache_types::CacheType;
use crate::control::channel_state::ChannelState;
use crate::engines::draw_manager::{Maxwell3DClearView, Maxwell3DDrawView, Maxwell3DIndirectView};
use crate::engines::fermi_2d::{Config as Fermi2DConfig, Surface as Fermi2DSurface};
use crate::engines::kepler_compute::DispatchCall;
use crate::engines::maxwell_dma::AccelerateDMAInterface;
use crate::query_cache::types::QueryPropertiesFlags;
pub use crate::rasterizer_download_area::RasterizerDownloadArea;

/// Shader loading callback stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadCallbackStage {
    Prepare,
    Build,
    Complete,
}

/// Callback for disk resource loading progress.
pub type DiskResourceLoadCallback =
    Arc<dyn Fn(LoadCallbackStage, usize, usize) + Send + Sync + 'static>;

/// Cancellation state for disk resource loading.
///
/// This is the Rust owner-equivalent of upstream's copied `std::stop_token`.
pub type DiskResourceLoadStop = Arc<AtomicBool>;

/// Inert DMA owner used only by unit-test rasterizers that never exercise DMA.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestAccelerateDMA;

#[cfg(test)]
impl AccelerateDMAInterface for TestAccelerateDMA {
    fn buffer_copy(&mut self, _src_address: u64, _dest_address: u64, _amount: u64) -> bool {
        false
    }

    fn buffer_clear(&mut self, _dst_address: u64, _amount: u64, _value: u32) -> bool {
        false
    }

    fn image_to_buffer(
        &mut self,
        _copy_info: &crate::engines::maxwell_dma::dma::ImageCopy,
        _src: &crate::engines::maxwell_dma::dma::ImageOperand,
        _dst: &crate::engines::maxwell_dma::dma::BufferOperand,
    ) -> bool {
        false
    }

    fn buffer_to_image(
        &mut self,
        _copy_info: &crate::engines::maxwell_dma::dma::ImageCopy,
        _src: &crate::engines::maxwell_dma::dma::BufferOperand,
        _dst: &crate::engines::maxwell_dma::dma::ImageOperand,
    ) -> bool {
        false
    }
}

/// Non-owning rasterizer pointer matching upstream `VideoCore::RasterizerInterface*`.
///
/// Upstream stores raw rasterizer pointers in GPU engines and relies on the
/// renderer/channel lifetime graph to keep them valid. Rust cannot express that
/// lifetime in the current owner graph, so the lifetime erasure is centralized
/// here instead of being reimplemented by each engine.
#[derive(Clone, Copy)]
pub struct RasterizerHandle {
    ptr: *const (dyn RasterizerInterface + 'static),
}

// Safety: this is a non-owning pointer equivalent to upstream's
// `RasterizerInterface*`. It does not provide synchronization or ownership; the
// renderer/channel graph must keep the rasterizer alive and serialize access.
unsafe impl Send for RasterizerHandle {}
unsafe impl Sync for RasterizerHandle {}

impl RasterizerHandle {
    pub fn from_ref(rasterizer: &dyn RasterizerInterface) -> Self {
        let raw = rasterizer as *const dyn RasterizerInterface;
        let ptr = unsafe {
            std::mem::transmute::<
                *const dyn RasterizerInterface,
                *const (dyn RasterizerInterface + 'static),
            >(raw)
        };
        Self { ptr }
    }

    pub fn as_ptr(self) -> *const (dyn RasterizerInterface + 'static) {
        self.ptr
    }

    pub fn debug_data_ptr(self) -> *const () {
        self.ptr as *const ()
    }

    /// # Safety
    ///
    /// The caller must guarantee the pointed rasterizer is still alive and that
    /// no aliasing mutable access exists for the duration of the returned
    /// borrow. This is the Rust expression of upstream's raw-pointer contract.
    pub unsafe fn as_mut<'a>(self) -> &'a mut dyn RasterizerInterface {
        unsafe { &mut *(self.ptr as *mut dyn RasterizerInterface) }
    }

    /// # Safety
    ///
    /// Same safety contract as [`Self::as_mut`].
    pub unsafe fn with_mut<R>(self, f: impl FnOnce(&mut dyn RasterizerInterface) -> R) -> R {
        let rasterizer = unsafe { self.as_mut() };
        f(rasterizer)
    }
}

/// Abstract rasterizer interface — corresponds to zuyu's `VideoCore::RasterizerInterface`.
///
/// Defines the full set of operations a GPU rasterizer must support.
/// Methods with default implementations are optional (matching C++ virtual with body).
/// Methods without defaults are pure virtual (= 0) in the C++.
pub trait RasterizerInterface {
    // ── Drawing ─────────────────────────────────────────────────────────

    /// Dispatch a draw invocation.
    ///
    /// `draw_view` is the draw-boundary Maxwell3D view consumed by the
    /// rasterizer. Upstream reaches this data via the rasterizer's `Maxwell3D*`;
    /// ruzu uses a temporary view so the interface can move toward that model
    /// without storing an unsafe persistent engine pointer in the backend.
    fn draw(&mut self, draw_view: Maxwell3DDrawView<'_>, instance_count: u32);

    /// Dispatch an indirect draw invocation.
    fn draw_indirect(&mut self, _indirect_view: Maxwell3DIndirectView<'_>) {}

    /// Dispatch a draw texture invocation.
    fn draw_texture(
        &mut self,
        draw_texture_view: crate::engines::draw_manager::Maxwell3DDrawTextureView<'_>,
    );

    /// Clear the current framebuffer.
    fn clear(&mut self, clear_view: Maxwell3DClearView<'_>, layer_count: u32);

    /// Dispatch a compute shader invocation.
    ///
    /// Upstream `RasterizerOpenGL::DispatchCompute` reads the currently bound
    /// `KeplerCompute` owner directly. Rust passes the already-recorded dispatch
    /// snapshot to avoid re-entering `KeplerCompute` mutably while it is calling
    /// into the rasterizer.
    fn dispatch_compute(&mut self, dispatch: &DispatchCall);

    // ── Queries ──────────────────────────────────────────────────────────

    /// Reset the counter of a query.
    fn reset_counter(&mut self, query_type: u32);

    /// Record a GPU query and cache it.
    ///
    /// `gpu_write` writes data to GPU virtual address space.
    fn query(
        &mut self,
        gpu_addr: u64,
        query_type: u32,
        flags: QueryPropertiesFlags,
        payload: u32,
        subreport: u32,
    );

    // ── Uniform buffers ─────────────────────────────────────────────────

    /// Signal a uniform buffer binding.
    fn bind_graphics_uniform_buffer(&mut self, stage: usize, index: u32, gpu_addr: u64, size: u32);

    /// Signal disabling of a uniform buffer.
    fn disable_graphics_uniform_buffer(&mut self, stage: usize, index: u32);

    // ── Synchronization ─────────────────────────────────────────────────

    /// Signal a GPU-based semaphore as a fence.
    fn signal_fence(&mut self, func: Box<dyn FnOnce() + Send>);

    /// Send an operation to be done after a certain amount of flushes.
    fn sync_operation(&mut self, func: Box<dyn FnOnce() + Send>);

    /// Signal a GPU-based syncpoint as a fence.
    fn signal_sync_point(&mut self, value: u32);

    /// Signal a GPU-based reference point.
    fn signal_reference(&mut self);

    /// Release all pending fences.
    fn release_fences(&mut self, force: bool);

    // ── Cache management ────────────────────────────────────────────────

    /// Flush all caches to Switch memory.
    fn flush_all(&mut self);

    /// Flush caches of the specified region to Switch memory.
    fn flush_region(&mut self, addr: u64, size: u64, which: CacheType);

    /// Check if the specified memory area requires flushing to CPU memory.
    fn must_flush_region(&self, addr: u64, size: u64, which: CacheType) -> bool;

    /// Get the download area for flushing a region.
    fn get_flush_area(&self, addr: u64, size: u64) -> RasterizerDownloadArea;

    /// Invalidate caches of the specified region.
    fn invalidate_region(&mut self, addr: u64, size: u64, which: CacheType);

    /// Invalidate multiple regions at once.
    fn inner_invalidation(&mut self, sequences: &[(u64, usize)]) {
        if *common::settings::values()
            .skip_cpu_inner_invalidation
            .get_value()
        {
            return;
        }
        for &(addr, size) in sequences {
            self.invalidate_region(addr, size as u64, CacheType::ALL);
        }
    }

    /// Notify that caches of the specified region are desynced with guest.
    fn on_cache_invalidation(&mut self, addr: u64, size: u64);

    /// Notify of a CPU write to the specified region.
    fn on_cpu_write(&mut self, addr: u64, size: u64) -> bool;

    /// Sync memory between guest and host.
    fn invalidate_gpu_cache(&mut self);

    /// Unmap a memory range.
    fn unmap_memory(&mut self, addr: u64, size: u64);

    /// Notify that GPU memory backing changed.
    fn modify_gpu_memory(&mut self, as_id: usize, addr: u64, size: u64);

    /// Flush and invalidate caches of the specified region.
    fn flush_and_invalidate_region(&mut self, addr: u64, size: u64, which: CacheType);

    // ── Barriers / sync ─────────────────────────────────────────────────

    /// Wait for previous primitive and compute operations.
    fn wait_for_idle(&mut self);

    /// Wait for reads and writes to render targets and flush caches.
    fn fragment_barrier(&mut self);

    /// Make available previous render target writes.
    fn tiled_cache_barrier(&mut self);

    /// Send all written commands to the host GPU.
    fn flush_commands(&mut self);

    /// Notify that a frame is about to finish.
    fn tick_frame(&mut self);

    // ── Acceleration ────────────────────────────────────────────────────

    /// Attempt conditional rendering acceleration.
    fn accelerate_conditional_rendering(&mut self) -> bool {
        false
    }

    /// Rust ownership adapter for backends whose upstream implementation reads
    /// the condition address from its live Maxwell3D owner.
    fn accelerate_conditional_rendering_with_address(
        &mut self,
        _condition_address: u64,
        _compare_size: u64,
    ) -> bool {
        self.accelerate_conditional_rendering()
    }

    /// Attempt to use a faster method to perform a surface copy.
    fn accelerate_surface_copy(
        &mut self,
        _src: &Fermi2DSurface,
        _dst: &Fermi2DSurface,
        _copy_config: &Fermi2DConfig,
    ) -> bool {
        false
    }

    /// Return the backend-owned DMA accelerator.
    fn access_accelerate_dma(&mut self) -> &mut dyn AccelerateDMAInterface;

    /// Accelerate inline memory write.
    fn accelerate_inline_to_memory(&mut self, address: u64, copy_size: usize, memory: &[u8]);

    // ── Disk resources ──────────────────────────────────────────────────

    /// Initialize disk cached resources for the game being emulated.
    fn load_disk_resources(
        &mut self,
        _title_id: u64,
        _stop_loading: DiskResourceLoadStop,
        _callback: DiskResourceLoadCallback,
    ) {
    }

    // ── Channel management ──────────────────────────────────────────────

    /// Initialize a GPU channel.
    fn initialize_channel(&mut self, _channel: &mut ChannelState) {}

    /// Bind a GPU channel.
    fn bind_channel(&mut self, _channel: &mut ChannelState) {}

    /// Release a GPU channel.
    fn release_channel(&mut self, _channel_id: i32) {}

    // ── Transform feedback ──────────────────────────────────────────────

    /// Register the address as a Transform Feedback Object.
    fn register_transform_feedback(&mut self, _tfb_object_addr: u64) {}

    /// Returns true when the rasterizer has Draw Transform Feedback capabilities.
    fn has_draw_transform_feedback(&self) -> bool {
        false
    }
}
