// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `vk_present_manager.h` / `vk_present_manager.cpp`.
//!
//! Manages presentation frames, a present thread, and swapchain copies.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use ash::vk;

use super::renderer_vulkan::OwnedSurface;
use super::scheduler::Scheduler;
use super::swapchain::Swapchain;
use crate::vulkan_common::vulkan_device::Device;
use crate::vulkan_common::vulkan_surface;
use crate::vulkan_common::vulkan_wrapper::PIPELINE_STAGE_GRAPHICS_COMPUTE_TRANSFER;

// ---------------------------------------------------------------------------
// Helper functions (port of anonymous namespace)
// ---------------------------------------------------------------------------

/// Port of `MakeImageSubresourceLayers`.
fn make_image_subresource_layers() -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        mip_level: 0,
        base_array_layer: 0,
        layer_count: 1,
    }
}

/// Port of the anonymous-namespace `CanBlitToSwapchain` helper.
fn can_blit_to_swapchain(device: &Device, format: vk::Format) -> bool {
    device
        .format_properties(format)
        .optimal_tiling_features
        .contains(vk::FormatFeatureFlags::BLIT_DST)
}

/// Port of `MakeImageBlit`.
fn make_image_blit(
    frame_width: i32,
    frame_height: i32,
    swapchain_width: i32,
    swapchain_height: i32,
) -> vk::ImageBlit {
    vk::ImageBlit {
        src_subresource: make_image_subresource_layers(),
        src_offsets: [
            vk::Offset3D { x: 0, y: 0, z: 0 },
            vk::Offset3D {
                x: frame_width,
                y: frame_height,
                z: 1,
            },
        ],
        dst_subresource: make_image_subresource_layers(),
        dst_offsets: [
            vk::Offset3D { x: 0, y: 0, z: 0 },
            vk::Offset3D {
                x: swapchain_width,
                y: swapchain_height,
                z: 1,
            },
        ],
    }
}

/// Port of `MakeImageCopy`.
fn make_image_copy(
    frame_width: u32,
    frame_height: u32,
    swapchain_width: u32,
    swapchain_height: u32,
) -> vk::ImageCopy {
    vk::ImageCopy {
        src_subresource: make_image_subresource_layers(),
        src_offset: vk::Offset3D { x: 0, y: 0, z: 0 },
        dst_subresource: make_image_subresource_layers(),
        dst_offset: vk::Offset3D { x: 0, y: 0, z: 0 },
        extent: vk::Extent3D {
            width: frame_width.min(swapchain_width),
            height: frame_height.min(swapchain_height),
            depth: 1,
        },
    }
}

/// Upstream `PresentManager::CopyToSwapchainImpl` waits for both the acquired
/// swapchain image and the completed composite at the first transfer command.
fn present_wait_stage_masks() -> [vk::PipelineStageFlags; 2] {
    [vk::PipelineStageFlags::TRANSFER; 2]
}

// ---------------------------------------------------------------------------
// Frame
// ---------------------------------------------------------------------------

/// Port of `Frame` struct.
///
/// A single presentation frame with its image, views, and synchronization
/// primitives.
/// All fields are `Copy` Vulkan handles/sizes: the present thread receives a
/// snapshot of the frame instead of sharing a reference with the GPU thread.
#[derive(Clone, Copy)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub image: vk::Image,
    pub image_memory: vk::DeviceMemory,
    pub image_view: vk::ImageView,
    pub framebuffer: vk::Framebuffer,
    pub cmdbuf: vk::CommandBuffer,
    pub render_ready: vk::Semaphore,
    pub present_done: vk::Fence,
}

impl Default for Frame {
    fn default() -> Self {
        Frame {
            width: 0,
            height: 0,
            image: vk::Image::null(),
            image_memory: vk::DeviceMemory::null(),
            image_view: vk::ImageView::null(),
            framebuffer: vk::Framebuffer::null(),
            cmdbuf: vk::CommandBuffer::null(),
            render_ready: vk::Semaphore::null(),
            present_done: vk::Fence::null(),
        }
    }
}

// ---------------------------------------------------------------------------
// PresentManager
// ---------------------------------------------------------------------------

/// Port of `PresentManager` class.
///
/// Manages a pool of `Frame` objects, a present queue, and an optional
/// present thread that copies rendered frames to the swapchain.
pub struct PresentManager {
    device: ash::Device,
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    frame_image_format: vk::Format,
    cmdpool: vk::CommandPool,
    frames: Vec<Frame>,
    use_present_thread: bool,
    /// State shared with the present thread. Upstream shares `this` between
    /// the main thread and `PresentThread`; the Rust split keeps the frame
    /// pool on the render side and the swapchain copy machinery here.
    ctx: Arc<PresentThreadContext>,
    /// Upstream `std::jthread present_thread`.
    present_thread: Option<std::thread::JoinHandle<()>>,
}

/// Present-thread-side owner: everything `PresentManager::CopyToSwapchain`
/// needs, shared between the render thread and the present thread.
pub(crate) struct PresentThreadContext {
    entry: ash::Entry,
    instance: ash::Instance,
    window_info: vulkan_surface::WindowSystemInfo,
    surface: Arc<Mutex<OwnedSurface>>,
    device: ash::Device,
    submit_mutex: Arc<Mutex<()>>,
    blit_supported: bool,
    /// Upstream `Swapchain& swapchain` + `std::mutex swapchain_mutex`.
    swapchain: Arc<Mutex<Swapchain>>,
    graphics_queue: vk::Queue,
    /// Queued `(frame_index, frame snapshot)` presentation jobs.
    present_queue: Mutex<VecDeque<(usize, Frame)>>,
    free_queue: Mutex<VecDeque<usize>>,
    frame_cv: Condvar,
    free_cv: Condvar,
    image_count: AtomicUsize,
    /// Cached `Swapchain::get_image_view_format()` as a raw `vk::Format`
    /// value. Upstream `RendererVulkan::Composite` reads the swapchain
    /// getters without a lock (renderer_vulkan.cpp:163); Rust caches them
    /// in atomics so the GPU thread's composite never contends on
    /// `swapchain_mutex`, which the present thread holds across
    /// `acquire_next_image` (MoltenVK blocks on the next drawable there —
    /// up to a vsync period per frame).
    image_view_format: std::sync::atomic::AtomicI32,
    stop: AtomicBool,
}

/// Port of `PresentManager::PresentThread`.
fn present_thread_main(ctx: &PresentThreadContext) {
    loop {
        let (frame_index, frame, mut swapchain) = {
            let mut queue = ctx.present_queue.lock().unwrap();
            let (frame_index, frame) = loop {
                if ctx.stop.load(Ordering::Acquire) {
                    return;
                }
                if let Some(job) = queue.pop_front() {
                    break job;
                }
                queue = ctx.frame_cv.wait(queue).unwrap();
            };

            // Match upstream's queue-lock -> swapchain-lock handoff. Keeping
            // the queue locked until the swapchain lock is held prevents
            // WaitPresent from observing an empty queue before this frame has
            // actually entered the presentation critical section.
            ctx.frame_cv.notify_one();
            let swapchain = ctx.swapchain.lock().unwrap();
            (frame_index, frame, swapchain)
        };
        ctx.copy_to_swapchain_locked(frame_index, &frame, None, &mut swapchain);
        drop(swapchain);
        ctx.release_frame(frame_index);
    }
}

/// Maximum number of images in flight.
/// Upstream caps this at 7 (FRAMES_IN_FLIGHT=8, TICKS_TO_DESTROY=8).
const MAX_IMAGES_IN_FLIGHT: usize = 7;

impl PresentManager {
    /// Port of `PresentManager::PresentManager`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        entry: ash::Entry,
        instance: ash::Instance,
        window_info: vulkan_surface::WindowSystemInfo,
        surface: Arc<Mutex<OwnedSurface>>,
        vulkan_device: &Device,
        memory_properties: vk::PhysicalDeviceMemoryProperties,
        frame_image_format: vk::Format,
        graphics_family: u32,
        image_count: usize,
        use_present_thread: bool,
        submit_mutex: Arc<Mutex<()>>,
        swapchain: Arc<Mutex<Swapchain>>,
        graphics_queue: vk::Queue,
    ) -> Self {
        let device = vulkan_device.get_logical().clone();
        let blit_supported = can_blit_to_swapchain(
            vulkan_device,
            swapchain.lock().unwrap().get_image_view_format(),
        );
        let effective_count = image_count.min(MAX_IMAGES_IN_FLIGHT);

        // Create command pool
        let pool_ci = vk::CommandPoolCreateInfo::builder()
            .flags(
                vk::CommandPoolCreateFlags::TRANSIENT
                    | vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER,
            )
            .queue_family_index(graphics_family)
            .build();
        let cmdpool = unsafe {
            device
                .create_command_pool(&pool_ci, None)
                .expect("Failed to create present command pool")
        };

        // Allocate command buffers
        let alloc_info = vk::CommandBufferAllocateInfo::builder()
            .command_pool(cmdpool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(effective_count as u32)
            .build();
        let cmdbufs = unsafe {
            device
                .allocate_command_buffers(&alloc_info)
                .expect("Failed to allocate present command buffers")
        };

        // Create frames
        let mut frames = Vec::with_capacity(effective_count);
        let mut free_queue = VecDeque::with_capacity(effective_count);

        let semaphore_ci = vk::SemaphoreCreateInfo::builder().build();
        let fence_ci = vk::FenceCreateInfo::builder()
            .flags(vk::FenceCreateFlags::SIGNALED)
            .build();

        for i in 0..effective_count {
            let render_ready = unsafe {
                device
                    .create_semaphore(&semaphore_ci, None)
                    .expect("Failed to create render_ready semaphore")
            };
            let present_done = unsafe {
                device
                    .create_fence(&fence_ci, None)
                    .expect("Failed to create present_done fence")
            };
            frames.push(Frame {
                width: 0,
                height: 0,
                image: vk::Image::null(),
                image_memory: vk::DeviceMemory::null(),
                image_view: vk::ImageView::null(),
                framebuffer: vk::Framebuffer::null(),
                cmdbuf: cmdbufs[i],
                render_ready,
                present_done,
            });
            free_queue.push_back(i);
        }

        let initial_image_view_format = swapchain.lock().unwrap().get_image_view_format();
        let ctx = Arc::new(PresentThreadContext {
            entry,
            instance,
            window_info,
            surface,
            device: device.clone(),
            submit_mutex,
            blit_supported,
            swapchain,
            graphics_queue,
            present_queue: Mutex::new(VecDeque::new()),
            free_queue: Mutex::new(free_queue),
            frame_cv: Condvar::new(),
            free_cv: Condvar::new(),
            image_count: AtomicUsize::new(effective_count),
            image_view_format: std::sync::atomic::AtomicI32::new(
                initial_image_view_format.as_raw(),
            ),
            stop: AtomicBool::new(false),
        });

        let present_thread = if use_present_thread {
            let thread_ctx = Arc::clone(&ctx);
            Some(
                std::thread::Builder::new()
                    .name("VulkanPresent".into())
                    .spawn(move || present_thread_main(&thread_ctx))
                    .expect("Failed to spawn Vulkan present thread"),
            )
        } else {
            None
        };

        PresentManager {
            device,
            memory_properties,
            frame_image_format,
            cmdpool,
            frames,
            use_present_thread,
            ctx,
            present_thread,
        }
    }

    /// Port of `PresentManager::GetRenderFrame`.
    ///
    /// Blocks until a free presentation frame is available, then returns it.
    pub fn get_render_frame(&mut self) -> &mut Frame {
        let index = self.get_render_frame_index();
        &mut self.frames[index]
    }

    /// Rust ownership helper for upstream `PresentManager::GetRenderFrame`.
    ///
    /// Upstream returns a `Frame*`; callers later pass the same pointer to
    /// `Present`. Rust needs the frame identity explicitly to avoid holding a
    /// mutable borrow across the whole present path.
    pub fn get_render_frame_index(&mut self) -> usize {
        let index = {
            let mut free = self.ctx.free_queue.lock().unwrap();
            while free.is_empty() {
                free = self.ctx.free_cv.wait(free).unwrap();
            }
            free.pop_front().unwrap()
        };

        // Wait for the presentation to be finished
        let frame = &self.frames[index];
        if frame.present_done != vk::Fence::null() {
            unsafe {
                self.device
                    .wait_for_fences(&[frame.present_done], true, u64::MAX)
                    .expect("Failed to wait for present_done fence");
                self.device
                    .reset_fences(&[frame.present_done])
                    .expect("Failed to reset present_done fence");
            }
        }

        index
    }

    pub fn frame(&self, index: usize) -> &Frame {
        &self.frames[index]
    }

    pub fn frame_mut(&mut self, index: usize) -> &mut Frame {
        &mut self.frames[index]
    }

    pub fn release_frame(&self, index: usize) {
        self.ctx.release_frame(index);
    }

    pub fn recreate_frame_by_index(
        &mut self,
        frame_index: usize,
        width: u32,
        height: u32,
        image_view_format: vk::Format,
        render_pass: vk::RenderPass,
    ) {
        let mut frame = std::mem::take(&mut self.frames[frame_index]);
        self.recreate_frame(&mut frame, width, height, image_view_format, render_pass);
        self.frames[frame_index] = frame;
    }

    /// Port of `PresentManager::Present`.
    ///
    /// Queues a frame for presentation, or presents directly if no present
    /// thread is active.
    pub fn present(&mut self, frame_index: usize, scheduler: &mut Scheduler) {
        // Frame is `Copy`: hand the present thread a snapshot. The frame slot
        // is exclusively owned by the presentation side until the thread
        // pushes the index back onto the free queue.
        let frame = self.frames[frame_index];
        if !self.use_present_thread {
            // Upstream `PresentManager::Present` drains Scheduler's worker
            // before entering the synchronous swapchain path.
            scheduler.wait_worker();
            self.ctx
                .copy_to_swapchain(frame_index, &frame, Some(scheduler));
            self.ctx.release_frame(frame_index);
            return;
        }

        let ctx = Arc::clone(&self.ctx);
        scheduler.record(move |_| {
            let mut queue = ctx.present_queue.lock().unwrap();
            queue.push_back((frame_index, frame));
            ctx.frame_cv.notify_one();
        });
    }

    /// Port of `PresentManager::RecreateFrame`.
    ///
    /// Recreates the frame's image, image view, and framebuffer to match
    /// the given dimensions and format.
    pub fn recreate_frame(
        &self,
        frame: &mut Frame,
        width: u32,
        height: u32,
        image_view_format: vk::Format,
        render_pass: vk::RenderPass,
    ) {
        self.destroy_frame_resources(frame);

        frame.width = width;
        frame.height = height;

        // Create image
        let image_ci = vk::ImageCreateInfo::builder()
            .flags(vk::ImageCreateFlags::MUTABLE_FORMAT)
            .image_type(vk::ImageType::TYPE_2D)
            .format(self.frame_image_format)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .build();

        frame.image = unsafe {
            self.device
                .create_image(&image_ci, None)
                .expect("Failed to create present frame image")
        };
        let memory_requirements = unsafe { self.device.get_image_memory_requirements(frame.image) };
        let memory_type_index = find_memory_type(
            &self.memory_properties,
            memory_requirements.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )
        .expect("Failed to find present frame memory type");
        let allocate_info = vk::MemoryAllocateInfo::builder()
            .allocation_size(memory_requirements.size)
            .memory_type_index(memory_type_index)
            .build();
        frame.image_memory = unsafe {
            self.device
                .allocate_memory(&allocate_info, None)
                .expect("Failed to allocate present frame image memory")
        };
        unsafe {
            self.device
                .bind_image_memory(frame.image, frame.image_memory, 0)
                .expect("Failed to bind present frame image memory");
        }

        // Create image view
        let view_ci = vk::ImageViewCreateInfo::builder()
            .image(frame.image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(image_view_format)
            .components(vk::ComponentMapping::default())
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            })
            .build();

        frame.image_view = unsafe {
            self.device
                .create_image_view(&view_ci, None)
                .expect("Failed to create present frame image view")
        };

        // Create framebuffer
        let attachments = [frame.image_view];
        let fb_ci = vk::FramebufferCreateInfo::builder()
            .render_pass(render_pass)
            .attachments(&attachments)
            .width(width)
            .height(height)
            .layers(1)
            .build();

        frame.framebuffer = unsafe {
            self.device
                .create_framebuffer(&fb_ci, None)
                .expect("Failed to create present frame framebuffer")
        };
    }

    /// Port of `PresentManager::WaitPresent`.
    ///
    /// Blocks until all queued frames have been presented.
    /// Lock-free swapchain image count for the GPU thread's composite.
    /// Upstream `Composite` reads `swapchain.GetImageCount()` without a
    /// lock (renderer_vulkan.cpp:163); taking `swapchain_mutex` here would
    /// stall composition behind the present thread's `acquire_next_image`.
    pub fn swapchain_image_count(&self) -> usize {
        self.ctx.image_count.load(Ordering::Acquire)
    }

    /// Lock-free swapchain image-view format (same rationale as
    /// `swapchain_image_count`).
    pub fn swapchain_image_view_format(&self) -> vk::Format {
        vk::Format::from_raw(self.ctx.image_view_format.load(Ordering::Acquire))
    }

    pub fn wait_present(&self) {
        if !self.use_present_thread {
            return;
        }

        // Wait for the present queue to be empty
        {
            let mut queue = self.ctx.present_queue.lock().unwrap();
            while !queue.is_empty() {
                queue = self.ctx.frame_cv.wait(queue).unwrap();
            }
        }

        // Acquire the swapchain mutex to ensure the last frame has been
        // presented (the present thread holds it for the whole copy).
        let _lock = self.ctx.swapchain.lock().unwrap();
    }

    fn destroy_frame_resources(&self, frame: &mut Frame) {
        unsafe {
            if frame.framebuffer != vk::Framebuffer::null() {
                self.device.destroy_framebuffer(frame.framebuffer, None);
                frame.framebuffer = vk::Framebuffer::null();
            }
            if frame.image_view != vk::ImageView::null() {
                self.device.destroy_image_view(frame.image_view, None);
                frame.image_view = vk::ImageView::null();
            }
            if frame.image != vk::Image::null() {
                self.device.destroy_image(frame.image, None);
                frame.image = vk::Image::null();
            }
            if frame.image_memory != vk::DeviceMemory::null() {
                self.device.free_memory(frame.image_memory, None);
                frame.image_memory = vk::DeviceMemory::null();
            }
        }
        frame.width = 0;
        frame.height = 0;
    }
}

impl PresentThreadContext {
    fn release_frame(&self, index: usize) {
        let mut free = self.free_queue.lock().unwrap();
        free.push_back(index);
        self.free_cv.notify_one();
    }

    fn set_image_count(&self, swapchain_image_count: usize) {
        self.image_count.store(
            swapchain_image_count.min(MAX_IMAGES_IN_FLIGHT),
            Ordering::Release,
        );
    }

    fn set_image_view_format(&self, format: vk::Format) {
        self.image_view_format
            .store(format.as_raw(), Ordering::Release);
    }

    /// Port of `PresentManager::CopyToSwapchain`.
    ///
    /// `scheduler` is `Some` only on the direct (non-threaded) path; the
    /// present thread cannot touch the GPU thread's scheduler and relies on
    /// the `render_ready` semaphore chain instead.
    fn copy_to_swapchain(
        &self,
        frame_index: usize,
        frame: &Frame,
        scheduler: Option<&mut Scheduler>,
    ) {
        let mut swapchain = self.swapchain.lock().unwrap();
        self.copy_to_swapchain_locked(frame_index, frame, scheduler, &mut swapchain);
    }

    fn copy_to_swapchain_locked(
        &self,
        _frame_index: usize,
        frame: &Frame,
        mut scheduler: Option<&mut Scheduler>,
        swapchain: &mut Swapchain,
    ) {
        let mut requires_surface_recreation = false;
        loop {
            let result = if requires_surface_recreation {
                self.recreate_surface(frame, swapchain)
            } else {
                self.copy_to_swapchain_once(frame, scheduler.as_deref_mut(), swapchain)
            };

            match result {
                Ok(()) if requires_surface_recreation => {
                    requires_surface_recreation = false;
                }
                Ok(()) => return,
                Err(vk::Result::ERROR_SURFACE_LOST_KHR) => {
                    requires_surface_recreation = true;
                }
                Err(result) => {
                    log::error!("Vulkan presentation failed: {:?}", result);
                    return;
                }
            }
        }
    }

    fn copy_to_swapchain_once(
        &self,
        frame: &Frame,
        mut scheduler: Option<&mut Scheduler>,
        swapchain: &mut Swapchain,
    ) -> Result<(), vk::Result> {
        let needs_recreation = swapchain.needs_recreation()
            || swapchain.get_width() != frame.width
            || swapchain.get_height() != frame.height;
        if needs_recreation {
            self.recreate_swapchain(frame, swapchain)?;
        }

        while swapchain.acquire_next_image(scheduler.as_deref_mut())? {
            self.recreate_swapchain(frame, swapchain)?;
        }

        let swapchain_image = swapchain.current_image();
        let swapchain_extent = swapchain.get_extent();
        let present_semaphore = swapchain.current_present_semaphore();
        let render_semaphore = swapchain.current_render_semaphore();
        self.copy_to_swapchain_impl(
            frame,
            swapchain_image,
            swapchain_extent,
            present_semaphore,
            render_semaphore,
            self.graphics_queue,
        );
        swapchain.present(render_semaphore)
    }

    /// Port of `PresentManager::RecreateSwapchain`.
    fn recreate_swapchain(
        &self,
        frame: &Frame,
        swapchain: &mut Swapchain,
    ) -> Result<(), vk::Result> {
        let surface = self.surface.lock().unwrap().handle();
        swapchain
            .create(surface, frame.width, frame.height)
            .map_err(|err| err.result)?;
        self.set_image_count(swapchain.get_image_count());
        self.set_image_view_format(swapchain.get_image_view_format());
        Ok(())
    }

    fn recreate_surface(&self, frame: &Frame, swapchain: &mut Swapchain) -> Result<(), vk::Result> {
        let new_surface = unsafe {
            vulkan_surface::create_surface(&self.entry, &self.instance, &self.window_info)
                .map_err(|err| err.result)?
        };
        self.surface.lock().unwrap().replace(new_surface);
        self.recreate_swapchain(frame, swapchain)
    }

    /// Port of `PresentManager::CopyToSwapchainImpl`.
    ///
    /// Records and submits commands to copy a frame image to the swapchain
    /// image, using blit or copy depending on hardware support.
    pub fn copy_to_swapchain_impl(
        &self,
        frame: &Frame,
        swapchain_image: vk::Image,
        swapchain_extent: vk::Extent2D,
        present_semaphore: vk::Semaphore,
        render_semaphore: vk::Semaphore,
        graphics_queue: vk::Queue,
    ) {
        let cmdbuf = frame.cmdbuf;

        let begin_info = vk::CommandBufferBeginInfo::builder()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
            .build();

        unsafe {
            self.device
                .begin_command_buffer(cmdbuf, &begin_info)
                .expect("Failed to begin present command buffer");
        }

        // Pre-barriers
        let pre_barriers = [
            // Swapchain image: UNDEFINED -> TRANSFER_DST_OPTIMAL
            vk::ImageMemoryBarrier {
                s_type: vk::StructureType::IMAGE_MEMORY_BARRIER,
                p_next: std::ptr::null(),
                src_access_mask: vk::AccessFlags::empty(),
                dst_access_mask: vk::AccessFlags::TRANSFER_WRITE,
                old_layout: vk::ImageLayout::UNDEFINED,
                new_layout: vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                image: swapchain_image,
                subresource_range: vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: vk::REMAINING_ARRAY_LAYERS,
                },
            },
            // Frame image: GENERAL -> TRANSFER_SRC_OPTIMAL
            vk::ImageMemoryBarrier {
                s_type: vk::StructureType::IMAGE_MEMORY_BARRIER,
                p_next: std::ptr::null(),
                src_access_mask: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                dst_access_mask: vk::AccessFlags::TRANSFER_READ,
                old_layout: vk::ImageLayout::GENERAL,
                new_layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                image: frame.image,
                subresource_range: vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: vk::REMAINING_ARRAY_LAYERS,
                },
            },
        ];

        // Post-barriers
        let post_barriers = [
            // Swapchain image: TRANSFER_DST_OPTIMAL -> PRESENT_SRC
            vk::ImageMemoryBarrier {
                s_type: vk::StructureType::IMAGE_MEMORY_BARRIER,
                p_next: std::ptr::null(),
                src_access_mask: vk::AccessFlags::TRANSFER_WRITE,
                dst_access_mask: vk::AccessFlags::MEMORY_READ,
                old_layout: vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                new_layout: vk::ImageLayout::PRESENT_SRC_KHR,
                src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                image: swapchain_image,
                subresource_range: vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: vk::REMAINING_ARRAY_LAYERS,
                },
            },
            // Frame image: TRANSFER_SRC_OPTIMAL -> GENERAL
            vk::ImageMemoryBarrier {
                s_type: vk::StructureType::IMAGE_MEMORY_BARRIER,
                p_next: std::ptr::null(),
                src_access_mask: vk::AccessFlags::TRANSFER_READ,
                dst_access_mask: vk::AccessFlags::MEMORY_WRITE,
                old_layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                new_layout: vk::ImageLayout::GENERAL,
                src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                image: frame.image,
                subresource_range: vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: vk::REMAINING_ARRAY_LAYERS,
                },
            },
        ];

        unsafe {
            self.device.cmd_pipeline_barrier(
                cmdbuf,
                PIPELINE_STAGE_GRAPHICS_COMPUTE_TRANSFER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &pre_barriers,
            );

            if self.blit_supported {
                let region = make_image_blit(
                    frame.width as i32,
                    frame.height as i32,
                    swapchain_extent.width as i32,
                    swapchain_extent.height as i32,
                );
                self.device.cmd_blit_image(
                    cmdbuf,
                    frame.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    swapchain_image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[region],
                    vk::Filter::LINEAR,
                );
            } else {
                let region = make_image_copy(
                    frame.width,
                    frame.height,
                    swapchain_extent.width,
                    swapchain_extent.height,
                );
                self.device.cmd_copy_image(
                    cmdbuf,
                    frame.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    swapchain_image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[region],
                );
            }

            self.device.cmd_pipeline_barrier(
                cmdbuf,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ALL_GRAPHICS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &post_barriers,
            );

            self.device
                .end_command_buffer(cmdbuf)
                .expect("Failed to end present command buffer");
        }

        // Submit
        let wait_semaphores = [present_semaphore, frame.render_ready];
        let wait_stages = present_wait_stage_masks();
        let cmdbufs = [cmdbuf];
        let signal_semaphores = [render_semaphore];

        let submit_info = vk::SubmitInfo::builder()
            .wait_semaphores(&wait_semaphores)
            .wait_dst_stage_mask(&wait_stages)
            .command_buffers(&cmdbufs)
            .signal_semaphores(&signal_semaphores)
            .build();

        unsafe {
            let _submit_lock = self.submit_mutex.lock().unwrap();
            self.device
                .queue_submit(graphics_queue, &[submit_info], frame.present_done)
                .expect("Failed to submit present commands");
        }
    }
}

impl Drop for PresentManager {
    fn drop(&mut self) {
        // Upstream `std::jthread` stops and joins the present thread before
        // frame resources are destroyed.
        if let Some(thread) = self.present_thread.take() {
            self.ctx.stop.store(true, Ordering::Release);
            self.ctx.frame_cv.notify_all();
            let _ = thread.join();
        }
        unsafe {
            let device = self.device.clone();
            for frame in &mut self.frames {
                if frame.framebuffer != vk::Framebuffer::null() {
                    device.destroy_framebuffer(frame.framebuffer, None);
                    frame.framebuffer = vk::Framebuffer::null();
                }
                if frame.image_view != vk::ImageView::null() {
                    device.destroy_image_view(frame.image_view, None);
                    frame.image_view = vk::ImageView::null();
                }
                if frame.image != vk::Image::null() {
                    device.destroy_image(frame.image, None);
                    frame.image = vk::Image::null();
                }
                if frame.image_memory != vk::DeviceMemory::null() {
                    device.free_memory(frame.image_memory, None);
                    frame.image_memory = vk::DeviceMemory::null();
                }
                if frame.render_ready != vk::Semaphore::null() {
                    device.destroy_semaphore(frame.render_ready, None);
                    frame.render_ready = vk::Semaphore::null();
                }
                if frame.present_done != vk::Fence::null() {
                    device.destroy_fence(frame.present_done, None);
                    frame.present_done = vk::Fence::null();
                }
            }
            if self.cmdpool != vk::CommandPool::null() {
                device.destroy_command_pool(self.cmdpool, None);
                self.cmdpool = vk::CommandPool::null();
            }
        }
    }
}

fn find_memory_type(
    properties: &vk::PhysicalDeviceMemoryProperties,
    type_bits: u32,
    required_flags: vk::MemoryPropertyFlags,
) -> Option<u32> {
    for index in 0..properties.memory_type_count {
        let type_supported = (type_bits & (1 << index)) != 0;
        let flags = properties.memory_types[index as usize].property_flags;
        if type_supported && flags.contains(required_flags) {
            return Some(index);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_waits_before_the_swapchain_transfer() {
        assert_eq!(
            present_wait_stage_masks(),
            [vk::PipelineStageFlags::TRANSFER; 2]
        );
    }
}
