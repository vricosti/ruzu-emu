// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's video_core/renderer_null/renderer_null.h and renderer_null.cpp
//! Status: COMPLET
//!
//! Null renderer — frame composition is a no-op (no display output).
//! Used for headless testing and benchmarking.

use std::sync::{Arc, RwLock};

use super::null_rasterizer::RasterizerNull;
use crate::framebuffer_config::FramebufferConfig;
use crate::host1x::syncpoint_manager::SyncpointManager;
use crate::rasterizer_interface::RasterizerInterface;
use crate::renderer_base::{
    update_current_framebuffer_layout, FramebufferLayout, RendererBase, RendererBaseData,
};

/// Null renderer — corresponds to Eden's `Null::RendererNull`.
///
/// Extends the renderer base concept with no-op frame composition.
/// Owns a [`RasterizerNull`] for draw call handling.
/// Null graphics context (no-op).
struct NullContext;
impl ruzu_core::frontend::graphics_context::GraphicsContext for NullContext {}

pub struct RendererNull {
    base_data: RendererBaseData,
    framebuffer_layout: Arc<RwLock<FramebufferLayout>>,
    frame_displayed_notify: Arc<dyn Fn() + Send + Sync>,
    frame_end_notify: Arc<dyn Fn() + Send + Sync>,
    null_context: NullContext,
    rasterizer: RasterizerNull,
}

impl RendererNull {
    /// Create a new null renderer.
    pub fn new(
        syncpoints: Arc<SyncpointManager>,
        framebuffer_layout: Arc<RwLock<FramebufferLayout>>,
        frame_displayed_notify: Arc<dyn Fn() + Send + Sync>,
        frame_end_notify: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        update_current_framebuffer_layout(&framebuffer_layout);
        Self {
            base_data: RendererBaseData::new(),
            framebuffer_layout,
            frame_displayed_notify,
            frame_end_notify,
            null_context: NullContext,
            rasterizer: RasterizerNull::new(syncpoints),
        }
    }

    /// Composite framebuffers — no-op in null renderer.
    ///
    /// Matches Eden's `RendererNull::Composite()` notification order.
    pub fn composite_impl(&mut self, layers: &[FramebufferConfig]) {
        if layers.is_empty() {
            return;
        }
        (self.frame_end_notify)();
        (self.frame_displayed_notify)();
    }

    /// Get a zeroed applet capture buffer.
    ///
    /// Matches Eden: returns `TiledSize` bytes of zeros.
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

    fn refresh_base_settings(&mut self) {
        update_current_framebuffer_layout(&self.framebuffer_layout);
    }

    fn is_screenshot_pending(&self) -> bool {
        self.base_data.is_screenshot_pending()
    }

    fn request_screenshot(
        &mut self,
        data: *mut std::ffi::c_void,
        callback: Box<dyn FnOnce(bool) + Send>,
        layout: ruzu_core::frontend::framebuffer_layout::FramebufferLayout,
    ) {
        self.base_data.request_screenshot(data, callback, layout);
    }

    fn set_gpu_ticks_getter(&mut self, getter: crate::renderer_base::GpuTicksGetter) {
        self.rasterizer.set_gpu_ticks_getter(getter);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_renderer() -> RendererNull {
        RendererNull::new(
            Arc::new(SyncpointManager::new()),
            Arc::new(RwLock::new(
                ruzu_core::frontend::framebuffer_layout::default_frame_layout(1280, 720),
            )),
            Arc::new(|| {}),
            Arc::new(|| {}),
        )
    }

    fn dummy_fb() -> FramebufferConfig {
        FramebufferConfig {
            address: 0,
            offset: 0,
            width: 1280,
            height: 720,
            stride: 1280,
            pixel_format:
                ruzu_core::hle::service::nvnflinger::pixel_format::PixelFormat::NoFormat,
            transform_flags: ruzu_core::hle::service::nvnflinger::buffer_transform_flags::BufferTransformFlags::empty(),
            crop_rect: common::math_util::Rectangle {
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
        let notifications = Arc::new(std::sync::Mutex::new(Vec::new()));
        let displayed_notifications = Arc::clone(&notifications);
        let ended_notifications = Arc::clone(&notifications);
        let mut renderer = RendererNull::new(
            Arc::new(SyncpointManager::new()),
            Arc::new(RwLock::new(
                ruzu_core::frontend::framebuffer_layout::default_frame_layout(1280, 720),
            )),
            Arc::new(move || {
                displayed_notifications.lock().unwrap().push("displayed");
            }),
            Arc::new(move || {
                ended_notifications.lock().unwrap().push("end");
            }),
        );

        // Empty framebuffer list should be a no-op
        renderer.composite_impl(&[]);
        assert_eq!(renderer.frame_count(), 0);
        assert!(notifications.lock().unwrap().is_empty());

        // Eden notifies the GPU and window without changing RendererBase's
        // frame counter.
        renderer.composite_impl(&[dummy_fb()]);
        assert_eq!(renderer.frame_count(), 0);
        assert_eq!(*notifications.lock().unwrap(), vec!["end", "displayed"]);

        renderer.composite_impl(&[dummy_fb(), dummy_fb()]);
        assert_eq!(renderer.frame_count(), 0);
        assert_eq!(
            *notifications.lock().unwrap(),
            vec!["end", "displayed", "end", "displayed"]
        );
    }

    #[test]
    fn test_renderer_null_uses_base_screenshot_request_lifecycle() {
        let mut renderer = new_renderer();
        let mut pixel = 0u32;
        let (tx, rx) = std::sync::mpsc::channel();
        let layout = ruzu_core::frontend::framebuffer_layout::default_frame_layout(1, 1);

        renderer.request_screenshot(
            (&mut pixel as *mut u32).cast(),
            Box::new(move |invert_y| tx.send(invert_y).unwrap()),
            layout,
        );

        assert!(renderer.is_screenshot_pending());
        assert!(rx.try_recv().is_err());
        renderer
            .base_data
            .settings
            .screenshot_complete_callback
            .take()
            .unwrap()(false);
        assert!(!rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap());
    }

    #[test]
    fn test_renderer_null_capture_buffer() {
        let renderer = new_renderer();

        let buf = renderer.get_applet_capture_buffer();
        assert_eq!(buf.len(), crate::capture::TILED_SIZE as usize);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_renderer_null_vendor() {
        let renderer = new_renderer();
        assert_eq!(renderer.device_vendor(), "NULL");
    }

    #[test]
    fn test_renderer_null_rasterizer_access() {
        let mut renderer = new_renderer();

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
