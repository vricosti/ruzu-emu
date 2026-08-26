// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/video_core/renderer_null/null_rasterizer.h and null_rasterizer.cpp
//! Status: COMPLET
//!
//! Null rasterizer — all drawing and rendering operations are no-ops.
//! Functional methods (query, signal_fence, sync_operation, signal_sync_point)
//! still perform their required side effects.

use std::sync::Arc;

use log::trace;

use crate::cache_types::CacheType;
use crate::control::channel_state::ChannelState;
use crate::control::channel_state_cache::{ChannelInfo, ChannelSetupCaches};
use crate::engines::maxwell_dma::{dma, AccelerateDMAInterface};
use crate::host1x::syncpoint_manager::SyncpointManager;
use crate::query_cache::types::QueryPropertiesFlags;
use crate::rasterizer_interface::{RasterizerDownloadArea, RasterizerInterface};

// ── AccelerateDMA ──────────────────────────────────────────────────────────

/// Null DMA accelerator — claims all DMA operations succeed without doing work.
///
/// Corresponds to zuyu's `Null::AccelerateDMA`.
pub struct AccelerateDMA;

impl AccelerateDMA {
    pub fn new() -> Self {
        Self
    }

    /// Pretend buffer copy succeeded.
    pub fn buffer_copy(&self, _start_address: u64, _end_address: u64, _amount: u64) -> bool {
        true
    }

    /// Pretend buffer clear succeeded.
    pub fn buffer_clear(&self, _src_address: u64, _amount: u64, _value: u32) -> bool {
        true
    }

    /// Image-to-buffer copy: not accelerated in null backend.
    pub fn image_to_buffer(&self) -> bool {
        false
    }

    /// Buffer-to-image copy: not accelerated in null backend.
    pub fn buffer_to_image(&self) -> bool {
        false
    }
}

impl Default for AccelerateDMA {
    fn default() -> Self {
        Self::new()
    }
}

impl AccelerateDMAInterface for AccelerateDMA {
    fn buffer_copy(&mut self, start_address: u64, end_address: u64, amount: u64) -> bool {
        AccelerateDMA::buffer_copy(self, start_address, end_address, amount)
    }

    fn buffer_clear(&mut self, src_address: u64, amount: u64, value: u32) -> bool {
        AccelerateDMA::buffer_clear(self, src_address, amount, value)
    }

    fn image_to_buffer(
        &mut self,
        _copy_info: &dma::ImageCopy,
        _src: &dma::ImageOperand,
        _dst: &dma::BufferOperand,
    ) -> bool {
        false
    }

    fn buffer_to_image(
        &mut self,
        _copy_info: &dma::ImageCopy,
        _src: &dma::BufferOperand,
        _dst: &dma::ImageOperand,
    ) -> bool {
        false
    }
}

// ── RasterizerNull ─────────────────────────────────────────────────────────

/// Null rasterizer — all rendering operations are no-ops.
///
/// Corresponds to zuyu's `Null::RasterizerNull`.
/// Implements [`RasterizerInterface`] with stub implementations.
/// Functional side effects (queries, fences, syncpoints) are preserved.
/// GPU virtual address → CPU virtual address translator.
///
/// Calls into the active GPU channel's `MemoryManager::gpu_to_cpu_address`.
/// Returns `None` if the GPU VA is not currently mapped in the channel's
/// page table.
pub type GpuToCpuTranslator = Arc<dyn Fn(u64) -> Option<u64> + Send + Sync>;

pub struct RasterizerNull {
    syncpoints: Arc<SyncpointManager>,
    accelerate_dma: AccelerateDMA,
    channel_caches: ChannelSetupCaches<ChannelInfo>,
    guest_memory_writer: Option<crate::renderer_base::GuestMemoryWriter>,
    gpu_ticks_getter: Option<crate::renderer_base::GpuTicksGetter>,
    /// Translates GPU VAs to CPU VAs. Upstream's `RasterizerNull::Query`
    /// calls `gpu_memory->Write<u64>(gpu_addr, ...)` where `gpu_memory`
    /// is a `Tegra::MemoryManager` that handles GPU VA → CPU VA
    /// translation internally. Ruzu's `guest_memory_writer` expects a
    /// CPU VA, so we must translate first via this hook. Without it,
    /// addresses (the GPU VA being passed verbatim to write_block).
    gpu_to_cpu: Option<GpuToCpuTranslator>,
    #[cfg(test)]
    inline_upload_callback: Option<Box<dyn FnMut(u64, usize, &[u8]) + Send>>,
    #[cfg(test)]
    surface_copy_succeeds: bool,
}

impl RasterizerNull {
    pub fn new(syncpoints: Arc<SyncpointManager>) -> Self {
        Self {
            syncpoints,
            accelerate_dma: AccelerateDMA::new(),
            channel_caches: ChannelSetupCaches::new(),
            guest_memory_writer: None,
            gpu_ticks_getter: None,
            gpu_to_cpu: None,
            #[cfg(test)]
            inline_upload_callback: None,
            #[cfg(test)]
            surface_copy_succeeds: true,
        }
    }

    /// Access the DMA accelerator.
    pub fn access_accelerate_dma(&mut self) -> &mut AccelerateDMA {
        &mut self.accelerate_dma
    }

    pub fn set_guest_memory_writer(&mut self, writer: crate::renderer_base::GuestMemoryWriter) {
        self.guest_memory_writer = Some(writer);
    }

    /// Register a GPU VA → CPU VA translator. See [`GpuToCpuTranslator`].
    pub fn set_gpu_to_cpu_translator(&mut self, translator: GpuToCpuTranslator) {
        self.gpu_to_cpu = Some(translator);
    }

    pub fn set_gpu_ticks_getter(&mut self, getter: crate::renderer_base::GpuTicksGetter) {
        self.gpu_ticks_getter = Some(getter);
    }

    #[cfg(test)]
    pub(crate) fn set_inline_upload_callback(
        &mut self,
        callback: impl FnMut(u64, usize, &[u8]) + Send + 'static,
    ) {
        self.inline_upload_callback = Some(Box::new(callback));
    }

    #[cfg(test)]
    pub(crate) fn set_surface_copy_succeeds(&mut self, succeeds: bool) {
        self.surface_copy_succeeds = succeeds;
    }
}

impl RasterizerInterface for RasterizerNull {
    // ── Drawing (all no-ops) ────────────────────────────────────────────

    fn draw(
        &mut self,
        _draw_view: crate::engines::draw_manager::Maxwell3DDrawView<'_>,
        _instance_count: u32,
    ) {
        trace!("RasterizerNull::draw (no-op)");
    }

    fn draw_texture(
        &mut self,
        _draw_texture_view: crate::engines::draw_manager::Maxwell3DDrawTextureView<'_>,
    ) {
        trace!("RasterizerNull::draw_texture (no-op)");
    }

    fn clear(
        &mut self,
        _clear_view: crate::engines::draw_manager::Maxwell3DClearView<'_>,
        _layer_count: u32,
    ) {
        trace!("RasterizerNull::clear (no-op)");
    }

    fn dispatch_compute(&mut self, _dispatch: &crate::engines::kepler_compute::DispatchCall) {
        trace!("RasterizerNull::dispatch_compute (no-op)");
    }

    // ── Queries ─────────────────────────────────────────────────────────

    fn reset_counter(&mut self, _query_type: u32) {}

    /// Write query result to GPU memory.
    ///
    /// Matches zuyu: if `has_timeout` is true, writes a u64 ticks value at
    /// gpu_addr+8 and the payload as u64 at gpu_addr. Otherwise writes
    /// payload as u32 at gpu_addr.
    fn query(
        &mut self,
        gpu_addr: u64,
        _query_type: u32,
        flags: QueryPropertiesFlags,
        payload: u32,
        _subreport: u32,
    ) {
        let Some(gpu_write) = self.guest_memory_writer.as_ref().cloned() else {
            return;
        };
        // Translate GPU VA → CPU VA via the channel's MemoryManager,
        // matching upstream's `gpu_memory->Write<u64>(gpu_addr, ...)`
        // semantics. Without translation, the puller's GPU-VA argument
        // is passed straight to write_block (which expects CPU VA) and
        // we get "Unmapped WriteBlock" on what looks like a high
        // 36-bit address (the unmapped GPU VA).
        let translate =
            |gpu_va: u64| -> Option<u64> { self.gpu_to_cpu.as_ref().and_then(|f| f(gpu_va)) };
        let has_timeout = flags.contains(QueryPropertiesFlags::HAS_TIMEOUT);
        if has_timeout {
            let gpu_ticks = self
                .gpu_ticks_getter
                .as_ref()
                .map(|getter| getter())
                .unwrap_or(0);
            if let Some(cpu) = translate(gpu_addr + 8) {
                gpu_write(cpu, &gpu_ticks.to_le_bytes());
            } else {
                log::error!(
                    "RasterizerNull::query: GPU VA 0x{:X}+8 has no CPU mapping (ticks write skipped)",
                    gpu_addr
                );
            }
            if let Some(cpu) = translate(gpu_addr) {
                gpu_write(cpu, &(payload as u64).to_le_bytes());
            } else {
                log::error!(
                    "RasterizerNull::query: GPU VA 0x{:X} has no CPU mapping (payload write skipped)",
                    gpu_addr
                );
            }
        } else if let Some(cpu) = translate(gpu_addr) {
            gpu_write(cpu, &payload.to_le_bytes());
        } else {
            log::error!(
                "RasterizerNull::query: GPU VA 0x{:X} has no CPU mapping (u32 payload write skipped)",
                gpu_addr
            );
        }
    }

    // ── Uniform buffers (no-ops) ────────────────────────────────────────

    fn bind_graphics_uniform_buffer(
        &mut self,
        _stage: usize,
        _index: u32,
        _gpu_addr: u64,
        _size: u32,
    ) {
    }

    fn disable_graphics_uniform_buffer(&mut self, _stage: usize, _index: u32) {}

    // ── Synchronization ─────────────────────────────────────────────────

    /// Execute fence callback immediately (null backend has no GPU latency).
    fn signal_fence(&mut self, func: Box<dyn FnOnce() + Send>) {
        func();
    }

    /// Execute sync operation immediately.
    fn sync_operation(&mut self, func: Box<dyn FnOnce() + Send>) {
        func();
    }

    /// Increment the syncpoint value.
    ///
    /// Matches zuyu's `RasterizerNull::SignalSyncPoint()` which increments
    /// both guest and host syncpoints through Host1x.
    fn signal_sync_point(&mut self, id: u32) {
        self.syncpoints.increment_guest(id);
        self.syncpoints.increment_host(id);
    }

    fn signal_reference(&mut self) {}

    fn release_fences(&mut self, _force: bool) {}

    // ── Cache management (no-ops) ───────────────────────────────────────

    fn flush_all(&mut self) {}

    fn flush_region(&mut self, _addr: u64, _size: u64, _which: CacheType) {}

    fn must_flush_region(&self, _addr: u64, _size: u64, _which: CacheType) -> bool {
        false
    }

    /// Get the flush area for a given address range, aligned to page boundaries.
    fn get_flush_area(&self, addr: u64, size: u64) -> RasterizerDownloadArea {
        const DEVICE_PAGESIZE: u64 = 4096;
        RasterizerDownloadArea {
            start_address: addr & !(DEVICE_PAGESIZE - 1),
            end_address: (addr + size + DEVICE_PAGESIZE - 1) & !(DEVICE_PAGESIZE - 1),
            preemptive: true,
        }
    }

    fn invalidate_region(&mut self, _addr: u64, _size: u64, _which: CacheType) {}

    fn on_cache_invalidation(&mut self, _addr: u64, _size: u64) {}

    fn on_cpu_write(&mut self, _addr: u64, _size: u64) -> bool {
        false
    }

    fn invalidate_gpu_cache(&mut self) {}

    fn unmap_memory(&mut self, _addr: u64, _size: u64) {}

    fn modify_gpu_memory(&mut self, _as_id: usize, _addr: u64, _size: u64) {}

    fn flush_and_invalidate_region(&mut self, _addr: u64, _size: u64, _which: CacheType) {}

    // ── Barriers / misc (no-ops) ────────────────────────────────────────

    fn wait_for_idle(&mut self) {}

    fn fragment_barrier(&mut self) {}

    fn tiled_cache_barrier(&mut self) {}

    fn flush_commands(&mut self) {}

    fn tick_frame(&mut self) {}

    // ── Acceleration ────────────────────────────────────────────────────

    /// Pretend surface copy succeeded.
    fn accelerate_surface_copy(
        &mut self,
        _src: &crate::engines::fermi_2d::Surface,
        _dst: &crate::engines::fermi_2d::Surface,
        _copy_config: &crate::engines::fermi_2d::Config,
    ) -> bool {
        #[cfg(test)]
        return self.surface_copy_succeeds;

        #[cfg(not(test))]
        true
    }

    fn accelerate_inline_to_memory(&mut self, _address: u64, _copy_size: usize, _memory: &[u8]) {
        #[cfg(test)]
        if let Some(callback) = self.inline_upload_callback.as_mut() {
            callback(_address, _copy_size, _memory);
        }
    }

    fn access_accelerate_dma(&mut self) -> &mut dyn AccelerateDMAInterface {
        &mut self.accelerate_dma
    }

    // ── Channel management ──────────────────────────────────────────────

    fn initialize_channel(&mut self, channel: &mut ChannelState) {
        self.channel_caches.create_channel(channel);
    }

    fn bind_channel(&mut self, channel: &mut ChannelState) {
        self.channel_caches.bind_to_channel(channel.bind_id);
    }

    fn release_channel(&mut self, channel_id: i32) {
        self.channel_caches.erase_channel(channel_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rasterizer_null_noop() {
        let sp = Arc::new(SyncpointManager::new());
        let mut rast = RasterizerNull::new(sp);

        let ds = crate::engines::draw_manager::DrawState::default();
        rast.draw(
            crate::engines::draw_manager::Maxwell3DDrawView::new(&ds, false),
            1,
        );
        rast.draw(
            crate::engines::draw_manager::Maxwell3DDrawView::new(&ds, false),
            4,
        );
        rast.draw_texture(crate::engines::draw_manager::Maxwell3DDrawTextureView::new(
            &ds,
            crate::engines::draw_manager::DrawTextureState::default(),
        ));
        rast.clear(
            crate::engines::draw_manager::Maxwell3DClearView::new(
                crate::engines::draw_manager::ClearState::default(),
                crate::engines::draw_manager::Maxwell3DRenderTargets::default(),
            ),
            1,
        );
        rast.dispatch_compute(&crate::engines::kepler_compute::DispatchCall::default());
        rast.flush_all();
        rast.wait_for_idle();
        rast.tick_frame();
        assert!(!rast.must_flush_region(0, 0, CacheType::ALL));
        assert!(!rast.on_cpu_write(0, 0));
        assert!(rast.accelerate_surface_copy(
            &crate::engines::fermi_2d::Surface::default(),
            &crate::engines::fermi_2d::Surface::default(),
            &crate::engines::fermi_2d::Config::default(),
        ));
    }

    #[test]
    fn test_signal_sync_point() {
        let sp = Arc::new(SyncpointManager::new());
        let mut rast = RasterizerNull::new(sp.clone());
        rast.signal_sync_point(1);
        assert_eq!(sp.get_guest_syncpoint_value(1), 1);
        assert_eq!(sp.get_host_syncpoint_value(1), 1);
    }

    #[test]
    fn test_signal_fence_executes_immediately() {
        let sp = Arc::new(SyncpointManager::new());
        let mut rast = RasterizerNull::new(sp);

        let executed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = executed.clone();
        rast.signal_fence(Box::new(move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }));
        assert!(executed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn test_query_without_timeout() {
        let sp = Arc::new(SyncpointManager::new());
        let mut rast = RasterizerNull::new(sp);

        let written = Arc::new(std::sync::Mutex::new(Vec::new()));
        let written_cb = Arc::clone(&written);
        rast.set_guest_memory_writer(Arc::new(move |addr, data| {
            written_cb.lock().unwrap().push((addr, data.to_vec()));
        }));
        rast.set_gpu_to_cpu_translator(Arc::new(Some));
        rast.query(0x1000, 0, QueryPropertiesFlags::empty(), 42, 0);

        let w = written.lock().unwrap();
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].0, 0x1000);
        assert_eq!(w[0].1, 42u32.to_le_bytes().to_vec());
    }

    #[test]
    fn test_query_with_timeout() {
        let sp = Arc::new(SyncpointManager::new());
        let mut rast = RasterizerNull::new(sp);

        let written = Arc::new(std::sync::Mutex::new(Vec::new()));
        let written_cb = Arc::clone(&written);
        rast.set_guest_memory_writer(Arc::new(move |addr, data| {
            written_cb.lock().unwrap().push((addr, data.to_vec()));
        }));
        rast.set_gpu_to_cpu_translator(Arc::new(Some));
        rast.set_gpu_ticks_getter(Arc::new(|| 0x1234_5678_9ABC_DEF0));
        rast.query(0x2000, 0, QueryPropertiesFlags::HAS_TIMEOUT, 99, 0);

        let w = written.lock().unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].0, 0x2008);
        assert_eq!(w[0].1, 0x1234_5678_9ABC_DEF0u64.to_le_bytes().to_vec());
        assert_eq!(w[1].0, 0x2000);
        assert_eq!(w[1].1, 99u64.to_le_bytes().to_vec());
    }

    #[test]
    fn test_query_non_payload_preserves_payload() {
        let sp = Arc::new(SyncpointManager::new());
        let mut rast = RasterizerNull::new(sp);

        let written = Arc::new(std::sync::Mutex::new(Vec::new()));
        let written_cb = Arc::clone(&written);
        rast.set_guest_memory_writer(Arc::new(move |addr, data| {
            written_cb.lock().unwrap().push((addr, data.to_vec()));
        }));
        rast.set_gpu_to_cpu_translator(Arc::new(Some));
        rast.query(0x3000, 2, QueryPropertiesFlags::empty(), 0xDEAD_BEEF, 0);

        let w = written.lock().unwrap();
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].0, 0x3000);
        assert_eq!(w[0].1, 0xDEAD_BEEFu32.to_le_bytes().to_vec());
    }

    #[test]
    fn test_get_flush_area_alignment() {
        let sp = Arc::new(SyncpointManager::new());
        let rast = RasterizerNull::new(sp);

        let area = rast.get_flush_area(0x1234, 0x100);
        assert_eq!(area.start_address, 0x1000);
        assert_eq!(area.end_address, 0x2000);
        assert!(area.preemptive);
    }

    #[test]
    fn test_accelerate_dma() {
        let dma = AccelerateDMA::new();
        assert!(dma.buffer_copy(0, 0x1000, 0x1000));
        assert!(dma.buffer_clear(0, 0x1000, 0));
        assert!(!dma.image_to_buffer());
        assert!(!dma.buffer_to_image());
    }

    #[test]
    fn test_channel_lifecycle_updates_channel_caches() {
        use parking_lot::Mutex;

        let sp = Arc::new(SyncpointManager::new());
        let mut rast = RasterizerNull::new(sp);
        let gpu = crate::gpu::Gpu::new(false, false);
        let mut channel = ChannelState::new(9);
        let memory_manager = Arc::new(Mutex::new(
            crate::memory_manager::MemoryManager::new_with_geometry(3, 32, 0x1_0000_0000, 17, 12),
        ));
        channel.memory_manager = Some(Arc::clone(&memory_manager));
        channel.init(&gpu, 0x1234);

        rast.initialize_channel(&mut channel);
        rast.bind_channel(&mut channel);

        assert_eq!(rast.channel_caches.program_id, 0x1234);
        assert!(Arc::ptr_eq(
            rast.channel_caches
                .gpu_memory
                .as_ref()
                .expect("bound GPU memory"),
            &memory_manager,
        ));

        rast.release_channel(9);
        assert_eq!(rast.channel_caches.program_id, 0);
        assert!(rast.channel_caches.gpu_memory.is_none());
    }

    #[test]
    fn test_trait_object() {
        let sp = Arc::new(SyncpointManager::new());
        let mut rast: Box<dyn RasterizerInterface> = Box::new(RasterizerNull::new(sp));

        // Should work through the trait object
        let ds = crate::engines::draw_manager::DrawState::default();
        rast.draw(
            crate::engines::draw_manager::Maxwell3DDrawView::new(&ds, false),
            1,
        );
        rast.clear(
            crate::engines::draw_manager::Maxwell3DClearView::new(
                crate::engines::draw_manager::ClearState::default(),
                crate::engines::draw_manager::Maxwell3DRenderTargets::default(),
            ),
            1,
        );
        rast.flush_all();
        rast.tick_frame();
    }
}
