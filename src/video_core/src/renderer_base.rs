// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/renderer_base.h and video_core/renderer_base.cpp
//!
//! Abstract base renderer interface.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::framebuffer_config::FramebufferConfig;
pub use ruzu_core::frontend::framebuffer_layout::FramebufferLayout;

/// Context used by renderers that do not require a host graphics context.
///
/// This is the Rust counterpart of the dummy context returned by non-OpenGL
/// `EmuWindow::CreateSharedContext()` implementations.
pub struct DummyGraphicsContext;

impl ruzu_core::frontend::graphics_context::GraphicsContext for DummyGraphicsContext {}

/// Renderer settings (screenshots, etc.).
pub struct RendererSettings {
    pub screenshot_requested: AtomicBool,
    pub screenshot_bits: *mut std::ffi::c_void,
    pub screenshot_complete_callback: Option<Box<dyn FnOnce(bool) + Send>>,
    pub screenshot_framebuffer_layout: FramebufferLayout,
}

// Safety: screenshot_bits is only accessed on the render thread.
unsafe impl Send for RendererSettings {}
unsafe impl Sync for RendererSettings {}

impl Default for RendererSettings {
    fn default() -> Self {
        Self {
            screenshot_requested: AtomicBool::new(false),
            screenshot_bits: std::ptr::null_mut(),
            screenshot_complete_callback: None,
            screenshot_framebuffer_layout: FramebufferLayout::default(),
        }
    }
}

/// Callback for reading guest GPU memory by address.
/// Upstream: `Tegra::MaxwellDeviceMemoryManager& device_memory`.
/// Returns whether the requested range was fully mapped, matching the
/// `GetPointer(...) != nullptr` distinction used by upstream present fallback.
pub type DeviceMemoryReader = std::sync::Arc<dyn Fn(u64, &mut [u8]) -> bool + Send + Sync>;
pub type ShaderCacheGpuReader = std::sync::Arc<dyn Fn(u64, &mut [u8]) + Send + Sync>;
pub type GuestMemoryWriter = std::sync::Arc<dyn Fn(u64, &[u8]) + Send + Sync>;
pub type GpuTicksGetter = std::sync::Arc<dyn Fn() -> u64 + Send + Sync>;
pub type GpuTickCallback = std::sync::Arc<dyn Fn() + Send + Sync>;
pub type InvalidateGpuCacheCallback = std::sync::Arc<dyn Fn() + Send + Sync>;

/// Abstract renderer base trait.
///
/// Renderers (OpenGL, Vulkan, Null) implement this trait.
pub trait RendererBase: Send {
    /// Get a mutable pointer to the graphics context owned by this renderer.
    /// Matches upstream `RendererBase::Context()`.
    /// The returned pointer is valid for the lifetime of the renderer.
    fn context_ptr(&mut self) -> *mut dyn ruzu_core::frontend::graphics_context::GraphicsContext;

    /// Create the CPU-thread context used by `GPU::ObtainContext()` in
    /// synchronous single-core mode.
    ///
    /// Upstream delegates this to `GetRenderWindow().CreateSharedContext()`.
    /// Non-OpenGL backends use a context with no host-side work.
    fn create_shared_context(
        &self,
    ) -> Box<dyn ruzu_core::frontend::graphics_context::GraphicsContext + Send> {
        Box::new(DummyGraphicsContext)
    }

    /// Finalize rendering the guest frame and draw into the presentation texture.
    fn composite(&mut self, layers: &[FramebufferConfig]);

    /// Get the tiled applet layer capture buffer.
    fn get_applet_capture_buffer(&mut self) -> Vec<u8>;

    /// Get the rasterizer interface.
    fn read_rasterizer(&self) -> *mut dyn crate::rasterizer_interface::RasterizerInterface;

    /// Get the device vendor string.
    fn get_device_vendor(&self) -> String;

    /// Get current FPS.
    fn current_fps(&self) -> f32;

    /// Get current frame count.
    fn current_frame(&self) -> i32;

    /// Refresh base settings.
    fn refresh_base_settings(&mut self);

    /// Returns true if a screenshot is being processed.
    fn is_screenshot_pending(&self) -> bool;

    /// Request a screenshot of the next composited frame.
    fn request_screenshot(
        &mut self,
        data: *mut std::ffi::c_void,
        callback: Box<dyn FnOnce(bool) + Send>,
        layout: FramebufferLayout,
    );

    /// Install a *GPU virtual address* reader on the renderer's shader cache.
    ///
    /// This reader's first argument is a **GPU virtual address**
    /// — i.e. it has already been resolved through `MemoryManager`'s page
    /// table. The OpenGL renderer forwards this into its shader cache so
    /// the recompiler can fetch Maxwell shader bytecode at the addresses
    /// reported by `Maxwell3D::shader_program_addresses`.
    ///
    /// Default impl is a no-op for renderers that do not yet have a
    /// shader cache (Null, current Vulkan stub).
    fn set_shader_cache_gpu_reader(&mut self, _reader: ShaderCacheGpuReader) {}

    /// Install the guest memory writer used by rasterizer-side query/semaphore writes.
    fn set_guest_memory_writer(&mut self, _writer: GuestMemoryWriter) {}

    /// Install the GPU tick getter used by rasterizer-side timestamped query writes.
    fn set_gpu_ticks_getter(&mut self, _getter: GpuTicksGetter) {}

    /// Install the callback used by rasterizer draw paths to process pending GPU sync work.
    fn set_gpu_tick_callback(&mut self, _callback: GpuTickCallback) {}

    /// Install the callback implementing upstream `gpu.InvalidateGPUCache()`.
    fn set_invalidate_gpu_cache_callback(&mut self, _callback: InvalidateGpuCacheCallback) {}

    /// Install a GPU VA → CPU VA translator used by rasterizer-side
    /// query writes. Mirrors upstream's `gpu_memory->Write<u64>(gpu_va,
    /// ...)`: the rasterizer receives GPU VAs from the puller and must
    /// translate to CPU VAs before passing them to `guest_memory_writer`
    /// (which expects CPU VAs, since it ultimately calls
    /// `Memory::write_block`).
    fn set_gpu_to_cpu_translator(
        &mut self,
        _translator: std::sync::Arc<dyn Fn(u64) -> Option<u64> + Send + Sync>,
    ) {
    }
}

/// Concrete base renderer data, shared by all renderer implementations.
pub struct RendererBaseData {
    pub current_fps: f32,
    pub current_frame: i32,
    pub settings: RendererSettings,
}

/// Port of upstream `RendererBase::UpdateCurrentFramebufferLayout()`.
///
/// Reden's frontend window owns the live layout behind an `RwLock`; updating
/// that same object is the split-owner equivalent of
/// `render_window.UpdateCurrentFramebufferLayout(width, height)`.
pub(crate) fn update_current_framebuffer_layout(
    framebuffer_layout: &std::sync::RwLock<FramebufferLayout>,
) {
    let mut layout = framebuffer_layout.write().unwrap();
    if layout.width > 0 && layout.height > 0 {
        *layout = ruzu_core::frontend::framebuffer_layout::default_frame_layout(
            layout.width,
            layout.height,
        );
    }
}

#[cfg(test)]
mod refresh_layout_tests {
    use super::*;
    use common::settings_enums::AspectRatio;
    use std::sync::{Mutex, OnceLock, RwLock};

    #[test]
    fn update_current_framebuffer_layout_applies_live_aspect_ratio() {
        static SETTINGS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = SETTINGS_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();

        let previous = *common::settings::values().aspect_ratio.get_value();
        common::settings::values_mut()
            .aspect_ratio
            .set_value(AspectRatio::R4_3);

        let layout = RwLock::new(FramebufferLayout {
            width: 1600,
            height: 900,
            ..FramebufferLayout::default()
        });
        update_current_framebuffer_layout(&layout);

        let layout = layout.read().unwrap();
        assert_eq!(layout.screen.get_width(), 1200);
        assert_eq!(layout.screen.get_height(), 900);
        assert_eq!(layout.screen.left, 200);

        common::settings::values_mut()
            .aspect_ratio
            .set_value(previous);
    }
}

impl RendererBaseData {
    pub fn new() -> Self {
        Self {
            current_fps: 0.0,
            current_frame: 0,
            settings: RendererSettings::default(),
        }
    }

    /// Returns true if a screenshot is being processed.
    pub fn is_screenshot_pending(&self) -> bool {
        self.settings.screenshot_requested.load(Ordering::SeqCst)
    }

    /// Request a screenshot of the next frame.
    pub fn request_screenshot(
        &mut self,
        data: *mut std::ffi::c_void,
        callback: Box<dyn FnOnce(bool) + Send>,
        layout: FramebufferLayout,
    ) {
        if self.is_screenshot_pending() {
            log::error!("A screenshot is already requested or in progress, ignoring the request");
            return;
        }
        self.settings.screenshot_bits = data;
        self.settings.screenshot_complete_callback = Some(Box::new(move |invert_y| {
            let _ = std::thread::Builder::new()
                .name("Screenshot".to_owned())
                .spawn(move || callback(invert_y));
        }));
        self.settings.screenshot_framebuffer_layout = layout;
        self.settings
            .screenshot_requested
            .store(true, Ordering::SeqCst);
    }
}

impl Default for RendererBaseData {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_request_publishes_pointer_layout_and_async_callback() {
        let mut renderer = RendererBaseData::new();
        let mut pixel = 0u32;
        let layout = FramebufferLayout {
            width: 1,
            height: 1,
            ..FramebufferLayout::default()
        };
        let (tx, rx) = std::sync::mpsc::channel();

        renderer.request_screenshot(
            (&mut pixel as *mut u32).cast(),
            Box::new(move |invert_y| tx.send(invert_y).unwrap()),
            layout,
        );

        assert!(renderer.is_screenshot_pending());
        assert_eq!(
            renderer.settings.screenshot_bits,
            (&mut pixel as *mut u32).cast()
        );
        assert_eq!(renderer.settings.screenshot_framebuffer_layout.width, 1);
        renderer
            .settings
            .screenshot_complete_callback
            .take()
            .unwrap()(true);
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),
            true
        );
    }
}
