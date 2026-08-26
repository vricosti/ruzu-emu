// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/video_core/renderer_null/renderer_null.h and renderer_null.cpp
//! Status: COMPLET
//!
//! Null renderer — frame composition is a no-op (no display output).
//! Used for headless testing and benchmarking.

use std::sync::Arc;

use log::trace;

use super::null_rasterizer::RasterizerNull;
use crate::framebuffer_config::FramebufferConfig;
use crate::host1x::syncpoint_manager::SyncpointManager;
use crate::rasterizer_interface::RasterizerInterface;
use crate::renderer_base::{RendererBase, RendererBaseData};

/// Null renderer — corresponds to zuyu's `Null::RendererNull`.
///
/// Extends the renderer base concept with no-op frame composition.
/// Owns a [`RasterizerNull`] for draw call handling.
/// Null graphics context (no-op).
struct NullContext;
impl ruzu_core::frontend::graphics_context::GraphicsContext for NullContext {}

pub struct RendererNull {
    rasterizer: RasterizerNull,
    base_data: RendererBaseData,
    null_context: NullContext,
}

impl RendererNull {
    /// Create a new null renderer.
    pub fn new(syncpoints: Arc<SyncpointManager>) -> Self {
        Self {
            rasterizer: RasterizerNull::new(syncpoints),
            base_data: RendererBaseData::new(),
            null_context: NullContext,
        }
    }

    /// Composite framebuffers — no-op in null renderer.
    ///
    /// Matches zuyu's `RendererNull::Composite()`: increments frame counter
    /// but produces no display output.
    pub fn composite_impl(&mut self, layers: &[FramebufferConfig]) {
        if layers.is_empty() {
            return;
        }
        self.base_data.current_frame += 1;
        trace!(
            "RendererNull::composite frame={}",
            self.base_data.current_frame
        );
    }

    /// Get a zeroed applet capture buffer.
    ///
    /// Matches zuyu: returns `TiledSize` bytes of zeros.
    pub fn get_applet_capture_buffer(&self) -> Vec<u8> {
        vec![0u8; crate::capture::TILED_SIZE as usize]
    }

    /// Access the rasterizer.
    pub fn rasterizer(&self) -> &RasterizerNull {
        &self.rasterizer
    }

    /// Access the rasterizer mutably.
    pub fn rasterizer_mut(&mut self) -> &mut RasterizerNull {
        &mut self.rasterizer
    }

    /// Access the rasterizer as a trait object.
    pub fn read_rasterizer(&mut self) -> &mut dyn RasterizerInterface {
        &mut self.rasterizer
    }

    /// Get the device vendor string.
    pub fn device_vendor(&self) -> &str {
        "NULL"
    }

    /// Get the current frame count.
    pub fn frame_count(&self) -> i32 {
        self.base_data.current_frame
    }
}

impl RendererBase for RendererNull {
    fn context_ptr(&mut self) -> *mut dyn ruzu_core::frontend::graphics_context::GraphicsContext {
        &mut self.null_context as *mut dyn ruzu_core::frontend::graphics_context::GraphicsContext
    }

    fn composite(&mut self, layers: &[FramebufferConfig]) {
        self.composite_impl(layers);
    }

    fn get_applet_capture_buffer(&mut self) -> Vec<u8> {
        RendererNull::get_applet_capture_buffer(self)
    }

    fn read_rasterizer(&self) -> *mut dyn RasterizerInterface {
        let trait_ref: &dyn RasterizerInterface = &self.rasterizer;
        trait_ref as *const dyn RasterizerInterface as *mut dyn RasterizerInterface
    }

    fn get_device_vendor(&self) -> String {
        self.device_vendor().to_string()
    }

    fn current_fps(&self) -> f32 {
        self.base_data.current_fps
    }

    fn current_frame(&self) -> i32 {
        self.base_data.current_frame
    }

    fn refresh_base_settings(&mut self) {}

    fn is_screenshot_pending(&self) -> bool {
        self.base_data.is_screenshot_pending()
    }

    fn request_screenshot(
        &mut self,
        _data: *mut std::ffi::c_void,
        callback: Box<dyn FnOnce(bool) + Send>,
        _layout: ruzu_core::frontend::framebuffer_layout::FramebufferLayout,
    ) {
        callback(false);
    }

    fn set_guest_memory_writer(&mut self, writer: crate::renderer_base::GuestMemoryWriter) {
        self.rasterizer.set_guest_memory_writer(writer);
    }

    fn set_gpu_ticks_getter(&mut self, getter: crate::renderer_base::GpuTicksGetter) {
        self.rasterizer.set_gpu_ticks_getter(getter);
    }

    fn set_gpu_to_cpu_translator(
        &mut self,
        translator: std::sync::Arc<dyn Fn(u64) -> Option<u64> + Send + Sync>,
    ) {
        self.rasterizer.set_gpu_to_cpu_translator(translator);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_fb() -> FramebufferConfig {
        FramebufferConfig {
            address: 0,
            offset: 0,
            width: 1280,
            height: 720,
            stride: 1280,
            pixel_format: crate::framebuffer_config::AndroidPixelFormat(0),
            transform_flags: crate::framebuffer_config::BufferTransformFlags(0),
            crop_rect: crate::framebuffer_config::RectI {
                left: 0,
                top: 0,
                right: 1280,
                bottom: 720,
            },
            blending: crate::framebuffer_config::BlendMode::Opaque,
        }
    }

    #[test]
    fn test_renderer_null_composite() {
        let sp = Arc::new(SyncpointManager::new());
        let mut renderer = RendererNull::new(sp);

        // Empty framebuffer list should be a no-op
        renderer.composite_impl(&[]);
        assert_eq!(renderer.frame_count(), 0);

        // Non-empty should increment
        renderer.composite_impl(&[dummy_fb()]);
        assert_eq!(renderer.frame_count(), 1);

        renderer.composite_impl(&[dummy_fb(), dummy_fb()]);
        assert_eq!(renderer.frame_count(), 2);
    }

    #[test]
    fn test_renderer_null_capture_buffer() {
        let sp = Arc::new(SyncpointManager::new());
        let renderer = RendererNull::new(sp);

        let buf = renderer.get_applet_capture_buffer();
        assert_eq!(buf.len(), crate::capture::TILED_SIZE as usize);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_renderer_null_vendor() {
        let sp = Arc::new(SyncpointManager::new());
        let renderer = RendererNull::new(sp);
        assert_eq!(renderer.device_vendor(), "NULL");
    }

    #[test]
    fn test_renderer_null_rasterizer_access() {
        let sp = Arc::new(SyncpointManager::new());
        let mut renderer = RendererNull::new(sp);

        // Should be able to access the rasterizer and call methods
        let ds = crate::engines::draw_manager::DrawState::default();
        renderer.rasterizer_mut().draw(
            crate::engines::draw_manager::Maxwell3DDrawView::new(&ds, false),
            1,
        );
        renderer.rasterizer_mut().flush_all();
        assert!(!renderer
            .rasterizer()
            .must_flush_region(0, 0, crate::cache_types::CacheType::ALL,));
    }
}
