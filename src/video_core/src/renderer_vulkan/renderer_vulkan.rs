// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `renderer_vulkan.h` / `renderer_vulkan.cpp`.
//!
//! Top-level Vulkan renderer that owns the device, swapchain, present manager,
//! blit screens, rasterizer, and optional turbo mode.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use ash::vk;

use crate::framebuffer_config::FramebufferConfig;
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::host1x::syncpoint_manager::SyncpointManager;
use crate::present::{PRESENT_FILTERS_FOR_APPLET_CAPTURE, PRESENT_FILTERS_FOR_DISPLAY};
use crate::rasterizer_interface::RasterizerInterface;
use crate::renderer_base::{RendererBase, RendererBaseData};
use crate::textures::decoders;
use crate::vulkan_common::vulkan_debug_callback::{
    create_debug_utils_callback, DebugUtilsMessenger,
};
use crate::vulkan_common::vulkan_device::Device;
use crate::vulkan_common::vulkan_instance;
use crate::vulkan_common::vulkan_library;
use crate::vulkan_common::vulkan_memory_allocator::{MappedBuffer, MemoryAllocator, MemoryUsage};
use crate::vulkan_common::vulkan_surface;
use crate::vulkan_common::vulkan_wrapper::{Instance, VulkanError};
use ruzu_core::frontend::framebuffer_layout::{default_frame_layout, FramebufferLayout, Rectangle};

use super::blit_screen::BlitScreen;
use super::present::util::{
    create_wrapped_image_allocation, create_wrapped_image_view, download_color_image,
};
use super::present_manager::{Frame, PresentManager};
use super::scheduler::Scheduler;
use super::state_tracker::StateTracker;
use super::swapchain::Swapchain;
use super::turbo_mode::TurboMode;

// ---------------------------------------------------------------------------
// Constants (from renderer_vulkan.cpp anonymous namespace)
// ---------------------------------------------------------------------------

/// Capture image format.
/// Maps to `CaptureFormat` in upstream.
pub const CAPTURE_FORMAT: vk::Format = vk::Format::A8B8G8R8_UNORM_PACK32;

/// Capture image size as a VkExtent2D.
pub const CAPTURE_IMAGE_SIZE: vk::Extent2D = vk::Extent2D {
    width: crate::capture::LINEAR_WIDTH,
    height: crate::capture::LINEAR_HEIGHT,
};

/// Capture image extent as a VkExtent3D.
pub const CAPTURE_IMAGE_EXTENT: vk::Extent3D = vk::Extent3D {
    width: crate::capture::LINEAR_WIDTH,
    height: crate::capture::LINEAR_HEIGHT,
    depth: crate::capture::LINEAR_DEPTH,
};

// ---------------------------------------------------------------------------
// Helper functions (from renderer_vulkan.cpp anonymous namespace)
// ---------------------------------------------------------------------------

/// Returns a human-readable Vulkan version string.
/// Port of `GetReadableVersion`.
pub fn get_readable_version(version: u32) -> String {
    format!(
        "{}.{}.{}",
        vk::api_version_major(version),
        vk::api_version_minor(version),
        vk::api_version_patch(version),
    )
}

/// Returns a driver-specific version string.
/// Port of `GetDriverVersion`.
///
/// Nvidia and Intel proprietary drivers encode version numbers differently
/// from the standard Vulkan versioning scheme.
pub fn get_driver_version(driver_id: vk::DriverId, version: u32) -> String {
    if driver_id == vk::DriverId::NVIDIA_PROPRIETARY {
        // Nvidia: 10.8.8.6 bit layout
        let major = (version >> 22) & 0x3ff;
        let minor = (version >> 14) & 0x0ff;
        let secondary = (version >> 6) & 0x0ff;
        let tertiary = version & 0x003f;
        return format!("{}.{}.{}.{}", major, minor, secondary, tertiary);
    }
    if driver_id == vk::DriverId::INTEL_PROPRIETARY_WINDOWS {
        // Intel Windows: 14.14 bit layout
        let major = version >> 14;
        let minor = version & 0x3fff;
        return format!("{}.{}", major, minor);
    }
    // Standard Vulkan version encoding
    get_readable_version(version)
}

/// Builds a comma-separated string of extensions.
/// Port of `BuildCommaSeparatedExtensions`.
pub fn build_comma_separated_extensions(extensions: &[String]) -> String {
    extensions.join(",")
}

fn build_driver_name(vendor_name: &str, driver_version: &str) -> String {
    format!("{} {}", vendor_name, driver_version)
}

fn bytes_to_gib(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0 / 1024.0
}

// ---------------------------------------------------------------------------
// RendererVulkan
// ---------------------------------------------------------------------------

/// Port of `RendererVulkan` class.
///
/// Owns the Vulkan instance, device, memory allocator, scheduler, swapchain,
/// present manager, blit screens (swapchain / capture / applet), rasterizer,
/// and optional turbo mode.
///
/// The full field set requires cross-crate types (Device, MemoryAllocator,
/// StateTracker, Scheduler, Swapchain, PresentManager, BlitScreen,
/// RasterizerVulkan, TurboMode) which are all declared in sibling modules.
pub struct RendererVulkan {
    // Rust drops fields in declaration order, unlike C++'s reverse declaration
    // order. Keep Vulkan owners here in upstream destruction order: dependent
    // resources, device, surface, then instance.
    /// Applet capture frame. Its raw Vulkan handles are released in `Drop`.
    applet_frame: Frame,
    /// Optional maximum-clock workload owner.
    #[allow(dead_code)]
    turbo_mode: Option<TurboMode>,
    /// Vulkan rasterizer owner.
    rasterizer: super::RasterizerVulkan,
    /// Applet capture blit/composition owner.
    blit_applet: BlitScreen,
    /// Screenshot/capture blit/composition owner.
    blit_capture: BlitScreen,
    /// Swapchain blit/composition owner.
    blit_swapchain: BlitScreen,
    /// Presentation frame manager.
    present_manager: PresentManager,
    /// Presentation swapchain owner.
    /// Shared with the present thread (upstream `Swapchain& swapchain` used
    /// from `PresentThread` under `swapchain_mutex`).
    swapchain: std::sync::Arc<std::sync::Mutex<Swapchain>>,
    /// Presentation command scheduler.
    ///
    /// Boxed so the non-owning references held by `RasterizerVulkan` remain
    /// stable when this renderer moves. Upstream owns this single scheduler
    /// here and passes it by reference to the rasterizer.
    scheduler: Box<Scheduler>,
    /// Vulkan command-buffer state tracker.
    ///
    /// Like `scheduler`, this is a renderer-owned upstream member boxed only
    /// to provide a stable address for Rust's non-owning owner bridge.
    #[allow(dead_code)]
    state_tracker: Box<StateTracker>,
    /// Vulkan memory allocator owner.
    memory_allocator: Box<MemoryAllocator>,
    /// Physical/logical Vulkan device owner.
    device: Box<Device>,
    /// Presentation surface owner.
    #[allow(dead_code)]
    surface: Arc<std::sync::Mutex<OwnedSurface>>,
    /// Validation callback owner, present when renderer debugging is enabled.
    #[allow(dead_code)]
    debug_messenger: Option<DebugUtilsMessenger>,
    /// Vulkan instance owner.
    #[allow(dead_code)]
    instance: Instance,

    /// Shared Tegra device memory manager used by presentation uploads.
    device_memory: Arc<MaxwellDeviceMemoryManager>,
    /// Frontend visibility state used for upstream `render_window.IsShown()`.
    window_shown: Arc<AtomicBool>,
    /// Frontend framebuffer layout used for upstream `render_window.GetFramebufferLayout()`.
    framebuffer_layout: Arc<RwLock<FramebufferLayout>>,
    /// Callback for upstream `render_window.OnFrameDisplayed()`.
    frame_displayed_notify: Arc<dyn Fn() + Send + Sync>,
    /// Callback for upstream `gpu.RendererFrameEndNotify()`.
    frame_end_notify: Arc<dyn Fn() + Send + Sync>,
    /// RendererBase shared state.
    base_data: RendererBaseData,
    /// Vulkan does not require a shared GL-style context.
    dummy_context: VulkanDummyContext,
}

unsafe impl Send for RendererVulkan {}

/// RAII counterpart of upstream `vk::SurfaceKHR`.
///
/// It is a separate owner so Rust can destroy the surface after the logical
/// device and before the instance, matching the effective C++ member order.
pub(super) struct OwnedSurface {
    loader: ash::extensions::khr::Surface,
    handle: vk::SurfaceKHR,
}

impl OwnedSurface {
    pub(super) fn new(loader: ash::extensions::khr::Surface, handle: vk::SurfaceKHR) -> Self {
        Self { loader, handle }
    }

    pub(super) fn handle(&self) -> vk::SurfaceKHR {
        self.handle
    }

    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub(super) fn replace(&mut self, handle: vk::SurfaceKHR) {
        if self.handle != vk::SurfaceKHR::null() {
            unsafe {
                self.loader.destroy_surface(self.handle, None);
            }
        }
        self.handle = handle;
    }
}

impl Drop for OwnedSurface {
    fn drop(&mut self) {
        if self.handle != vk::SurfaceKHR::null() {
            unsafe {
                self.loader.destroy_surface(self.handle, None);
            }
            self.handle = vk::SurfaceKHR::null();
        }
    }
}

struct VulkanDummyContext;

impl ruzu_core::frontend::graphics_context::GraphicsContext for VulkanDummyContext {}

impl RendererVulkan {
    /// Port of `RendererVulkan::RendererVulkan`.
    ///
    /// In the full implementation, this constructor chain-initializes:
    /// library, instance (with debug if enabled), debug_messenger, surface,
    /// device (via CreateDevice), memory_allocator, state_tracker, scheduler,
    /// swapchain, present_manager, blit_swapchain, blit_capture, blit_applet,
    /// rasterizer, and optionally turbo_mode.
    pub fn new(
        shader_notify: crate::shader_notify::ShaderNotifyHandle,
        window_info: &ruzu_core::frontend::emu_window::WindowSystemInfo,
        drawable_size: (u32, u32),
        window_shown: Arc<AtomicBool>,
        framebuffer_layout: Arc<RwLock<FramebufferLayout>>,
        frame_displayed_notify: Arc<dyn Fn() + Send + Sync>,
        frame_end_notify: Arc<dyn Fn() + Send + Sync>,
        syncpoints: Arc<SyncpointManager>,
        device_memory: Arc<MaxwellDeviceMemoryManager>,
    ) -> Result<Self, VulkanError> {
        let entry = vulkan_library::open_library()?;
        let window_type = map_window_type(window_info.type_)?;
        let instance = vulkan_instance::create_instance(
            entry,
            vk::API_VERSION_1_1,
            window_type,
            *common::settings::values().renderer_debug.get_value(),
        )?;
        let debug_messenger = if *common::settings::values().renderer_debug.get_value() {
            Some(create_debug_utils_callback(
                &instance.entry,
                &instance.instance,
            )?)
        } else {
            None
        };
        let surface_info = vulkan_surface::WindowSystemInfo {
            window_type,
            display_connection: window_info.display_connection as *mut std::ffi::c_void,
            render_surface: window_info.render_surface as *mut std::ffi::c_void,
        };
        let surface_handle = unsafe {
            vulkan_surface::create_surface(&instance.entry, &instance.instance, &surface_info)?
        };
        let surface_loader =
            ash::extensions::khr::Surface::new(&instance.entry, &instance.instance);
        let surface = Arc::new(std::sync::Mutex::new(OwnedSurface::new(
            surface_loader,
            surface_handle,
        )));

        let device = Box::new(create_device(&instance, surface.lock().unwrap().handle())?);
        // `RasterizerVulkan` and its `PipelineCache` retain the upstream
        // `const Device&`. Box the owner before constructing either borrower
        // so its address remains stable when `RendererVulkan` is returned.
        let mut memory_allocator = Box::new(MemoryAllocator::new(&device));
        let mut state_tracker = Box::new(StateTracker::new());
        let device_fault = device.is_device_fault_supported().then(|| {
            vk::ExtDeviceFaultFn::load(|name| unsafe {
                instance
                    .instance
                    .get_device_proc_addr(device.get_logical().handle(), name.as_ptr())
                    .map_or(std::ptr::null(), |function| {
                        function as *const std::ffi::c_void
                    })
            })
        });
        let mut scheduler = Box::new(
            Scheduler::new(
                device.get_logical().clone(),
                device.get_graphics_queue(),
                device.get_graphics_family(),
                device.is_timeline_semaphore_supported(),
                device.has_synchronization2() && device.api_version() >= vk::API_VERSION_1_3,
                device.synchronization2_extension().cloned(),
                device_fault,
                device.is_ext_transform_feedback_supported(),
            )
            .map_err(VulkanError::new)?,
        );
        scheduler.set_state_tracker(std::ptr::NonNull::from(state_tracker.as_mut()));
        let submit_mutex = scheduler.submit_mutex();
        let (surface_loader, surface_handle) = {
            let surface = surface.lock().unwrap();
            (surface.loader.clone(), surface.handle())
        };
        let swapchain = Swapchain::new(
            &instance.instance,
            surface_loader,
            surface_handle,
            &device,
            submit_mutex.clone(),
            drawable_size.0.max(1),
            drawable_size.1.max(1),
        )?;
        let swapchain_image_count = swapchain.get_image_count();
        let swapchain = std::sync::Arc::new(std::sync::Mutex::new(swapchain));
        // Upstream gates the present thread on `Settings::values.async_presentation`.
        let use_present_thread = *common::settings::values().async_presentation.get_value();
        let present_manager = PresentManager::new(
            instance.entry.clone(),
            instance.instance.clone(),
            surface_info,
            Arc::clone(&surface),
            device.as_ref(),
            memory_allocator.as_mut(),
            device.get_graphics_family(),
            swapchain_image_count,
            use_present_thread,
            submit_mutex,
            std::sync::Arc::clone(&swapchain),
            device.get_graphics_queue(),
        );
        let blit_swapchain = BlitScreen::new(&PRESENT_FILTERS_FOR_DISPLAY);
        let blit_capture = BlitScreen::new(&PRESENT_FILTERS_FOR_DISPLAY);
        let blit_applet = BlitScreen::new(&PRESENT_FILTERS_FOR_APPLET_CAPTURE);
        let rasterizer = super::RasterizerVulkan::new(
            shader_notify,
            device.as_ref(),
            instance.instance.clone(),
            device.get_physical(),
            device.get_driver_id(),
            device.cant_blit_msaa(),
            device.is_depth_bounds_supported(),
            device.is_ext_depth_range_unrestricted_supported(),
            device.is_nv_viewport_swizzle_supported(),
            device.is_ext_index_type_uint8_supported(),
            device.has_null_descriptor(),
            device.is_ext_extended_dynamic_state_supported(),
            device.is_ext_transform_feedback_supported(),
            device.is_host_query_reset_supported(),
            device.is_subgroup_feature_supported(
                vk::SubgroupFeatureFlags::BASIC
                    | vk::SubgroupFeatureFlags::ARITHMETIC
                    | vk::SubgroupFeatureFlags::SHUFFLE
                    | vk::SubgroupFeatureFlags::SHUFFLE_RELATIVE,
            ),
            device.is_ext_conditional_rendering(),
            device.is_ext_extended_dynamic_state2_supported(),
            device.is_ext_extended_dynamic_state2_extras_supported(),
            device.is_ext_extended_dynamic_state3_blending_supported(),
            device.is_ext_extended_dynamic_state3_enables_supported(),
            device.is_ext_color_write_enable_supported(),
            super::graphics_pipeline::DynamicState3Support {
                depth_clamp_enable: device.supports_dynamic_state3_depth_clamp_enable(),
                logic_op_enable: device.supports_dynamic_state3_logic_op_enable(),
                line_rasterization_mode: device.supports_dynamic_state3_line_rasterization_mode(),
                conservative_rasterization_mode: device
                    .supports_dynamic_state3_conservative_rasterization_mode(),
                line_stipple_enable: device.supports_dynamic_state3_line_stipple_enable(),
                alpha_to_coverage_enable: device.supports_dynamic_state3_alpha_to_coverage_enable(),
                alpha_to_one_enable: device.supports_dynamic_state3_alpha_to_one_enable(),
            },
            device.is_ext_line_rasterization_supported(),
            device.supports_smooth_lines(),
            device.is_ext_vertex_input_dynamic_state_supported(),
            device.is_topology_list_primitive_restart_supported(),
            device.is_patch_list_primitive_restart_supported(),
            device.must_emulate_scaled_formats(),
            device.must_emulate_bgr565(),
            device.is_ext_4444_formats_supported(),
            device.is_khr_image_format_list_supported(),
            device.is_optimal_astc_supported(),
            device.is_ext_custom_border_color_supported(),
            device.is_ext_sampler_filter_minmax_supported(),
            device.get_max_viewports(),
            device.get_max_vertex_input_attributes(),
            device.get_max_vertex_input_bindings(),
            device.get_max_compute_work_group_count(),
            device.is_khr_draw_indirect_count_supported(),
            device.is_khr_push_descriptor_supported(),
            syncpoints,
            Arc::clone(&device_memory),
            memory_allocator.as_mut(),
            state_tracker.as_mut(),
            scheduler.as_mut(),
        )
        .map_err(|err| {
            log::error!("Failed to initialize Vulkan rasterizer: {}", err);
            VulkanError::new(vk::Result::ERROR_INITIALIZATION_FAILED)
        })?;

        let turbo_mode = if *common::settings::values()
            .renderer_force_max_clock
            .get_value()
            && device.should_boost_clocks()
        {
            let turbo_mode =
                TurboMode::new(&instance.entry, &instance.instance, device.get_physical())?;
            scheduler.register_on_submit(Some(turbo_mode.submit_callback()));
            Some(turbo_mode)
        } else {
            None
        };

        let renderer = RendererVulkan {
            applet_frame: Frame::default(),
            turbo_mode,
            rasterizer,
            blit_applet,
            blit_capture,
            blit_swapchain,
            present_manager,
            swapchain,
            scheduler,
            state_tracker,
            memory_allocator,
            device,
            surface,
            debug_messenger,
            instance,
            device_memory,
            window_shown,
            framebuffer_layout,
            frame_displayed_notify,
            frame_end_notify,
            base_data: RendererBaseData::new(),
            dummy_context: VulkanDummyContext,
        };
        renderer.report();
        Ok(renderer)
    }

    /// Port of `RendererVulkan::Composite`.
    ///
    /// Renders the given framebuffers to the display. This is the main
    /// per-frame entry point called by the GPU thread.
    ///
    /// Upstream flow:
    /// 1. RenderAppletCaptureLayer
    /// 2. Early-return if window not shown
    /// 3. RenderScreenshot
    /// 4. Get render frame from present manager
    /// 5. Draw to frame via blit_swapchain
    /// 6. Flush scheduler with render_ready semaphore
    /// 7. Present frame
    /// 8. Notify GPU of frame end
    /// 9. Tick rasterizer frame
    pub fn composite_impl(&mut self, framebuffers: &[FramebufferConfig]) {
        let _frame_displayed = FrameDisplayedNotifyGuard::new(&self.frame_displayed_notify);
        self.render_applet_capture_layer(framebuffers);
        if !should_present_window(&self.window_shown) {
            return;
        }
        let layout = self.current_framebuffer_layout_for_present();
        self.render_screenshot(framebuffers);

        let frame_index = self.present_manager.get_render_frame_index();
        // Upstream reads these swapchain getters without a lock
        // (renderer_vulkan.cpp:163). Locking `swapchain_mutex` here stalled
        // the GPU thread behind the present thread, which holds that mutex
        // across `acquire_next_image` (MoltenVK blocks on the next drawable
        // — up to a vsync period per presented frame; measured at 37% of
        // both values in atomics updated at swapchain (re)creation.
        let swapchain_image_count = self.present_manager.swapchain_image_count();
        let swapchain_image_view_format = self.present_manager.swapchain_image_view_format();
        self.scheduler
            .request_outside_render_pass_operation_context();
        self.blit_swapchain.draw_to_present_frame(
            self.device.as_ref(),
            &mut self.rasterizer,
            &mut self.scheduler,
            &mut self.present_manager,
            &self.memory_allocator,
            &self.device_memory,
            frame_index,
            framebuffers,
            &layout,
            swapchain_image_count,
            swapchain_image_view_format,
        );

        let render_ready = self.present_manager.frame(frame_index).render_ready;
        self.scheduler.flush_with_signal(render_ready);
        self.present_manager
            .present(frame_index, &mut self.scheduler);
        (self.frame_end_notify)();
        self.rasterizer.tick_frame();
    }

    /// Port of `RendererVulkan::GetAppletCaptureBuffer`.
    ///
    /// Downloads the applet capture image from GPU to CPU and returns the
    /// pixel data as a byte vector.
    pub fn get_applet_capture_buffer(&mut self) -> Vec<u8> {
        let mut out = vec![0; crate::capture::TILED_SIZE as usize];

        if self.applet_frame.image == vk::Image::null() {
            return out;
        }

        let dst_buffer = self.create_download_buffer(crate::capture::TILED_SIZE as vk::DeviceSize);
        self.scheduler
            .request_outside_render_pass_operation_context();
        let device = self.device.get_logical().clone();
        let image = self.applet_frame.image;
        let buffer = dst_buffer.buffer();
        self.scheduler.record(move |cmdbuf| {
            download_color_image(&device, cmdbuf, image, buffer, CAPTURE_IMAGE_EXTENT);
        });
        self.scheduler.finish();
        dst_buffer.invalidate();
        decoders::swizzle_texture(
            &mut out,
            dst_buffer.mapped_slice(),
            crate::capture::BYTES_PER_PIXEL,
            crate::capture::LINEAR_WIDTH,
            crate::capture::LINEAR_HEIGHT,
            crate::capture::LINEAR_DEPTH,
            crate::capture::BLOCK_HEIGHT,
            crate::capture::BLOCK_DEPTH,
            0,
        );
        out
    }

    /// Port of `RendererVulkan::GetDeviceVendor`.
    pub fn get_device_vendor(&self) -> String {
        self.device.get_driver_name()
    }

    /// Port of `RendererVulkan::Report`.
    ///
    /// Logs the four device information fields emitted by upstream.
    fn report(&self) {
        let vendor_name = self.device.get_vendor_name();
        let model_name = self.device.get_model_name();
        let driver_version = get_driver_version(
            self.device.get_driver_id(),
            self.device.get_driver_version(),
        );
        let driver_name = build_driver_name(&vendor_name, &driver_version);
        let api_version = get_readable_version(self.device.api_version());
        let extensions = self
            .device
            .get_available_extensions()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let extensions = build_comma_separated_extensions(&extensions);
        let available_vram = bytes_to_gib(self.device.get_device_local_memory());

        log::info!("Driver: {}", driver_name);
        log::info!("Device: {}", model_name);
        log::info!("Vulkan: {}", api_version);
        log::info!("Available VRAM: {:.2} GiB", available_vram);
        let _ = extensions;
    }

    /// Port of `RendererVulkan::RenderToBuffer`.
    ///
    /// Creates a temporary frame, draws framebuffers to it via blit_capture,
    /// and copies the result to a buffer for readback.
    fn render_to_buffer(
        &mut self,
        framebuffers: &[FramebufferConfig],
        layout: &FramebufferLayout,
        format: vk::Format,
        buffer_size: vk::DeviceSize,
    ) -> MappedBuffer {
        let mut frame = Frame::default();
        let image = create_wrapped_image_allocation(
            self.memory_allocator.as_ref(),
            vk::Extent2D {
                width: layout.width,
                height: layout.height,
            },
            format,
        );
        frame.set_image_allocation(image);
        frame.image_view =
            create_wrapped_image_view(self.device.get_logical(), frame.image, format);
        frame.framebuffer = self.blit_capture.create_framebuffer(
            self.device.as_ref(),
            &mut self.scheduler,
            &self.present_manager,
            layout,
            frame.image_view,
            format,
        );

        let dst_buffer = self.create_download_buffer(buffer_size);
        self.blit_capture.draw_to_frame(
            self.device.as_ref(),
            &mut self.rasterizer,
            &mut self.scheduler,
            &self.present_manager,
            &self.memory_allocator,
            &self.device_memory,
            &mut frame,
            framebuffers,
            layout,
            1,
            format,
        );

        self.scheduler
            .request_outside_render_pass_operation_context();
        let device = self.device.get_logical().clone();
        let image = frame.image;
        let buffer = dst_buffer.buffer();
        let extent = vk::Extent3D {
            width: layout.width,
            height: layout.height,
            depth: 1,
        };
        self.scheduler.record(move |cmdbuf| {
            download_color_image(&device, cmdbuf, image, buffer, extent);
        });
        self.scheduler.finish();
        dst_buffer.invalidate();

        unsafe {
            if frame.framebuffer != vk::Framebuffer::null() {
                self.device
                    .get_logical()
                    .destroy_framebuffer(frame.framebuffer, None);
                frame.framebuffer = vk::Framebuffer::null();
            }
            if frame.image_view != vk::ImageView::null() {
                self.device
                    .get_logical()
                    .destroy_image_view(frame.image_view, None);
                frame.image_view = vk::ImageView::null();
            }
        }
        dst_buffer
    }

    /// Port of `RendererVulkan::RenderScreenshot`.
    ///
    /// If a screenshot is requested, renders to a buffer at the appropriate
    /// resolution and format, then saves or delivers the result.
    fn render_screenshot(&mut self, framebuffers: &[FramebufferConfig]) {
        if !self.base_data.is_screenshot_pending() {
            return;
        }

        let screenshot_layout = self
            .base_data
            .settings
            .screenshot_framebuffer_layout
            .clone();
        let layout = FramebufferLayout {
            width: screenshot_layout.width,
            height: screenshot_layout.height,
            screen: Rectangle::new(0, 0, screenshot_layout.width, screenshot_layout.height),
            is_srgb: false,
        };
        let buffer_size = layout.width as vk::DeviceSize * layout.height as vk::DeviceSize * 4;
        let dst_buffer = self.render_to_buffer(
            framebuffers,
            &layout,
            vk::Format::B8G8R8A8_UNORM,
            buffer_size,
        );
        let dst = self.base_data.settings.screenshot_bits.cast::<u8>();
        if !dst.is_null() {
            let copy_len = buffer_size as usize;
            unsafe {
                std::ptr::copy_nonoverlapping(dst_buffer.mapped_slice().as_ptr(), dst, copy_len);
            }
        }
        if let Some(callback) = self.base_data.settings.screenshot_complete_callback.take() {
            callback(false);
        }
        self.base_data
            .settings
            .screenshot_requested
            .store(false, Ordering::SeqCst);
    }

    /// Port of `RendererVulkan::RenderAppletCaptureLayer`.
    ///
    /// Renders framebuffers to the applet capture frame at 1280x720
    /// using the applet-specific blit screen and filter configuration.
    fn render_applet_capture_layer(&mut self, framebuffers: &[FramebufferConfig]) {
        let layout = capture_framebuffer_layout();
        if self.applet_frame.image == vk::Image::null() {
            let image = create_wrapped_image_allocation(
                self.memory_allocator.as_ref(),
                CAPTURE_IMAGE_SIZE,
                CAPTURE_FORMAT,
            );
            self.applet_frame.set_image_allocation(image);
            self.applet_frame.image_view = create_wrapped_image_view(
                self.device.get_logical(),
                self.applet_frame.image,
                CAPTURE_FORMAT,
            );
            self.applet_frame.framebuffer = self.blit_applet.create_framebuffer(
                self.device.as_ref(),
                &mut self.scheduler,
                &self.present_manager,
                &layout,
                self.applet_frame.image_view,
                CAPTURE_FORMAT,
            );
        }

        self.blit_applet.draw_to_frame(
            self.device.as_ref(),
            &mut self.rasterizer,
            &mut self.scheduler,
            &self.present_manager,
            &self.memory_allocator,
            &self.device_memory,
            &mut self.applet_frame,
            framebuffers,
            &layout,
            1,
            CAPTURE_FORMAT,
        );
    }

    fn create_download_buffer(&self, size: vk::DeviceSize) -> MappedBuffer {
        let ci = vk::BufferCreateInfo::builder()
            .size(size.max(1))
            .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .build();
        self.memory_allocator
            .create_mapped_buffer(&ci, MemoryUsage::Download)
            .expect("Failed to create Vulkan download buffer")
    }

    /// Keep the present layout synchronized with the cached WSI surface extent.
    ///
    /// Upstream relies on `EmuWindow_SDL2::OnResize` updating
    /// `render_window.GetFramebufferLayout()` before `RendererVulkan::Composite`.
    /// On macOS/MoltenVK, the present thread can hold the swapchain mutex while
    /// the WSI layer waits for a drawable. Upstream does not block
    /// `RendererVulkan::Composite` on a swapchain query for layout; if the
    /// mutex is busy, use the frontend-provided layout for this frame.
    fn current_framebuffer_layout_for_present(&self) -> FramebufferLayout {
        let layout = self.framebuffer_layout.read().unwrap().clone();
        let Ok(swapchain) = self.swapchain.try_lock() else {
            return layout;
        };
        let extent = swapchain.get_extent();
        if extent.width == 0 || extent.height == 0 {
            return layout;
        }
        if extent.width == layout.width && extent.height == layout.height {
            return layout;
        }

        let updated = default_frame_layout(extent.width, extent.height);
        *self.framebuffer_layout.write().unwrap() = updated.clone();
        updated
    }
}

impl Drop for RendererVulkan {
    /// Port of `RendererVulkan::~RendererVulkan`.
    ///
    /// Upstream: clears the scheduler on-submit callback, then waits for
    /// the device to become idle before destruction.
    fn drop(&mut self) {
        self.scheduler.register_on_submit(None);
        unsafe {
            self.device.get_logical().device_wait_idle().ok();
            if self.applet_frame.framebuffer != vk::Framebuffer::null() {
                self.device
                    .get_logical()
                    .destroy_framebuffer(self.applet_frame.framebuffer, None);
                self.applet_frame.framebuffer = vk::Framebuffer::null();
            }
            if self.applet_frame.image_view != vk::ImageView::null() {
                self.device
                    .get_logical()
                    .destroy_image_view(self.applet_frame.image_view, None);
                self.applet_frame.image_view = vk::ImageView::null();
            }
        }
    }
}

impl RendererBase for RendererVulkan {
    fn context_ptr(&mut self) -> *mut dyn ruzu_core::frontend::graphics_context::GraphicsContext {
        &mut self.dummy_context as *mut dyn ruzu_core::frontend::graphics_context::GraphicsContext
    }

    fn composite(&mut self, layers: &[FramebufferConfig]) {
        self.composite_impl(layers);
    }

    fn get_applet_capture_buffer(&mut self) -> Vec<u8> {
        RendererVulkan::get_applet_capture_buffer(self)
    }

    fn read_rasterizer(&self) -> *mut dyn RasterizerInterface {
        let trait_ref: &dyn RasterizerInterface = &self.rasterizer;
        trait_ref as *const dyn RasterizerInterface as *mut dyn RasterizerInterface
    }

    fn get_device_vendor(&self) -> String {
        RendererVulkan::get_device_vendor(self)
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

fn map_window_type(
    window_type: ruzu_core::frontend::emu_window::WindowSystemType,
) -> Result<vulkan_instance::WindowSystemType, VulkanError> {
    match window_type {
        ruzu_core::frontend::emu_window::WindowSystemType::Headless => {
            Ok(vulkan_instance::WindowSystemType::Headless)
        }
        #[cfg(target_os = "linux")]
        ruzu_core::frontend::emu_window::WindowSystemType::X11 => {
            Ok(vulkan_instance::WindowSystemType::X11)
        }
        #[cfg(target_os = "linux")]
        ruzu_core::frontend::emu_window::WindowSystemType::Wayland => {
            Ok(vulkan_instance::WindowSystemType::Wayland)
        }
        #[cfg(target_os = "macos")]
        ruzu_core::frontend::emu_window::WindowSystemType::Cocoa => {
            Ok(vulkan_instance::WindowSystemType::Cocoa)
        }
        #[cfg(target_os = "windows")]
        ruzu_core::frontend::emu_window::WindowSystemType::Windows => {
            Ok(vulkan_instance::WindowSystemType::Windows)
        }
        #[cfg(target_os = "android")]
        ruzu_core::frontend::emu_window::WindowSystemType::Android => {
            Ok(vulkan_instance::WindowSystemType::Android)
        }
        _ => {
            log::error!("Unsupported Vulkan window system: {:?}", window_type);
            Err(VulkanError::new(vk::Result::ERROR_INITIALIZATION_FAILED))
        }
    }
}

fn capture_framebuffer_layout() -> FramebufferLayout {
    FramebufferLayout {
        width: crate::capture::LINEAR_WIDTH,
        height: crate::capture::LINEAR_HEIGHT,
        screen: Rectangle::new(
            0,
            0,
            crate::capture::LINEAR_WIDTH,
            crate::capture::LINEAR_HEIGHT,
        ),
        is_srgb: false,
    }
}

fn should_present_window(window_shown: &AtomicBool) -> bool {
    window_shown.load(Ordering::Relaxed)
}

struct FrameDisplayedNotifyGuard {
    notify: Arc<dyn Fn() + Send + Sync>,
}

impl FrameDisplayedNotifyGuard {
    fn new(notify: &Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            notify: Arc::clone(notify),
        }
    }
}

impl Drop for FrameDisplayedNotifyGuard {
    fn drop(&mut self) {
        (self.notify.as_ref())();
    }
}

/// Port of free function `CreateDevice`.
pub fn create_device(instance: &Instance, surface: vk::SurfaceKHR) -> Result<Device, VulkanError> {
    let devices = unsafe {
        instance
            .instance
            .enumerate_physical_devices()
            .map_err(VulkanError::new)?
    };
    let device_index = *common::settings::values().vulkan_device.get_value();
    let selected_index = validate_physical_device_index(device_index, devices.len())?;
    Device::new(
        &instance.entry,
        instance.instance.clone(),
        devices[selected_index],
        surface,
    )
}

fn validate_physical_device_index(
    device_index: u32,
    device_count: usize,
) -> Result<usize, VulkanError> {
    if device_index >= device_count as u32 {
        log::error!("Invalid Vulkan device index {}!", device_index);
        return Err(VulkanError::new(vk::Result::ERROR_INITIALIZATION_FAILED));
    }
    Ok(device_index as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_version() {
        let version = vk::make_api_version(0, 1, 3, 250);
        let s = get_readable_version(version);
        assert_eq!(s, "1.3.250");
    }

    #[test]
    fn nvidia_driver_version() {
        // Nvidia version 525.60.11 encodes differently
        let s = get_driver_version(
            vk::DriverId::NVIDIA_PROPRIETARY,
            (525 << 22) | (60 << 14) | (11 << 6) | 0,
        );
        assert!(s.starts_with("525.60.11."));
    }

    #[test]
    fn standard_driver_version() {
        let version = vk::make_api_version(0, 23, 1, 4);
        let s = get_driver_version(vk::DriverId::MESA_RADV, version);
        assert_eq!(s, "23.1.4");
    }

    #[test]
    fn capture_constants() {
        assert_eq!(CAPTURE_IMAGE_SIZE.width, crate::capture::LINEAR_WIDTH);
        assert_eq!(CAPTURE_IMAGE_SIZE.height, crate::capture::LINEAR_HEIGHT);
        assert_eq!(CAPTURE_IMAGE_EXTENT.depth, crate::capture::LINEAR_DEPTH);
        assert_eq!(CAPTURE_FORMAT, vk::Format::A8B8G8R8_UNORM_PACK32);
    }

    #[test]
    fn physical_device_index_validation_matches_upstream_bounds() {
        assert_eq!(validate_physical_device_index(0, 1).unwrap(), 0);
        assert_eq!(validate_physical_device_index(1, 2).unwrap(), 1);
        assert!(validate_physical_device_index(2, 2).is_err());
        assert!(validate_physical_device_index(0, 0).is_err());
    }

    #[test]
    fn window_visibility_gate_matches_composite_branch() {
        let shown = AtomicBool::new(true);
        assert!(should_present_window(&shown));
        shown.store(false, Ordering::Relaxed);
        assert!(!should_present_window(&shown));
    }

    #[test]
    fn frame_displayed_notify_guard_matches_scope_exit() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_clone = Arc::clone(&calls);
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            calls_clone.fetch_add(1, Ordering::Relaxed);
        });

        {
            let _guard = FrameDisplayedNotifyGuard::new(&notify);
            assert_eq!(calls.load(Ordering::Relaxed), 0);
        }

        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn driver_name_builder_matches_report_format() {
        assert_eq!(
            build_driver_name("NVIDIA", "525.60.11.0"),
            "NVIDIA 525.60.11.0"
        );
        assert_eq!(build_driver_name("", "1.2.3"), " 1.2.3");
        assert_eq!(build_driver_name("MoltenVK", ""), "MoltenVK ");
    }

    #[test]
    fn bytes_to_gib_matches_upstream_report_units() {
        assert_eq!(bytes_to_gib(0), 0.0);
        assert_eq!(bytes_to_gib(1024 * 1024 * 1024), 1.0);
        assert_eq!(bytes_to_gib(5 * 1024 * 1024 * 1024), 5.0);
    }

    #[test]
    fn comma_separated_extensions() {
        let exts = vec!["VK_KHR_swapchain".into(), "VK_EXT_debug_utils".into()];
        assert_eq!(
            build_comma_separated_extensions(&exts),
            "VK_KHR_swapchain,VK_EXT_debug_utils"
        );
    }
}
