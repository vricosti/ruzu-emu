// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/video_core/renderer_opengl/renderer_opengl.h and renderer_opengl.cpp
//!
//! OpenGL GPU renderer — provides an alternative backend to Vulkan.
//!
use std::ffi::CStr;
use std::sync::{Arc, OnceLock, RwLock};

use log::{debug, info};
use thiserror::Error;

use super::gl_blit_screen::BlitScreen;
use super::gl_device::Device;
use super::gl_rasterizer::RasterizerOpenGL;
use super::gl_resource_manager::{OGLFramebuffer, OGLRenderbuffer};
use super::gl_shader_manager::{ProgramManager, ProgramManagerHandle};
use super::gl_state_tracker::StateTracker;
use super::{
    gl_buffer_cache, gl_graphics_pipeline, gl_rasterizer, gl_shader_context, gl_shader_util,
    present,
};

use crate::capture;
use crate::framebuffer_config::FramebufferConfig;
use crate::host1x::syncpoint_manager::SyncpointManager;
use crate::present::{PRESENT_FILTERS_FOR_APPLET_CAPTURE, PRESENT_FILTERS_FOR_DISPLAY};
use crate::rasterizer_interface::RasterizerInterface;
use crate::renderer_base::{RendererBase, RendererBaseData};
use ruzu_core::frontend::framebuffer_layout::FramebufferLayout;
use ruzu_core::frontend::graphics_context::GraphicsContext;

const GL_VERTEX_ATTRIB_ARRAY_UNIFIED_NV: u32 = 0x8F1E;
const GL_ELEMENT_ARRAY_UNIFIED_NV: u32 = 0x8F1F;

type GlEnableClientState = unsafe extern "system" fn(cap: u32);
static GL_ENABLE_CLIENT_STATE: OnceLock<Option<GlEnableClientState>> = OnceLock::new();

fn load_renderer_extra_functions<F>(load_fn: &mut F)
where
    F: FnMut(&'static str) -> *const std::os::raw::c_void,
{
    let pointer = load_fn("glEnableClientState");
    let function = if pointer.is_null() {
        None
    } else {
        Some(unsafe {
            std::mem::transmute::<*const std::os::raw::c_void, GlEnableClientState>(pointer)
        })
    };
    let _ = GL_ENABLE_CLIENT_STATE.set(function);
}

fn gl_string(name: u32) -> String {
    unsafe {
        let pointer = gl::GetString(name);
        if pointer.is_null() {
            return String::new();
        }
        CStr::from_ptr(pointer.cast())
            .to_string_lossy()
            .into_owned()
    }
}

/// Upstream anonymous-namespace `GetSource`.
fn debug_source(source: u32) -> &'static str {
    match source {
        gl::DEBUG_SOURCE_API => "API",
        gl::DEBUG_SOURCE_WINDOW_SYSTEM => "WINDOW_SYSTEM",
        gl::DEBUG_SOURCE_SHADER_COMPILER => "SHADER_COMPILER",
        gl::DEBUG_SOURCE_THIRD_PARTY => "THIRD_PARTY",
        gl::DEBUG_SOURCE_APPLICATION => "APPLICATION",
        gl::DEBUG_SOURCE_OTHER => "OTHER",
        _ => {
            log::error!("Unknown OpenGL debug source 0x{source:x}");
            "Unknown source"
        }
    }
}

/// Upstream anonymous-namespace `GetType`.
fn debug_type(gltype: u32) -> &'static str {
    match gltype {
        gl::DEBUG_TYPE_ERROR => "ERROR",
        gl::DEBUG_TYPE_DEPRECATED_BEHAVIOR => "DEPRECATED_BEHAVIOR",
        gl::DEBUG_TYPE_UNDEFINED_BEHAVIOR => "UNDEFINED_BEHAVIOR",
        gl::DEBUG_TYPE_PORTABILITY => "PORTABILITY",
        gl::DEBUG_TYPE_PERFORMANCE => "PERFORMANCE",
        gl::DEBUG_TYPE_OTHER => "OTHER",
        gl::DEBUG_TYPE_MARKER => "MARKER",
        _ => {
            log::error!("Unknown OpenGL debug type 0x{gltype:x}");
            "Unknown type"
        }
    }
}

fn framebuffer_layout_for_present(
    framebuffer_layout: &RwLock<FramebufferLayout>,
) -> FramebufferLayout {
    framebuffer_layout.read().unwrap().clone()
}

fn has_gl_extension(name: &str) -> bool {
    unsafe {
        let mut count = 0;
        gl::GetIntegerv(gl::NUM_EXTENSIONS, &mut count);
        (0..count as u32).any(|index| {
            let pointer = gl::GetStringi(gl::EXTENSIONS, index);
            !pointer.is_null() && CStr::from_ptr(pointer.cast()).to_bytes() == name.as_bytes()
        })
    }
}

#[derive(Debug, Error)]
pub enum OpenGLError {
    #[error("OpenGL initialization failed: {0}")]
    InitFailed(String),
    #[error("Required GL extension missing: {0}")]
    MissingExtension(String),
}

/// Main OpenGL renderer, corresponding to zuyu's `RendererOpenGL`.
///
/// Owns the device info, state tracker, blit screen pipeline, rasterizer,
/// graphics context, and base renderer data.
pub struct RendererOpenGL {
    // Rust drops fields in declaration order. Keep non-owning consumers before
    // the objects they reference, matching C++'s reverse member destruction.
    blit_applet: BlitScreen,
    blit_screen: BlitScreen,
    capture_renderbuffer: OGLRenderbuffer,
    capture_framebuffer: OGLFramebuffer,
    screenshot_framebuffer: OGLFramebuffer,
    rasterizer: Box<RasterizerOpenGL>,
    /// Concrete owner of the shared OpenGL program manager.
    ///
    /// Upstream declares this before `rasterizer`, but C++ destroys members in
    /// reverse order. Rust drops fields in declaration order, so this field is
    /// declared after `rasterizer` to keep the same effective teardown order.
    #[allow(dead_code)]
    program_manager: ProgramManagerHandle,
    state_tracker: Box<StateTracker>,
    device: Box<Device>,
    /// Callback for upstream `gpu.RendererFrameEndNotify()`.
    frame_end_notify: Arc<dyn Fn() + Send + Sync>,
    /// Callback for upstream `render_window.OnFrameDisplayed()`.
    frame_displayed_notify: Arc<dyn Fn() + Send + Sync>,
    /// Common renderer state (frame count, FPS, screenshot settings).
    base_data: RendererBaseData,
    /// Frontend framebuffer layout used for upstream
    /// `emu_window.GetFramebufferLayout()` on every composite.
    framebuffer_layout: Arc<RwLock<FramebufferLayout>>,
    /// Graphics context for swap buffers / make current. It must outlive all
    /// OpenGL resources above.
    /// Upstream: `std::unique_ptr<Core::Frontend::GraphicsContext> context` in RendererBase.
    context: Box<dyn GraphicsContext + Send>,
    /// Frontend-owned equivalent of
    /// `render_window.CreateSharedContext()`. Shader workers and the GPU CPU
    /// thread each request independent shared contexts from this same owner.
    shared_context_factory: gl_shader_context::SharedContextFactory,
}

// The renderer and its OpenGL-owned state move to, then remain on, the render
// thread. Raw non-owning references are only dereferenced on that thread.
unsafe impl Send for RendererOpenGL {}

impl RendererOpenGL {
    /// Create a new RendererOpenGL. Must be called with a current GL context.
    ///
    /// `load_fn` is used to load GL function pointers (typically SDL_GL_GetProcAddress).
    /// `context` is the graphics context used for swap buffers and thread binding.
    ///
    /// Upstream: `RendererOpenGL::RendererOpenGL(emu_window, device_memory, gpu, context)`
    pub fn new<F>(
        mut load_fn: F,
        syncpoints: Arc<SyncpointManager>,
        device_memory: Arc<crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager>,
        shader_notify: crate::shader_notify::ShaderNotifyHandle,
        strict_context_required: bool,
        mut context: Box<dyn GraphicsContext + Send>,
        shared_context_factory: gl_shader_context::SharedContextFactory,
        framebuffer_layout: Arc<RwLock<FramebufferLayout>>,
        frame_end_notify: Arc<dyn Fn() + Send + Sync>,
        frame_displayed_notify: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, OpenGLError>
    where
        F: FnMut(&'static str) -> *const std::os::raw::c_void,
    {
        context.make_current();

        // Load GL function pointers
        gl::load_with(&mut load_fn);
        gl_buffer_cache::load_extra_functions(&mut load_fn);
        gl_graphics_pipeline::load_extra_functions(&mut load_fn);
        gl_shader_util::load_extra_functions(&mut load_fn);
        gl_rasterizer::load_extra_functions(&mut load_fn);
        present::window_adapt_pass::load_extra_functions(&mut load_fn);
        load_renderer_extra_functions(&mut load_fn);
        StateTracker::load_compat_functions(load_fn);

        // Query device capabilities
        let device =
            Box::new(Device::new(strict_context_required).map_err(OpenGLError::InitFailed)?);
        let device_ptr: *const Device = &*device;

        let program_manager = ProgramManager::new_shared(&device);

        let device_memory_reader: crate::renderer_base::DeviceMemoryReader = {
            let device_memory = Arc::clone(&device_memory);
            Arc::new(move |addr, out| {
                let host_ptr = device_memory.get_pointer(addr);
                if host_ptr.is_null() {
                    return false;
                }
                unsafe {
                    std::ptr::copy_nonoverlapping(host_ptr, out.as_mut_ptr(), out.len());
                }
                true
            })
        };

        // Keep the tracker heap-stable: the rasterizer, texture cache, and
        // presentation helpers hold the same non-owning reference as upstream.
        let mut state_tracker = Box::new(StateTracker::new());
        let state_tracker_ptr: *mut StateTracker = state_tracker.as_mut();
        let mut rasterizer = Box::new(RasterizerOpenGL::new(
            &device,
            syncpoints,
            device_memory,
            Arc::clone(&program_manager),
            state_tracker.as_mut(),
            Some(Arc::clone(&shared_context_factory)),
            shader_notify,
        ));
        rasterizer.set_device_memory_reader(Arc::clone(&device_memory_reader));
        let rasterizer_ptr: *mut RasterizerOpenGL = &mut *rasterizer;

        // Install the debug callback before constructing the presentation
        // shaders, matching the first statement in Eden's constructor body.
        unsafe {
            if *common::settings::values().renderer_debug.get_value()
                && has_gl_extension("GL_KHR_debug")
            {
                gl::Enable(gl::DEBUG_OUTPUT);
                gl::Enable(gl::DEBUG_OUTPUT_SYNCHRONOUS);
                gl::DebugMessageCallback(Some(gl_debug_callback), std::ptr::null());
                debug!("OpenGL debug output enabled");
            }
        }

        Self::add_telemetry_fields();

        // Set up initial GL state before constructing the presentation passes.
        unsafe {
            // Initialize vertex attributes to (0, 0, 0, 1)
            let mut max_attribs: i32 = 0;
            gl::GetIntegerv(gl::MAX_VERTEX_ATTRIBS, &mut max_attribs);
            for attrib in 0..max_attribs {
                gl::VertexAttrib4f(attrib as u32, 0.0, 0.0, 0.0, 1.0);
            }

            if !has_gl_extension("GL_ARB_seamless_cubemap_per_texture")
                && !has_gl_extension("GL_AMD_seamless_cubemap_per_texture")
            {
                gl::Enable(gl::TEXTURE_CUBE_MAP_SEAMLESS);
            }

            // Enable vertex buffer unified memory if available (NVIDIA extension).
            if device.has_vertex_buffer_unified_memory() {
                let enable_client_state = GL_ENABLE_CLIENT_STATE
                    .get()
                    .and_then(|entry| *entry)
                    .ok_or_else(|| {
                        OpenGLError::InitFailed(
                            "GL_NV_vertex_buffer_unified_memory is present but glEnableClientState is unavailable"
                                .to_string(),
                        )
                    })?;
                enable_client_state(GL_VERTEX_ATTRIB_ARRAY_UNIFIED_NV);
                enable_client_state(GL_ELEMENT_ARRAY_UNIFIED_NV);
            }
        }

        // Initialize the presentation passes after all constructor GL state,
        // as Eden does. The rasterizer is already heap-stable, so layers may
        // retain its non-owning pointer.
        let blit_screen = BlitScreen::new(
            Arc::clone(&program_manager),
            rasterizer_ptr,
            state_tracker_ptr,
            device_ptr,
            Arc::clone(&device_memory_reader),
            &PRESENT_FILTERS_FOR_DISPLAY,
        );
        let blit_applet = BlitScreen::new(
            Arc::clone(&program_manager),
            rasterizer_ptr,
            state_tracker_ptr,
            device_ptr,
            Arc::clone(&device_memory_reader),
            &PRESENT_FILTERS_FOR_APPLET_CAPTURE,
        );

        // Create capture framebuffer and renderbuffer for applet capture layer.
        // Port of upstream constructor: capture_framebuffer.Create(); capture_renderbuffer.Create();
        // glBindRenderbuffer(...); glRenderbufferStorage(..., GL_SRGB8, LinearWidth, LinearHeight);
        let mut capture_framebuffer = OGLFramebuffer::new();
        capture_framebuffer.create();
        let mut capture_renderbuffer = OGLRenderbuffer::new();
        capture_renderbuffer.create();
        unsafe {
            gl::BindRenderbuffer(gl::RENDERBUFFER, capture_renderbuffer.handle);
            gl::RenderbufferStorage(
                gl::RENDERBUFFER,
                gl::SRGB8,
                capture::LINEAR_WIDTH as i32,
                capture::LINEAR_HEIGHT as i32,
            );
        }

        context.done_current();

        Ok(Self {
            blit_applet,
            blit_screen,
            capture_renderbuffer,
            capture_framebuffer,
            screenshot_framebuffer: OGLFramebuffer::new(),
            rasterizer,
            program_manager,
            state_tracker,
            device,
            frame_end_notify,
            frame_displayed_notify,
            base_data: RendererBaseData::new(),
            framebuffer_layout,
            context,
            shared_context_factory,
        })
    }

    /// Upstream `RendererOpenGL::AddTelemetryFields`.
    fn add_telemetry_fields() {
        let gl_version = gl_string(gl::VERSION);
        let gpu_vendor = gl_string(gl::VENDOR);
        let gpu_model = gl_string(gl::RENDERER);
        info!("GL_VERSION: {}", gl_version);
        info!("GL_VENDOR: {}", gpu_vendor);
        info!("GL_RENDERER: {}", gpu_model);
    }

    pub fn rasterizer_mut(&mut self) -> &mut RasterizerOpenGL {
        &mut self.rasterizer
    }

    /// Composite framebuffers to the screen.
    ///
    /// Port of `RendererOpenGL::Composite()`.
    ///
    /// Upstream flow:
    /// 1. RenderAppletCaptureLayer(framebuffers)
    /// 2. RenderScreenshot(framebuffers)
    /// 3. state_tracker.BindFramebuffer(0)
    /// 4. blit_screen->DrawScreen(framebuffers, layout, false)
    /// 5. ++m_current_frame
    /// 6. gpu.RendererFrameEndNotify()
    /// 7. rasterizer.TickFrame()
    /// 8. context->SwapBuffers()
    /// 9. render_window.OnFrameDisplayed()
    pub fn composite_impl(&mut self, framebuffers: &[FramebufferConfig]) {
        // Upstream reads `emu_window.GetFramebufferLayout()` for every
        // composite. The frontend updates this shared value on each resize.
        let framebuffer_layout = framebuffer_layout_for_present(&self.framebuffer_layout);
        self.context.make_current();

        if framebuffers.is_empty() {
            return;
        }

        self.render_applet_capture_layer(framebuffers);
        self.render_screenshot(framebuffers);

        self.state_tracker.bind_framebuffer(0);
        self.blit_screen
            .draw_screen(framebuffers, &framebuffer_layout, false);

        self.base_data.current_frame += 1;

        (self.frame_end_notify)();
        self.rasterizer.tick_frame();

        self.context.swap_buffers();
        (self.frame_displayed_notify)();
    }

    /// Render the applet capture layer to the capture framebuffer.
    ///
    /// Port of `RendererOpenGL::RenderAppletCaptureLayer()`.
    fn render_applet_capture_layer(&mut self, framebuffers: &[FramebufferConfig]) {
        unsafe {
            let mut old_read_fb = 0;
            let mut old_draw_fb = 0;
            gl::GetIntegerv(gl::READ_FRAMEBUFFER_BINDING, &mut old_read_fb);
            gl::GetIntegerv(gl::DRAW_FRAMEBUFFER_BINDING, &mut old_draw_fb);
            gl::BindFramebuffer(gl::FRAMEBUFFER, self.capture_framebuffer.handle);
            gl::FramebufferRenderbuffer(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::RENDERBUFFER,
                self.capture_renderbuffer.handle,
            );

            self.blit_applet
                .draw_screen(framebuffers, &capture::LAYOUT, true);

            gl::BindFramebuffer(gl::READ_FRAMEBUFFER, old_read_fb as u32);
            gl::BindFramebuffer(gl::DRAW_FRAMEBUFFER, old_draw_fb as u32);
        }
    }

    /// Handle pending screenshot request.
    ///
    /// Port of `RendererOpenGL::RenderScreenshot()`.
    fn render_screenshot(&mut self, framebuffers: &[FramebufferConfig]) {
        if !self.base_data.is_screenshot_pending() {
            return;
        }

        let layout = self
            .base_data
            .settings
            .screenshot_framebuffer_layout
            .clone();
        let dst = self.base_data.settings.screenshot_bits;

        self.render_to_buffer(framebuffers, &layout, dst);

        if let Some(callback) = self.base_data.settings.screenshot_complete_callback.take() {
            callback(true);
        }
        self.base_data
            .settings
            .screenshot_requested
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Render framebuffers to a memory buffer (for screenshots).
    ///
    /// Port of `RendererOpenGL::RenderToBuffer()`.
    fn render_to_buffer(
        &mut self,
        framebuffers: &[FramebufferConfig],
        layout: &crate::renderer_base::FramebufferLayout,
        dst: *mut std::ffi::c_void,
    ) {
        unsafe {
            let mut old_read_fb: i32 = 0;
            let mut old_draw_fb: i32 = 0;
            gl::GetIntegerv(gl::READ_FRAMEBUFFER_BINDING, &mut old_read_fb);
            gl::GetIntegerv(gl::DRAW_FRAMEBUFFER_BINDING, &mut old_draw_fb);

            self.screenshot_framebuffer.create();
            gl::BindFramebuffer(gl::FRAMEBUFFER, self.screenshot_framebuffer.handle);

            let mut renderbuffer: u32 = 0;
            gl::GenRenderbuffers(1, &mut renderbuffer);
            gl::BindRenderbuffer(gl::RENDERBUFFER, renderbuffer);
            gl::RenderbufferStorage(
                gl::RENDERBUFFER,
                gl::SRGB8,
                layout.width as i32,
                layout.height as i32,
            );
            gl::FramebufferRenderbuffer(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::RENDERBUFFER,
                renderbuffer,
            );

            self.blit_screen.draw_screen(framebuffers, layout, false);

            gl::BindBuffer(gl::PIXEL_PACK_BUFFER, 0);
            gl::PixelStorei(gl::PACK_ROW_LENGTH, 0);
            gl::ReadPixels(
                0,
                0,
                layout.width as i32,
                layout.height as i32,
                gl::BGRA,
                gl::UNSIGNED_INT_8_8_8_8_REV,
                dst,
            );

            self.screenshot_framebuffer.release();
            gl::DeleteRenderbuffers(1, &renderbuffer);

            gl::BindFramebuffer(gl::READ_FRAMEBUFFER, old_read_fb as u32);
            gl::BindFramebuffer(gl::DRAW_FRAMEBUFFER, old_draw_fb as u32);
        }
    }

    /// Get the device info.
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Get the vendor name string.
    pub fn device_vendor(&self) -> &str {
        self.device.vendor_name()
    }

    /// Get the current frame count.
    pub fn frame_count(&self) -> i32 {
        self.base_data.current_frame
    }

    /// Tick the rasterizer (end-of-frame cleanup).
    pub fn tick_frame(&mut self) {
        self.rasterizer.tick_frame();
    }
}

impl RendererBase for RendererOpenGL {
    fn context_ptr(&mut self) -> *mut dyn ruzu_core::frontend::graphics_context::GraphicsContext {
        &mut *self.context as *mut dyn ruzu_core::frontend::graphics_context::GraphicsContext
    }

    fn create_shared_context(&self) -> Box<dyn GraphicsContext + Send> {
        (self.shared_context_factory)()
    }

    fn composite(&mut self, layers: &[FramebufferConfig]) {
        self.composite_impl(layers);
    }

    fn request_screenshot(
        &mut self,
        data: *mut std::ffi::c_void,
        callback: Box<dyn FnOnce(bool) + Send>,
        layout: FramebufferLayout,
    ) {
        self.base_data.request_screenshot(data, callback, layout);
    }

    fn set_shader_cache_gpu_reader(&mut self, reader: crate::renderer_base::ShaderCacheGpuReader) {
        // The OpenGL shader cache now compiles graphics pipelines through the
        // channel-owned shared shader cache. Keep forwarding this reader to
        // the rasterizer for compatibility paths outside shader compilation.
        self.rasterizer.set_gpu_memory_reader(reader);
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

    fn get_applet_capture_buffer(&mut self) -> Vec<u8> {
        use crate::capture;
        let tiled_size = capture::TILED_SIZE as usize;
        let mut out = vec![0u8; tiled_size];

        unsafe {
            let mut old_read_fb: i32 = 0;
            let mut old_draw_fb: i32 = 0;
            let mut old_pixel_pack_buffer: i32 = 0;
            let mut old_pack_row_length: i32 = 0;
            gl::GetIntegerv(gl::READ_FRAMEBUFFER_BINDING, &mut old_read_fb);
            gl::GetIntegerv(gl::DRAW_FRAMEBUFFER_BINDING, &mut old_draw_fb);
            gl::GetIntegerv(gl::PIXEL_PACK_BUFFER_BINDING, &mut old_pixel_pack_buffer);
            gl::GetIntegerv(gl::PACK_ROW_LENGTH, &mut old_pack_row_length);

            gl::BindFramebuffer(gl::FRAMEBUFFER, self.capture_framebuffer.handle);
            gl::FramebufferRenderbuffer(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::RENDERBUFFER,
                self.capture_renderbuffer.handle,
            );
            gl::BindBuffer(gl::PIXEL_PACK_BUFFER, 0);
            gl::PixelStorei(gl::PACK_ROW_LENGTH, 0);

            // Read linear pixels from capture renderbuffer.
            let mut linear = vec![0u8; tiled_size];
            gl::ReadPixels(
                0,
                0,
                capture::LINEAR_WIDTH as i32,
                capture::LINEAR_HEIGHT as i32,
                gl::RGBA,
                gl::UNSIGNED_INT_8_8_8_8_REV,
                linear.as_mut_ptr() as *mut _,
            );

            gl::BindFramebuffer(gl::READ_FRAMEBUFFER, old_read_fb as u32);
            gl::BindFramebuffer(gl::DRAW_FRAMEBUFFER, old_draw_fb as u32);
            gl::BindBuffer(gl::PIXEL_PACK_BUFFER, old_pixel_pack_buffer as u32);
            gl::PixelStorei(gl::PACK_ROW_LENGTH, old_pack_row_length);

            crate::textures::decoders::swizzle_texture(
                &mut out,
                &linear,
                capture::BYTES_PER_PIXEL,
                capture::LINEAR_WIDTH,
                capture::LINEAR_HEIGHT,
                capture::LINEAR_DEPTH,
                capture::BLOCK_HEIGHT,
                capture::BLOCK_DEPTH,
                0,
            );
        }

        out
    }

    fn read_rasterizer(&self) -> *mut dyn RasterizerInterface {
        // Safety: We need a raw pointer to the rasterizer for GPU-level access.
        // This matches upstream's ReadRasterizer() returning a raw pointer.
        // Cast through a trait reference to create a wide pointer.
        let trait_ref: &dyn RasterizerInterface = &*self.rasterizer;
        trait_ref as *const dyn RasterizerInterface as *mut dyn RasterizerInterface
    }

    fn get_device_vendor(&self) -> String {
        self.device.vendor_name().to_string()
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
}

fn gl_debug_level(severity: gl::types::GLenum) -> log::Level {
    match severity {
        gl::DEBUG_SEVERITY_HIGH => log::Level::Error,
        gl::DEBUG_SEVERITY_MEDIUM => log::Level::Warn,
        gl::DEBUG_SEVERITY_LOW | gl::DEBUG_SEVERITY_NOTIFICATION => log::Level::Debug,
        _ => log::Level::Debug,
    }
}

/// OpenGL debug message callback (GL_KHR_debug).
extern "system" fn gl_debug_callback(
    source: gl::types::GLenum,
    gltype: gl::types::GLenum,
    id: gl::types::GLuint,
    severity: gl::types::GLenum,
    _length: gl::types::GLsizei,
    message: *const gl::types::GLchar,
    _user_param: *mut std::os::raw::c_void,
) {
    let msg = unsafe {
        std::ffi::CStr::from_ptr(message)
            .to_string_lossy()
            .into_owned()
    };

    let source_str = debug_source(source);
    let type_str = debug_type(gltype);

    log::log!(
        gl_debug_level(severity),
        "{} {} {}: {}",
        source_str,
        type_str,
        id,
        msg
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composite_layout_observes_frontend_updates() {
        use ruzu_core::frontend::framebuffer_layout::Rectangle;

        let layout = RwLock::new(FramebufferLayout {
            width: 1280,
            height: 720,
            screen: Rectangle::new(0, 0, 1280, 720),
            is_srgb: false,
        });
        *layout.write().unwrap() = FramebufferLayout {
            width: 1280,
            height: 674,
            screen: Rectangle::new(40, 0, 1240, 674),
            is_srgb: false,
        };

        let observed = framebuffer_layout_for_present(&layout);
        assert_eq!((observed.width, observed.height), (1280, 674));
        assert_eq!(observed.screen.left, 40);
        assert_eq!(observed.screen.right, 1240);
    }

    #[test]
    fn debug_notifications_are_not_silently_discarded() {
        assert_eq!(gl_debug_level(gl::DEBUG_SEVERITY_HIGH), log::Level::Error);
        assert_eq!(gl_debug_level(gl::DEBUG_SEVERITY_MEDIUM), log::Level::Warn);
        assert_eq!(gl_debug_level(gl::DEBUG_SEVERITY_LOW), log::Level::Debug);
        assert_eq!(
            gl_debug_level(gl::DEBUG_SEVERITY_NOTIFICATION),
            log::Level::Debug
        );
        assert_eq!(
            debug_source(gl::DEBUG_SOURCE_WINDOW_SYSTEM),
            "WINDOW_SYSTEM"
        );
        assert_eq!(
            debug_source(gl::DEBUG_SOURCE_SHADER_COMPILER),
            "SHADER_COMPILER"
        );
        assert_eq!(
            debug_type(gl::DEBUG_TYPE_UNDEFINED_BEHAVIOR),
            "UNDEFINED_BEHAVIOR"
        );
        assert_eq!(debug_type(gl::DEBUG_TYPE_PERFORMANCE), "PERFORMANCE");
    }
}
