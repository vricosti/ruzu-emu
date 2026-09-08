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

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLDevice,
    MTLOrigin, MTLPixelFormat, MTLSize, MTLStorageMode, MTLTexture, MTLTextureDescriptor,
    MTLTextureUsage, MTLViewport,
};
use thiserror::Error;

use crate::framebuffer_config::FramebufferConfig;
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::host1x::syncpoint_manager::SyncpointManager;
use crate::rasterizer_interface::RasterizerInterface;
use crate::renderer_base::{FramebufferLayout, RendererBase, RendererBaseData};

use super::metal_buffer::{MetalBuffer, MetalBufferError};
use super::metal_device::{MetalDevice, MetalDeviceError};
use super::metal_layer::{MetalLayer, MetalLayerError};
use super::metal_presenter::{MetalPresenter, MetalPresenterError};
use super::metal_rasterizer::{MetalRasterizer, MetalRasterizerError};
use super::metal_scheduler::MetalSchedulerError;
use super::present::layer::Layer;

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
    #[error(transparent)]
    Buffer(#[from] MetalBufferError),
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
    #[error("invalid screenshot dimensions, screen rectangle or destination pointer")]
    InvalidScreenshot,
    #[error("Metal failed to allocate the screenshot texture or copy encoder")]
    ScreenshotAllocation,
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
    applet_frame: Option<Retained<ProtocolObject<dyn MTLTexture>>>,
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
            applet_frame: None,
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
        let sources: Vec<_> = layers
            .iter()
            .filter_map(|framebuffer| {
                let framebuffer_addr = framebuffer.address.wrapping_add(framebuffer.offset as u64);
                if framebuffer_addr == 0 {
                    return None;
                }
                let cache = self.rasterizer.texture_cache();
                let mutex: *const _ = &cache.base.mutex;
                let _guard = unsafe { (*mutex).lock() };
                cache
                    .framebuffer_image_view(framebuffer, framebuffer_addr)
                    .map(|(texture, width, height, _)| {
                        Layer::configure_draw(texture, framebuffer, width, height)
                    })
            })
            .collect();

        if sources.is_empty() {
            log::warn!("Metal presentation skipped: no cached guest framebuffer image");
            return;
        }
        if let Err(error) = self.render_applet_capture_layer(&sources) {
            log::error!("Metal applet capture failed: {error}");
            return;
        }
        if !self.window_shown.load(Ordering::Relaxed) {
            return;
        }
        if let Err(error) = self.render_screenshot(&sources) {
            log::error!("Metal screenshot failed: {error}");
        }
        if let Err(error) = self
            .presenter
            .present_layers(self.rasterizer.scheduler(), &sources)
        {
            log::error!("Metal presentation failed: {error}");
            return;
        }
        (self.frame_end_notify)();
        self.rasterizer.tick_frame();
        self.base_data.current_frame = self.base_data.current_frame.wrapping_add(1);
    }

    /// Eden RenderAppletCaptureLayer: preserve a composed image even while hidden.
    /// Same-queue ordering makes subsequent capture downloads see the last frame.
    fn render_applet_capture_layer(&mut self, layers: &[Layer]) -> Result<(), MetalRendererError> {
        if self.applet_frame.is_none() {
            let descriptor = MTLTextureDescriptor::new();
            descriptor.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
            descriptor.setStorageMode(MTLStorageMode::Private);
            descriptor.setUsage(MTLTextureUsage::RenderTarget);
            unsafe {
                descriptor.setWidth(crate::capture::LINEAR_WIDTH as usize);
                descriptor.setHeight(crate::capture::LINEAR_HEIGHT as usize);
            }
            self.applet_frame = Some(
                self.device
                    .device()
                    .newTextureWithDescriptor(&descriptor)
                    .ok_or(MetalRendererError::ScreenshotAllocation)?,
            );
        }
        self.rasterizer.scheduler().flush()?;
        let command = self.rasterizer.scheduler().begin()?;
        self.presenter.draw_layers(
            &command,
            layers,
            self.applet_frame.as_ref().unwrap(),
            MTLViewport {
                originX: 0.0,
                originY: 0.0,
                width: crate::capture::LINEAR_WIDTH as f64,
                height: crate::capture::LINEAR_HEIGHT as f64,
                znear: 0.0,
                zfar: 1.0,
            },
            Some(MetalPresenter::background()),
        )?;
        self.rasterizer.scheduler().commit(command)?;
        Ok(())
    }

    fn download_applet_capture(&mut self) -> Result<Vec<u8>, MetalRendererError> {
        let mut out = vec![0; crate::capture::TILED_SIZE as usize];
        let Some(source) = self.applet_frame.clone() else {
            return Ok(out);
        };
        let width = crate::capture::LINEAR_WIDTH as usize;
        let height = crate::capture::LINEAR_HEIGHT as usize;
        let pitch = width * crate::capture::BYTES_PER_PIXEL as usize;
        let download = MetalBuffer::new(&self.device, pitch * height)?;
        self.rasterizer.scheduler().flush()?;
        let command = self.rasterizer.scheduler().begin()?;
        let encoder = command
            .blitCommandEncoder()
            .ok_or(MetalRendererError::ScreenshotAllocation)?;
        unsafe {
            encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                &source, 0, 0, MTLOrigin { x: 0, y: 0, z: 0 }, MTLSize { width, height, depth: 1 },
                download.handle(), 0, pitch, pitch * height);
        }
        encoder.endEncoding();
        self.rasterizer.scheduler().finish(command)?;
        // MetalBuffer uses shared storage; completed GPU writes are CPU-visible.
        let bytes = unsafe { std::slice::from_raw_parts(download.contents_ptr(), pitch * height) };
        crate::textures::decoders::swizzle_texture(
            &mut out,
            bytes,
            crate::capture::BYTES_PER_PIXEL,
            crate::capture::LINEAR_WIDTH,
            crate::capture::LINEAR_HEIGHT,
            crate::capture::LINEAR_DEPTH,
            crate::capture::BLOCK_HEIGHT,
            crate::capture::BLOCK_DEPTH,
            0,
        );
        Ok(out)
    }

    /// Native counterpart of Eden RendererVulkan::RenderToBuffer. The capture
    /// command buffer follows all outstanding guest rendering on the same queue.
    fn render_to_buffer(
        &mut self,
        sources: &[Layer],
        layout: &FramebufferLayout,
    ) -> Result<(MetalBuffer, usize), MetalRendererError> {
        let width = layout.width as usize;
        let height = layout.height as usize;
        let screen = layout.screen;
        if width == 0
            || height == 0
            || screen.left >= screen.right
            || screen.top >= screen.bottom
            || screen.right > layout.width
            || screen.bottom > layout.height
        {
            return Err(MetalRendererError::InvalidScreenshot);
        }
        // Texture-to-buffer blits require padded rows. Only actual pixel bytes
        // are later copied into the frontend's tightly packed BGRA allocation.
        let row_pitch = width
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(255))
            .map(|bytes| bytes & !255)
            .ok_or(MetalRendererError::InvalidScreenshot)?;
        let size = row_pitch
            .checked_mul(height)
            .filter(|size| *size <= isize::MAX as usize)
            .ok_or(MetalRendererError::InvalidScreenshot)?;
        let descriptor = MTLTextureDescriptor::new();
        descriptor.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        descriptor.setStorageMode(MTLStorageMode::Private);
        descriptor.setUsage(MTLTextureUsage::RenderTarget);
        unsafe {
            descriptor.setWidth(width);
            descriptor.setHeight(height);
        }
        let target = self
            .device
            .device()
            .newTextureWithDescriptor(&descriptor)
            .ok_or(MetalRendererError::ScreenshotAllocation)?;
        let download = MetalBuffer::new(&self.device, size)?;
        let command_buffer = self.rasterizer.scheduler().begin()?;
        let settings = common::settings::values();
        self.presenter.draw_layers(
            &command_buffer,
            sources,
            &target,
            MTLViewport {
                originX: screen.left as f64,
                originY: screen.top as f64,
                width: (screen.right - screen.left) as f64,
                height: (screen.bottom - screen.top) as f64,
                znear: 0.0,
                zfar: 1.0,
            },
            Some(MTLClearColor {
                red: *settings.bg_red.get_value() as f64 / 255.0,
                green: *settings.bg_green.get_value() as f64 / 255.0,
                blue: *settings.bg_blue.get_value() as f64 / 255.0,
                alpha: 1.0,
            }),
        )?;
        let encoder = command_buffer
            .blitCommandEncoder()
            .ok_or(MetalRendererError::ScreenshotAllocation)?;
        unsafe {
            encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                &target, 0, 0, MTLOrigin { x: 0, y: 0, z: 0 },
                MTLSize { width, height, depth: 1 }, download.handle(), 0, row_pitch, size,
            );
        }
        encoder.endEncoding();
        self.rasterizer.scheduler().finish(command_buffer)?;
        Ok((download, row_pitch))
    }

    fn render_screenshot(&mut self, sources: &[Layer]) -> Result<(), MetalRendererError> {
        if !self.base_data.is_screenshot_pending() {
            return Ok(());
        }
        let result = (|| {
            let destination = self.base_data.settings.screenshot_bits.cast::<u8>();
            if destination.is_null() {
                return Err(MetalRendererError::InvalidScreenshot);
            }
            let layout = self
                .base_data
                .settings
                .screenshot_framebuffer_layout
                .clone();
            let (download, pitch) = self.render_to_buffer(sources, &layout)?;
            let row_bytes = layout.width as usize * 4;
            for row in 0..layout.height as usize {
                // The frontend owns width*height*4 writable bytes until its
                // callback is consumed; finish above made GPU writes visible.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        download.contents_ptr().add(row * pitch),
                        destination.add(row * row_bytes),
                        row_bytes,
                    );
                }
            }
            Ok(())
        })();
        let settings = &mut self.base_data.settings;
        settings.screenshot_bits = std::ptr::null_mut();
        let callback = settings.screenshot_complete_callback.take();
        if result.is_ok() {
            if let Some(callback) = callback {
                callback(false); // Top-down BGRA, not a failure/success flag.
            }
        }
        settings.screenshot_requested.store(false, Ordering::SeqCst);
        result
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
        self.download_applet_capture().unwrap_or_else(|error| {
            log::error!("Metal applet capture download failed: {error}");
            Vec::new()
        })
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
        data: *mut std::ffi::c_void,
        callback: Box<dyn FnOnce(bool) + Send>,
        layout: FramebufferLayout,
    ) {
        self.base_data.request_screenshot(data, callback, layout);
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

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::rc::Retained;
    use objc2_quartz_core::CAMetalLayer;
    use ruzu_core::frontend::emu_window::WindowSystemInfo;
    use ruzu_core::frontend::framebuffer_layout::Rectangle;
    use std::sync::mpsc;
    use std::time::Duration;

    fn headless_renderer() -> RendererMetal {
        let layer = CAMetalLayer::new();
        let window = WindowSystemInfo {
            render_surface: Retained::as_ptr(&layer) as usize,
            ..WindowSystemInfo::default()
        };
        RendererMetal::new(
            &window,
            Arc::new(AtomicBool::new(true)),
            Arc::new(RwLock::new(FramebufferLayout::default())),
            Arc::new(|| {}),
            Arc::new(|| {}),
            Arc::new(SyncpointManager::new()),
            Arc::new(MaxwellDeviceMemoryManager::default()),
        )
        .unwrap()
    }

    const COLORS: [[u8; 4]; 6] = [
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255, 255, 0, 255],
        [0, 255, 255, 255],
        [255, 0, 255, 255],
    ];

    fn queue_source_upload(
        renderer: &mut RendererMetal,
    ) -> Retained<ProtocolObject<dyn MTLTexture>> {
        let descriptor = MTLTextureDescriptor::new();
        descriptor.setPixelFormat(MTLPixelFormat::RGBA8Unorm);
        descriptor.setStorageMode(MTLStorageMode::Private);
        descriptor.setUsage(MTLTextureUsage::ShaderRead);
        unsafe {
            descriptor.setWidth(3);
            descriptor.setHeight(2);
        }
        let texture = renderer
            .device
            .device()
            .newTextureWithDescriptor(&descriptor)
            .unwrap();
        let upload = MetalBuffer::new(&renderer.device, 512).unwrap();
        let mut bytes = [0; 512];
        for (index, color) in COLORS.iter().enumerate() {
            let offset = (index / 3) * 256 + (index % 3) * 4;
            bytes[offset..offset + 4].copy_from_slice(color);
        }
        upload.write(0, &bytes).unwrap();
        renderer.rasterizer.scheduler().with_blit_encoder(|encoder| unsafe {
            encoder.copyFromBuffer_sourceOffset_sourceBytesPerRow_sourceBytesPerImage_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                upload.handle(), 0, 256, 512, MTLSize { width: 3, height: 2, depth: 1 },
                &texture, 0, 0, MTLOrigin { x: 0, y: 0, z: 0 },
            );
        }).unwrap();
        texture
    }

    #[test]
    fn native_layer_blending_and_capture_round_trip() {
        objc2::rc::autoreleasepool(|_| {
            use crate::framebuffer_config::BlendMode;
            let mut renderer = headless_renderer();
            let make_texture = |rgba: [u8; 4]| {
                let desc = MTLTextureDescriptor::new();
                desc.setPixelFormat(MTLPixelFormat::RGBA8Unorm);
                desc.setStorageMode(MTLStorageMode::Shared);
                desc.setUsage(MTLTextureUsage::ShaderRead);
                let texture = renderer
                    .device
                    .device()
                    .newTextureWithDescriptor(&desc)
                    .unwrap();
                unsafe {
                    texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                        objc2_metal::MTLRegion {
                            origin: MTLOrigin { x: 0, y: 0, z: 0 },
                            size: MTLSize {
                                width: 1,
                                height: 1,
                                depth: 1,
                            },
                        },
                        0,
                        std::ptr::NonNull::from(&rgba).cast(),
                        4,
                    );
                }
                texture
            };
            let background = make_texture([0, 0, 255, 255]);
            let foreground = make_texture([128, 0, 0, 128]);
            assert!(renderer.get_applet_capture_buffer().iter().all(|&b| b == 0));
            let layout = FramebufferLayout {
                width: 1,
                height: 1,
                screen: Rectangle {
                    left: 0,
                    top: 0,
                    right: 1,
                    bottom: 1,
                },
                is_srgb: false,
            };
            for (mode, expected) in [
                (BlendMode::Opaque, [0, 0, 128, 128]),
                (BlendMode::Premultiplied, [127, 0, 128, 128]),
                (BlendMode::Coverage, [127, 0, 64, 128]),
            ] {
                let layers = [
                    Layer::opaque(background.clone()),
                    Layer {
                        texture: foreground.clone(),
                        crop: [0.0, 0.0, 1.0, 1.0],
                        blending: mode,
                    },
                ];
                let (download, _) = renderer.render_to_buffer(&layers, &layout).unwrap();
                let actual = unsafe { std::slice::from_raw_parts(download.contents_ptr(), 4) };
                for (&actual, expected) in actual.iter().zip(expected) {
                    assert!(
                        (actual as i16 - expected as i16).abs() <= 1,
                        "{mode:?}: {actual} != {expected}"
                    );
                }
            }
            renderer
                .render_applet_capture_layer(&[Layer::opaque(background)])
                .unwrap();
            let first = renderer.applet_frame.clone().unwrap();
            renderer
                .render_applet_capture_layer(&[Layer::opaque(foreground)])
                .unwrap();
            assert!(std::ptr::eq(
                &*first,
                &**renderer.applet_frame.as_ref().unwrap()
            ));
            let tiled = renderer.get_applet_capture_buffer();
            assert_eq!(tiled.len(), crate::capture::TILED_SIZE as usize);
            let mut linear = vec![
                0;
                (crate::capture::LINEAR_WIDTH * crate::capture::LINEAR_HEIGHT * 4)
                    as usize
            ];
            crate::textures::decoders::unswizzle_texture(
                &mut linear,
                &tiled,
                4,
                crate::capture::LINEAR_WIDTH,
                crate::capture::LINEAR_HEIGHT,
                1,
                crate::capture::BLOCK_HEIGHT,
                0,
                0,
            );
            assert!(linear
                .chunks_exact(4)
                .all(|pixel| pixel == [0, 0, 128, 128]));
        });
    }

    #[test]
    fn layer_crop_and_flip_sample_guest_coordinates() {
        objc2::rc::autoreleasepool(|_| {
            use ruzu_core::hle::service::nvnflinger::buffer_transform_flags::BufferTransformFlags;
            let mut renderer = headless_renderer();
            let source = queue_source_upload(&mut renderer);
            let config = FramebufferConfig {
                width: 3,
                height: 2,
                crop_rect: common::math_util::Rectangle::new(1, 0, 3, 2),
                transform_flags: BufferTransformFlags::FLIP_H | BufferTransformFlags::FLIP_V,
                ..Default::default()
            };
            let layer = Layer::configure_draw(source, &config, 3, 2);
            let layout = FramebufferLayout {
                width: 2,
                height: 2,
                screen: Rectangle {
                    left: 0,
                    top: 0,
                    right: 2,
                    bottom: 2,
                },
                is_srgb: false,
            };
            let (download, pitch) = renderer.render_to_buffer(&[layer], &layout).unwrap();
            for (i, expected) in [
                [255, 0, 255, 255],
                [255, 255, 0, 255],
                [255, 0, 0, 255],
                [0, 255, 0, 255],
            ]
            .iter()
            .enumerate()
            {
                let pixel = unsafe {
                    std::slice::from_raw_parts(
                        download.contents_ptr().add((i / 2) * pitch + (i % 2) * 4),
                        4,
                    )
                };
                assert_eq!(pixel, expected);
            }
        });
    }

    #[test]
    fn screenshot_waits_for_upload_and_returns_packed_top_down_bgra_once() {
        objc2::rc::autoreleasepool(|_| {
            let mut renderer = headless_renderer();
            let source = queue_source_upload(&mut renderer);
            let initial_tick = renderer.rasterizer.scheduler().current_tick();
            renderer
                .render_screenshot(&[Layer::opaque(source.clone())])
                .unwrap();
            assert_eq!(renderer.rasterizer.scheduler().current_tick(), initial_tick);
            assert!(renderer.rasterizer.scheduler().has_active_work());

            let layout = FramebufferLayout {
                width: 5,
                height: 4,
                screen: Rectangle {
                    left: 1,
                    top: 1,
                    right: 4,
                    bottom: 3,
                },
                is_srgb: false,
            };
            let mut pixels = [0xACu8; 88];
            let (tx, rx) = mpsc::channel();
            renderer.request_screenshot(
                unsafe { pixels.as_mut_ptr().add(4).cast() },
                Box::new(move |invert_y| tx.send(invert_y).unwrap()),
                layout.clone(),
            );
            assert!(renderer.is_screenshot_pending());
            assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
            let (duplicate_tx, duplicate_rx) = mpsc::channel();
            renderer.request_screenshot(
                std::ptr::null_mut(),
                Box::new(move |value| duplicate_tx.send(value).unwrap()),
                layout,
            );
            assert!(matches!(
                duplicate_rx.try_recv(),
                Err(mpsc::TryRecvError::Disconnected)
            ));
            renderer
                .render_screenshot(&[Layer::opaque(source.clone())])
                .unwrap();
            assert!(!rx.recv_timeout(Duration::from_secs(2)).unwrap());
            assert!(!renderer.is_screenshot_pending());
            assert!(renderer.base_data.settings.screenshot_bits.is_null());
            assert_eq!(&pixels[..4], &[0xAC; 4]);
            assert_eq!(&pixels[84..], &[0xAC; 4]);
            let settings = common::settings::values();
            let background = [
                *settings.bg_blue.get_value(),
                *settings.bg_green.get_value(),
                *settings.bg_red.get_value(),
                255,
            ];
            for y in 0..4 {
                for x in 0..5 {
                    let expected = if (1..4).contains(&x) && (1..3).contains(&y) {
                        let [r, g, b, a] = COLORS[(y - 1) * 3 + x - 1];
                        [b, g, r, a]
                    } else {
                        background
                    };
                    let offset = 4 + (y * 5 + x) * 4;
                    assert_eq!(&pixels[offset..offset + 4], &expected, "pixel ({x},{y})");
                }
            }
            let captured_tick = renderer.rasterizer.scheduler().current_tick();
            assert_eq!(captured_tick, initial_tick + 2);
            renderer
                .render_screenshot(&[Layer::opaque(source.clone())])
                .unwrap();
            assert_eq!(
                renderer.rasterizer.scheduler().current_tick(),
                captured_tick
            );
        });
    }

    #[test]
    fn invalid_capture_cancels_request_without_reporting_a_blank_image() {
        objc2::rc::autoreleasepool(|_| {
            let mut renderer = headless_renderer();
            let source = queue_source_upload(&mut renderer);
            let initial_tick = renderer.rasterizer.scheduler().current_tick();
            let mut pixels = [0xACu8; 4];
            let (tx, rx) = mpsc::channel();
            renderer.request_screenshot(
                pixels.as_mut_ptr().cast(),
                Box::new(move |value| tx.send(value).unwrap()),
                FramebufferLayout {
                    width: 0,
                    height: 0,
                    ..FramebufferLayout::default()
                },
            );
            assert!(matches!(
                renderer.render_screenshot(&[Layer::opaque(source.clone())]),
                Err(MetalRendererError::InvalidScreenshot)
            ));
            assert!(!renderer.is_screenshot_pending());
            assert!(matches!(
                rx.try_recv(),
                Err(mpsc::TryRecvError::Disconnected)
            ));
            assert_eq!(pixels, [0xAC; 4]);
            assert_eq!(renderer.rasterizer.scheduler().current_tick(), initial_tick);
            renderer.rasterizer.scheduler().finish_all().unwrap();
        });
    }
}
