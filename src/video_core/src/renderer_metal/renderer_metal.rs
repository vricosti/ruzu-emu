// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal renderer owner for macOS.
//!
//! This is the Metal API counterpart of Eden's
//! `renderer_vulkan/renderer_vulkan.{h,cpp}`. The platform backend differs,
//! but renderer/rasterizer ownership and per-frame ordering stay aligned with
//! Eden: resolve the guest framebuffer, present it, notify frame end, then
//! advance rasterizer caches.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use thiserror::Error;

use crate::framebuffer_config::FramebufferConfig;
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::host1x::syncpoint_manager::SyncpointManager;
use crate::rasterizer_interface::RasterizerInterface;
use crate::renderer_base::{FramebufferLayout, RendererBase, RendererBaseData};

use super::metal_device::{MetalDevice, MetalDeviceError};
use super::metal_layer::{MetalLayer, MetalLayerError};
use super::metal_presenter::{MetalPresenter, MetalPresenterError};
use super::metal_rasterizer::{MetalRasterizer, MetalRasterizerError};

#[derive(Debug, Error)]
pub enum MetalRendererError {
    #[error(transparent)]
    Device(#[from] MetalDeviceError),
    #[error(transparent)]
    Layer(#[from] MetalLayerError),
    #[error(transparent)]
    Presenter(#[from] MetalPresenterError),
    #[error(transparent)]
    Rasterizer(#[from] MetalRasterizerError),
}

struct MetalDummyContext;

impl ruzu_core::frontend::graphics_context::GraphicsContext for MetalDummyContext {}

pub struct RendererMetal {
    device: MetalDevice,
    rasterizer: MetalRasterizer,
    presenter: MetalPresenter,
    window_shown: Arc<AtomicBool>,
    framebuffer_layout: Arc<RwLock<FramebufferLayout>>,
    frame_displayed_notify: Arc<dyn Fn() + Send + Sync>,
    frame_end_notify: Arc<dyn Fn() + Send + Sync>,
    base_data: RendererBaseData,
    dummy_context: MetalDummyContext,
}

// SAFETY: construction happens on the boot thread and ownership is then moved
// once into the GPU thread. All Metal encoders, cache back-pointers and
// channel-state pointers are created and consumed on that GPU thread; no
// renderer method is invoked concurrently. This is the same owner-transfer
// contract used by RendererVulkan and RendererOpenGL.
unsafe impl Send for RendererMetal {}

impl RendererMetal {
    pub fn new(
        window_info: &ruzu_core::frontend::emu_window::WindowSystemInfo,
        window_shown: Arc<AtomicBool>,
        framebuffer_layout: Arc<RwLock<FramebufferLayout>>,
        frame_displayed_notify: Arc<dyn Fn() + Send + Sync>,
        frame_end_notify: Arc<dyn Fn() + Send + Sync>,
        syncpoints: Arc<SyncpointManager>,
        device_memory: Arc<MaxwellDeviceMemoryManager>,
    ) -> Result<Self, MetalRendererError> {
        let device = MetalDevice::new()?;
        let layer = unsafe { MetalLayer::from_raw(window_info.render_surface, &device)? };
        let presenter = MetalPresenter::new(layer, &device)?;
        let rasterizer = MetalRasterizer::new(device.clone(), syncpoints, device_memory)?;
        log::info!("Metal device: {}", device.name());
        Ok(Self {
            device,
            rasterizer,
            presenter,
            window_shown,
            framebuffer_layout,
            frame_displayed_notify,
            frame_end_notify,
            base_data: RendererBaseData::new(),
            dummy_context: MetalDummyContext,
        })
    }

    pub fn rasterizer_mut(&mut self) -> &mut MetalRasterizer {
        &mut self.rasterizer
    }

    fn composite_impl(&mut self, layers: &[FramebufferConfig]) {
        struct FrameDisplayedGuard(Arc<dyn Fn() + Send + Sync>);
        impl Drop for FrameDisplayedGuard {
            fn drop(&mut self) {
                (self.0)();
            }
        }
        let _frame_displayed = FrameDisplayedGuard(Arc::clone(&self.frame_displayed_notify));
        if !self.window_shown.load(Ordering::Relaxed) {
            return;
        }

        let source = layers.iter().rev().find_map(|framebuffer| {
            let framebuffer_addr = framebuffer.address.wrapping_add(framebuffer.offset as u64);
            if framebuffer_addr == 0 {
                return None;
            }
            let cache = self.rasterizer.texture_cache();
            let mutex: *const _ = &cache.base.mutex;
            let _guard = unsafe { (*mutex).lock() };
            cache
                .framebuffer_image_view(framebuffer, framebuffer_addr)
                .map(|(texture, _, _)| texture)
        });

        let Some(source) = source else {
            log::warn!("Metal presentation skipped: no cached guest framebuffer image");
            return;
        };
        if let Err(error) = self
            .presenter
            .present_texture(self.rasterizer.scheduler(), &source)
        {
            log::error!("Metal presentation failed: {error}");
            return;
        }
        (self.frame_end_notify)();
        self.rasterizer.tick_frame();
        self.base_data.current_frame = self.base_data.current_frame.wrapping_add(1);
    }
}

impl RendererBase for RendererMetal {
    fn context_ptr(&mut self) -> *mut dyn ruzu_core::frontend::graphics_context::GraphicsContext {
        &mut self.dummy_context
    }

    fn composite(&mut self, layers: &[FramebufferConfig]) {
        self.composite_impl(layers);
    }

    fn get_applet_capture_buffer(&mut self) -> Vec<u8> {
        vec![0; crate::capture::tiled_size() as usize]
    }

    fn read_rasterizer(&self) -> *mut dyn RasterizerInterface {
        let rasterizer: &dyn RasterizerInterface = &self.rasterizer;
        rasterizer as *const dyn RasterizerInterface as *mut dyn RasterizerInterface
    }

    fn get_device_vendor(&self) -> String {
        self.device.name()
    }

    fn current_fps(&self) -> f32 {
        self.base_data.current_fps
    }

    fn current_frame(&self) -> i32 {
        self.base_data.current_frame
    }

    fn refresh_base_settings(&mut self) {
        crate::renderer_base::update_current_framebuffer_layout(&self.framebuffer_layout);
    }

    fn is_screenshot_pending(&self) -> bool {
        self.base_data.is_screenshot_pending()
    }

    fn request_screenshot(
        &mut self,
        _data: *mut std::ffi::c_void,
        callback: Box<dyn FnOnce(bool) + Send>,
        _layout: FramebufferLayout,
    ) {
        callback(false);
    }

    fn set_guest_memory_writer(&mut self, writer: crate::renderer_base::GuestMemoryWriter) {
        self.rasterizer.set_guest_memory_writer(writer);
    }

    fn set_gpu_ticks_getter(&mut self, getter: crate::renderer_base::GpuTicksGetter) {
        self.rasterizer.set_gpu_ticks_getter(getter);
    }

    fn set_gpu_tick_callback(&mut self, callback: crate::renderer_base::GpuTickCallback) {
        self.rasterizer.set_gpu_tick_callback(callback);
    }

    fn set_invalidate_gpu_cache_callback(
        &mut self,
        callback: crate::renderer_base::InvalidateGpuCacheCallback,
    ) {
        self.rasterizer.set_invalidate_gpu_cache_callback(callback);
    }
}
