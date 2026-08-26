// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `vk_compute_pipeline.h` / `vk_compute_pipeline.cpp`.
//!
//! Manages compilation and configuration of a single Vulkan compute pipeline.
//! Supports asynchronous pipeline building via a background thread worker.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::buffer_cache::buffer_cache_base::BufferCacheRuntime;
use ash::vk;
use common::thread_worker::ThreadWorker;
use shader_recompiler::shader_info::{
    ImageBufferDescriptor, ImageDescriptor, ImageFormat, Info as ShaderInfo,
    TextureBufferDescriptor, TextureDescriptor,
};

use crate::shader_notify::ShaderNotifyHandle;

use super::buffer_cache::VulkanCommonBufferCache;
use super::descriptor_buffer::DescriptorBufferRing;
use super::descriptor_pool::{DescriptorAllocator, DescriptorPool};
use super::pipeline_helper::{
    num_descriptor_entries, pixel_format_from_image_format, push_image_descriptors,
    write_descriptor_buffer, DescriptorBufferLayout, DescriptorLayoutBuilder,
    RescalingPushConstant, RESCALING_LAYOUT_WORDS_OFFSET,
};
use super::pipeline_statistics::PipelineStatistics;
use super::scheduler::Scheduler;
use super::texture_cache::TextureCache;
use super::update_descriptor::UpdateDescriptorQueue;
use crate::engines::kepler_compute::{DispatchCall, LaunchParams};
use crate::texture_cache::texture_cache_base::{ComputeDescriptorSyncRegs, ImageViewInOut};
use crate::textures::texture::texture_pair;
use crate::vulkan_common::vulkan_device::DeviceReference;

#[derive(Clone, Copy)]
pub(crate) struct ComputePipelineRuntime {
    scheduler: NonNull<Scheduler>,
    guest_descriptor_queue: NonNull<UpdateDescriptorQueue>,
    descriptor_buffer_ring: NonNull<DescriptorBufferRing>,
    descriptor_pool: NonNull<DescriptorPool>,
}

impl ComputePipelineRuntime {
    pub(crate) fn new(
        scheduler: &mut Scheduler,
        guest_descriptor_queue: &mut UpdateDescriptorQueue,
        descriptor_buffer_ring: &mut DescriptorBufferRing,
        descriptor_pool: &mut DescriptorPool,
    ) -> Self {
        Self {
            scheduler: NonNull::from(scheduler),
            guest_descriptor_queue: NonNull::from(guest_descriptor_queue),
            descriptor_buffer_ring: NonNull::from(descriptor_buffer_ring),
            descriptor_pool: NonNull::from(descriptor_pool),
        }
    }
}

unsafe impl Send for ComputePipelineRuntime {}
unsafe impl Sync for ComputePipelineRuntime {}

// ---------------------------------------------------------------------------
// ComputePipeline
// ---------------------------------------------------------------------------

/// Port of `ComputePipeline` class.
///
/// Wraps a single Vulkan compute pipeline, handling asynchronous building,
/// descriptor set allocation, and per-dispatch configuration.
///
/// Upstream fields:
/// - `device` — reference to the Vulkan device wrapper
/// - `pipeline_cache` — shared VkPipelineCache for compilation
/// - `guest_descriptor_queue` — queue for descriptor updates
/// - `info` — shader info from the recompiler
/// - `uniform_buffer_sizes` — per-binding UBO sizes
/// - `spv_module` — SPIR-V shader module
/// - `descriptor_set_layout` — layout for the pipeline's descriptor set
/// - `descriptor_allocator` — allocates descriptor sets from the pool
/// - `pipeline_layout` — pipeline layout handle
/// - `descriptor_update_template` — template for fast descriptor updates
/// - `pipeline` — the compiled VkPipeline
/// - `build_condvar` / `build_mutex` / `is_built` — async build synchronization
pub struct ComputePipeline {
    device_owner: DeviceReference,
    /// Upstream `vk::PipelineCache& pipeline_cache`. The raw handle is copied
    /// because ash Vulkan handles are lightweight references to driver state.
    #[allow(dead_code)]
    pipeline_cache: vk::PipelineCache,
    guest_descriptor_queue: NonNull<UpdateDescriptorQueue>,
    descriptor_buffer_ring: NonNull<DescriptorBufferRing>,
    /// Upstream `Shader::Info info`.
    info: ShaderInfo,
    /// Retained for upstream state parity. The Rust build closure captures the
    /// scalar value because it cannot borrow a partially constructed `Self`.
    #[allow(dead_code)]
    shader_hash: u64,

    /// Number of payload entries reserved from the guest descriptor queue.
    ///
    /// Port of upstream `num_descriptor_entries`.
    num_descriptor_entries: u32,

    /// Uniform buffer sizes per binding (from shader info).
    uniform_buffer_sizes: crate::buffer_cache::buffer_cache_base::ComputeUniformBufferSizes,

    /// SPV shader module.
    spv_module: vk::ShaderModule,

    /// Descriptor set layout for this pipeline.
    descriptor_set_layout: vk::DescriptorSetLayout,

    /// Upstream `uses_push_descriptor` selected from device capabilities.
    uses_push_descriptor: bool,
    uses_descriptor_buffer: bool,
    descriptor_buffer_layout: DescriptorBufferLayout,

    /// Upstream per-pipeline descriptor-set allocator.
    descriptor_allocator: Option<DescriptorAllocator>,

    /// Pipeline layout.
    pipeline_layout: vk::PipelineLayout,

    /// Descriptor update template.
    descriptor_update_template: vk::DescriptorUpdateTemplate,

    /// The compiled compute pipeline handle.
    pipeline: Arc<Mutex<vk::Pipeline>>,

    /// Synchronization for async build.
    build_condvar: Arc<Condvar>,
    build_mutex: Arc<Mutex<()>>,
    is_built: Arc<AtomicBool>,
}

// Vulkan pipeline objects are opaque device handles. Upstream queues compute
// pipeline construction on `ThreadWorker` and then transfers ownership back to
// the cache; the Rust disk preload path mirrors that transfer.
unsafe impl Send for ComputePipeline {}

impl ComputePipeline {
    /// Port of `ComputePipeline::ComputePipeline`.
    ///
    /// This:
    /// 1. Creates the descriptor set layout from shader info
    /// 2. Creates the pipeline layout with push constant ranges
    /// 3. Creates the descriptor update template
    /// 4. Optionally builds the pipeline asynchronously via thread_worker
    /// 5. Notifies shader_notify when building starts/ends
    ///
    /// The pipeline can be built synchronously (if no thread_worker) or
    /// asynchronously, with `is_built` signaling completion.
    /// Port of `ComputePipeline::ComputePipeline` with an optional upstream
    /// `Common::ThreadWorker`.
    pub(crate) fn new(
        device_ref: DeviceReference,
        info: ShaderInfo,
        spv_module: vk::ShaderModule,
        pipeline_cache: vk::PipelineCache,
        shader_notify: ShaderNotifyHandle,
        worker: Option<&ThreadWorker>,
        runtime: ComputePipelineRuntime,
        shader_hash: u64,
        pipeline_statistics: Option<Arc<PipelineStatistics>>,
    ) -> Option<Self> {
        let vulkan_device = device_ref.get();
        let device = vulkan_device.get_logical();
        shader_notify.mark_shader_building();
        let mut layout_builder = DescriptorLayoutBuilder::new(vulkan_device);
        layout_builder.add(&info, vk::ShaderStageFlags::COMPUTE);
        let num_descriptor_entries = num_descriptor_entries(&info);
        let uses_push_descriptor = layout_builder.can_use_push_descriptor();
        let descriptor_buffer_ring = unsafe { runtime.descriptor_buffer_ring.as_ref() };
        let mut uses_descriptor_buffer =
            layout_builder.can_use_descriptor_buffer() && descriptor_buffer_ring.is_valid();
        let Ok(mut descriptor_set_layout) = layout_builder
            .create_descriptor_set_layout(uses_push_descriptor, uses_descriptor_buffer)
        else {
            unsafe {
                device.destroy_shader_module(spv_module, None);
            }
            shader_notify.mark_shader_complete();
            return None;
        };
        let mut descriptor_buffer_layout = if uses_descriptor_buffer {
            layout_builder.make_descriptor_buffer_layout(descriptor_set_layout)
        } else {
            DescriptorBufferLayout::default()
        };
        if uses_descriptor_buffer
            && !descriptor_buffer_ring.can_allocate(descriptor_buffer_layout.size)
        {
            log::debug!(
                "Compute shader {:016X} needs {} descriptor bytes per dispatch, falling back to sets",
                shader_hash,
                descriptor_buffer_layout.size
            );
            unsafe {
                device.destroy_descriptor_set_layout(descriptor_set_layout, None);
            }
            uses_descriptor_buffer = false;
            descriptor_buffer_layout = DescriptorBufferLayout::default();
            descriptor_set_layout = match layout_builder.create_descriptor_set_layout(false, false)
            {
                Ok(layout) => layout,
                Err(_) => {
                    unsafe { device.destroy_shader_module(spv_module, None) };
                    shader_notify.mark_shader_complete();
                    return None;
                }
            };
        }
        let Ok(pipeline_layout) = layout_builder.create_pipeline_layout(descriptor_set_layout)
        else {
            unsafe {
                device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                device.destroy_shader_module(spv_module, None);
            }
            shader_notify.mark_shader_complete();
            return None;
        };
        let descriptor_update_template = if uses_descriptor_buffer {
            vk::DescriptorUpdateTemplate::null()
        } else {
            match layout_builder.create_template(
                descriptor_set_layout,
                pipeline_layout,
                uses_push_descriptor,
            ) {
                Ok(template) => template,
                Err(_) => {
                    unsafe {
                        device.destroy_pipeline_layout(pipeline_layout, None);
                        if descriptor_set_layout != vk::DescriptorSetLayout::null() {
                            device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                        }
                        device.destroy_shader_module(spv_module, None);
                    }
                    shader_notify.mark_shader_complete();
                    return None;
                }
            }
        };
        let descriptor_allocator = if !uses_descriptor_buffer && !uses_push_descriptor {
            match unsafe { runtime.descriptor_pool.as_ref() }.allocator_for_info(
                vulkan_device,
                unsafe { runtime.scheduler.as_ref() },
                descriptor_set_layout,
                &info,
            ) {
                Ok(allocator) => Some(allocator),
                Err(_) => {
                    unsafe {
                        if descriptor_update_template != vk::DescriptorUpdateTemplate::null() {
                            device.destroy_descriptor_update_template(
                                descriptor_update_template,
                                None,
                            );
                        }
                        device.destroy_pipeline_layout(pipeline_layout, None);
                        device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                        device.destroy_shader_module(spv_module, None);
                    }
                    shader_notify.mark_shader_complete();
                    return None;
                }
            }
        } else {
            None
        };
        let mut uniform_buffer_sizes =
            crate::buffer_cache::buffer_cache_base::ComputeUniformBufferSizes::default();
        let uniform_buffer_count = uniform_buffer_sizes.len();
        uniform_buffer_sizes
            .copy_from_slice(&info.constant_buffer_used_sizes[..uniform_buffer_count]);

        let pipeline = Arc::new(Mutex::new(vk::Pipeline::null()));
        let build_condvar = Arc::new(Condvar::new());
        let build_mutex = Arc::new(Mutex::new(()));
        let is_built = Arc::new(AtomicBool::new(false));

        let build = {
            let pipeline = pipeline.clone();
            let build_condvar = build_condvar.clone();
            let build_mutex = build_mutex.clone();
            let is_built = is_built.clone();
            let supports_subgroup_size_control =
                vulkan_device.is_ext_subgroup_size_control_supported();
            let capture_statistics = vulkan_device.is_khr_pipeline_executable_properties_enabled()
                && *common::settings::values().renderer_debug.get_value();
            move || {
                let device = device_ref.get().get_logical();
                let main_name = std::ffi::CString::new("main").unwrap();
                let mut subgroup_size =
                    vk::PipelineShaderStageRequiredSubgroupSizeCreateInfoEXT::builder()
                        .required_subgroup_size(
                            crate::vulkan_common::vulkan_device::GUEST_WARP_SIZE,
                        );
                let mut stage_builder = vk::PipelineShaderStageCreateInfo::builder()
                    .stage(vk::ShaderStageFlags::COMPUTE)
                    .module(spv_module)
                    .name(&main_name);
                if supports_subgroup_size_control {
                    stage_builder = stage_builder.push_next(&mut subgroup_size);
                }
                let stage_ci = stage_builder.build();

                let mut flags = vk::PipelineCreateFlags::empty();
                if capture_statistics {
                    flags |= vk::PipelineCreateFlags::CAPTURE_STATISTICS_KHR;
                }
                if uses_descriptor_buffer {
                    flags |= vk::PipelineCreateFlags::DESCRIPTOR_BUFFER_EXT;
                }
                let ci = vk::ComputePipelineCreateInfo::builder()
                    .flags(flags)
                    .stage(stage_ci)
                    .layout(pipeline_layout)
                    .build();

                if let Ok(pipelines) =
                    unsafe { device.create_compute_pipelines(pipeline_cache, &[ci], None) }
                {
                    let created = pipelines[0];
                    *pipeline.lock().unwrap() = created;
                    if let Some(statistics) = &pipeline_statistics {
                        statistics.collect(device_ref.get(), created);
                    }
                } else {
                    log::error!(
                        "ComputePipeline: driver rejected compute shader {:016X}",
                        shader_hash
                    );
                }

                {
                    let _lock = build_mutex.lock().unwrap();
                    is_built.store(true, Ordering::Relaxed);
                }
                // Upstream has a single scheduler waiter, whereas the Rust
                // owner may also wait from `pipeline()` or `Drop`. Wake every
                // waiter after publishing the terminal build state.
                build_condvar.notify_all();
                shader_notify.mark_shader_complete();
            }
        };
        if let Some(worker) = worker {
            worker.queue_stateless_work(build);
        } else {
            build();
        }

        Some(ComputePipeline {
            device_owner: device_ref,
            pipeline_cache,
            guest_descriptor_queue: runtime.guest_descriptor_queue,
            descriptor_buffer_ring: runtime.descriptor_buffer_ring,
            info,
            shader_hash,
            num_descriptor_entries,
            uniform_buffer_sizes,
            spv_module,
            descriptor_set_layout,
            uses_push_descriptor,
            uses_descriptor_buffer,
            descriptor_buffer_layout,
            descriptor_allocator,
            pipeline_layout,
            descriptor_update_template,
            pipeline,
            build_condvar,
            build_mutex,
            is_built,
        })
    }

    /// Port of `ComputePipeline::Configure`.
    ///
    /// Configures and dispatches the compute pipeline:
    /// 1. Waits for async build if necessary
    /// 2. Fills buffer descriptors from kepler_compute engine state
    /// 3. Fills texture/image descriptors from texture cache
    /// 4. Commits a descriptor set and updates it
    /// 5. Binds the pipeline, descriptor set, and dispatches
    ///
    /// Requires access to: kepler_compute, gpu_memory, scheduler,
    /// buffer_cache, texture_cache.
    pub fn configure(
        &self,
        dispatch: &DispatchCall,
        scheduler: &mut Scheduler,
        buffer_cache: &mut VulkanCommonBufferCache,
        texture_cache: &mut TextureCache,
        fallback_sampler: vk::Sampler,
        push_descriptor: Option<ash::extensions::khr::PushDescriptor>,
        read_gpu: &dyn Fn(u64, &mut [u8]) -> bool,
    ) -> bool {
        // SAFETY: both owners are boxed by RasterizerVulkan and outlive every
        // cached compute pipeline. Dispatches are serialized on the GPU thread.
        let descriptor_queue = unsafe { &mut *self.guest_descriptor_queue.as_ptr() };
        let descriptor_buffer_ring = unsafe { &mut *self.descriptor_buffer_ring.as_ptr() };
        descriptor_queue.acquire(
            scheduler,
            self.num_descriptor_entries as usize,
            self.uses_descriptor_buffer,
        );
        // SAFETY: PipelineCache stores compute pipelines in Boxes, so this
        // per-pipeline size array stays at a stable address.
        unsafe {
            buffer_cache.set_compute_uniform_buffer_state(
                self.info.constant_buffer_mask,
                &self.uniform_buffer_sizes,
            );
        }
        buffer_cache.unbind_compute_storage_buffers();
        for (index, desc) in self.info.storage_buffers_descriptors.iter().enumerate() {
            assert_eq!(desc.count, 1);
            buffer_cache.bind_compute_storage_buffer(
                index,
                desc.cbuf_index,
                desc.cbuf_offset,
                desc.is_written,
            );
        }

        texture_cache
            .base
            .synchronize_compute_descriptors(ComputeDescriptorSyncRegs {
                linked_tsc: dispatch.launch_description.linked_tsc,
                tic_addr: dispatch.tic_address,
                tic_limit: dispatch.tic_limit,
                tsc_addr: dispatch.tsc_address,
                tsc_limit: dispatch.tsc_limit,
            });
        let mut handles = collect_texture_handles(&self.info, dispatch, |address| {
            let mut value = [0u8; 4];
            let _ = read_gpu(address, &mut value);
            u32::from_le_bytes(value)
        });
        let samplers = handles
            .sampler_indices
            .iter()
            .map(|&index| texture_cache.get_sampler_id(index, true))
            .collect::<Vec<_>>();
        texture_cache.fill_image_views(&mut handles.views, true, true);

        buffer_cache.unbind_compute_texture_buffers();
        let mut view_index = 0usize;
        for desc in &self.info.texture_buffer_descriptors {
            for _ in 0..desc.count {
                bind_compute_texture_buffer(
                    buffer_cache,
                    texture_cache,
                    &handles.views[view_index],
                    view_index,
                    false,
                    false,
                    None,
                );
                view_index += 1;
            }
        }
        for desc in &self.info.image_buffer_descriptors {
            for _ in 0..desc.count {
                bind_compute_texture_buffer(
                    buffer_cache,
                    texture_cache,
                    &handles.views[view_index],
                    view_index,
                    desc.is_written,
                    true,
                    Some(desc.format),
                );
                view_index += 1;
            }
        }
        buffer_cache.update_compute_buffers();
        buffer_cache.bind_host_compute_buffers();
        if buffer_cache.any_buffer_uploaded {
            buffer_cache.runtime.post_copy_barrier();
            buffer_cache.any_buffer_uploaded = false;
        }

        let mut rescaling = RescalingPushConstant::new();
        let mut sampler_index = 0usize;
        view_index = 0;
        push_image_descriptors(
            texture_cache,
            descriptor_queue,
            &self.info,
            &mut rescaling,
            &samplers,
            &mut sampler_index,
            &handles.views,
            &mut view_index,
            fallback_sampler,
        );

        if !self.is_built.load(Ordering::Relaxed) {
            let build_condvar = Arc::clone(&self.build_condvar);
            let build_mutex = Arc::clone(&self.build_mutex);
            let is_built = Arc::clone(&self.is_built);
            scheduler.record(move |_cmdbuf| {
                let lock = build_mutex.lock().unwrap();
                let _guard = build_condvar
                    .wait_while(lock, |_| !is_built.load(Ordering::Relaxed))
                    .unwrap();
            });
        }
        let descriptor_data = descriptor_queue.update_data();
        let mut descriptor_buffer_offset = 0;
        let mut descriptor_buffer_chunk = 0;
        if self.uses_descriptor_buffer {
            let allocation =
                descriptor_buffer_ring.allocate(scheduler, self.descriptor_buffer_layout.size);
            if allocation.host.is_null() {
                log::debug!("Failed to reserve descriptor memory, skipping dispatch");
                return false;
            }
            unsafe {
                write_descriptor_buffer(
                    self.device_owner.get(),
                    &self.descriptor_buffer_layout,
                    descriptor_data,
                    allocation.host,
                );
            }
            descriptor_buffer_offset = allocation.offset;
            descriptor_buffer_chunk = allocation.chunk;
        }
        let bind_descriptor_buffer = self.uses_descriptor_buffer
            && scheduler.update_descriptor_buffer_chunk(descriptor_buffer_chunk);
        let device = self.device_owner;
        let pipeline = Arc::clone(&self.pipeline);
        let pipeline_layout = self.pipeline_layout;
        let descriptor_set_layout = self.descriptor_set_layout;
        let descriptor_update_template = self.descriptor_update_template;
        let descriptor_allocator = self
            .descriptor_allocator
            .as_ref()
            .map(DescriptorAllocator::reference);
        let uses_push_descriptor = self.uses_push_descriptor;
        let uses_descriptor_buffer = self.uses_descriptor_buffer;
        let descriptor_buffer_binding = bind_descriptor_buffer.then(|| {
            let info = descriptor_buffer_ring.binding_info(descriptor_buffer_chunk);
            (info.address, info.usage)
        });
        let descriptor_data = descriptor_data as usize;
        let rescaling_data = *rescaling.data();
        let is_rescaling =
            !self.info.texture_descriptors.is_empty() || !self.info.image_descriptors.is_empty();
        scheduler.record(move |cmdbuf| unsafe {
            let vulkan_device = device.get();
            let logical = vulkan_device.get_logical();
            if let Some((address, usage)) = descriptor_buffer_binding {
                let binding_info = vk::DescriptorBufferBindingInfoEXT::builder()
                    .address(address)
                    .usage(usage)
                    .build();
                vulkan_device
                    .descriptor_buffer_extension()
                    .expect("descriptor-buffer compute pipeline requires extension")
                    .cmd_bind_descriptor_buffers(cmdbuf, &[binding_info]);
            }
            let pipeline = *pipeline.lock().unwrap();
            if pipeline == vk::Pipeline::null() {
                return;
            }
            logical.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::COMPUTE, pipeline);
            if descriptor_set_layout == vk::DescriptorSetLayout::null() {
                return;
            }
            if is_rescaling {
                logical.cmd_push_constants(
                    cmdbuf,
                    pipeline_layout,
                    vk::ShaderStageFlags::COMPUTE,
                    RESCALING_LAYOUT_WORDS_OFFSET,
                    bytemuck::bytes_of(&rescaling_data),
                );
            }
            let descriptor_data = descriptor_data as *const std::ffi::c_void;
            if uses_descriptor_buffer {
                vulkan_device
                    .descriptor_buffer_extension()
                    .expect("descriptor-buffer compute pipeline requires extension")
                    .cmd_set_descriptor_buffer_offsets(
                        cmdbuf,
                        vk::PipelineBindPoint::COMPUTE,
                        pipeline_layout,
                        0,
                        &[0],
                        &[descriptor_buffer_offset],
                    );
            } else if uses_push_descriptor {
                push_descriptor
                    .as_ref()
                    .expect("compute push-descriptor pipeline requires extension")
                    .cmd_push_descriptor_set_with_template(
                        cmdbuf,
                        descriptor_update_template,
                        pipeline_layout,
                        0,
                        descriptor_data,
                    );
            } else {
                let descriptor_set = descriptor_allocator
                    .as_ref()
                    .expect("descriptor-set compute pipeline requires an initialized allocator")
                    .commit()
                    .expect("failed to commit compute descriptor set");
                logical.update_descriptor_set_with_template(
                    descriptor_set,
                    descriptor_update_template,
                    descriptor_data,
                );
                logical.cmd_bind_descriptor_sets(
                    cmdbuf,
                    vk::PipelineBindPoint::COMPUTE,
                    pipeline_layout,
                    0,
                    &[descriptor_set],
                    &[],
                );
            }
        });
        true
    }

    /// Returns whether the pipeline has finished building.
    pub fn is_built(&self) -> bool {
        self.is_built.load(Ordering::Relaxed)
    }

    /// Port of upstream `ComputePipeline::IsBound`.
    pub fn is_bound(&self) -> bool {
        *self.pipeline.lock().unwrap() != vk::Pipeline::null()
    }

    /// Returns the pipeline handle.
    pub fn pipeline(&self) -> vk::Pipeline {
        self.wait_for_build();
        *self.pipeline.lock().unwrap()
    }

    /// Stable state captured by queued dispatches for upstream `IsBound()`.
    pub fn pipeline_state(&self) -> Arc<Mutex<vk::Pipeline>> {
        Arc::clone(&self.pipeline)
    }

    /// Returns the pipeline layout handle.
    pub fn pipeline_layout(&self) -> vk::PipelineLayout {
        self.pipeline_layout
    }

    /// Returns the descriptor set layout.
    pub fn descriptor_set_layout(&self) -> vk::DescriptorSetLayout {
        self.descriptor_set_layout
    }

    /// Returns the descriptor update template.
    pub fn descriptor_update_template(&self) -> vk::DescriptorUpdateTemplate {
        self.descriptor_update_template
    }

    pub fn info(&self) -> &ShaderInfo {
        &self.info
    }

    pub fn uniform_buffer_sizes(
        &self,
    ) -> &crate::buffer_cache::buffer_cache_base::ComputeUniformBufferSizes {
        &self.uniform_buffer_sizes
    }

    fn wait_for_build(&self) {
        if self.is_built.load(Ordering::Acquire) {
            return;
        }
        let lock = self.build_mutex.lock().unwrap();
        let _guard = self
            .build_condvar
            .wait_while(lock, |_| !self.is_built.load(Ordering::Relaxed))
            .unwrap();
    }
}

#[derive(Default)]
struct ComputeTextureHandles {
    views: Vec<ImageViewInOut>,
    sampler_indices: Vec<u32>,
}

trait ComputeTextureHandleDescriptor {
    fn has_secondary(&self) -> bool;
    fn cbuf_index(&self) -> u32;
    fn cbuf_offset(&self) -> u32;
    fn shift_left(&self) -> u32;
    fn secondary_cbuf_index(&self) -> u32;
    fn secondary_cbuf_offset(&self) -> u32;
    fn secondary_shift_left(&self) -> u32;
    fn size_shift(&self) -> u32;
}

macro_rules! impl_compute_texture_handle_descriptor {
    ($ty:ty) => {
        impl ComputeTextureHandleDescriptor for $ty {
            fn has_secondary(&self) -> bool {
                self.has_secondary
            }
            fn cbuf_index(&self) -> u32 {
                self.cbuf_index
            }
            fn cbuf_offset(&self) -> u32 {
                self.cbuf_offset
            }
            fn shift_left(&self) -> u32 {
                self.shift_left
            }
            fn secondary_cbuf_index(&self) -> u32 {
                self.secondary_cbuf_index
            }
            fn secondary_cbuf_offset(&self) -> u32 {
                self.secondary_cbuf_offset
            }
            fn secondary_shift_left(&self) -> u32 {
                self.secondary_shift_left
            }
            fn size_shift(&self) -> u32 {
                self.size_shift
            }
        }
    };
}

impl_compute_texture_handle_descriptor!(TextureBufferDescriptor);
impl_compute_texture_handle_descriptor!(TextureDescriptor);

impl ComputeTextureHandleDescriptor for ImageBufferDescriptor {
    fn has_secondary(&self) -> bool {
        false
    }
    fn cbuf_index(&self) -> u32 {
        self.cbuf_index
    }
    fn cbuf_offset(&self) -> u32 {
        self.cbuf_offset
    }
    fn shift_left(&self) -> u32 {
        0
    }
    fn secondary_cbuf_index(&self) -> u32 {
        0
    }
    fn secondary_cbuf_offset(&self) -> u32 {
        0
    }
    fn secondary_shift_left(&self) -> u32 {
        0
    }
    fn size_shift(&self) -> u32 {
        self.size_shift
    }
}

impl ComputeTextureHandleDescriptor for ImageDescriptor {
    fn has_secondary(&self) -> bool {
        false
    }
    fn cbuf_index(&self) -> u32 {
        self.cbuf_index
    }
    fn cbuf_offset(&self) -> u32 {
        self.cbuf_offset
    }
    fn shift_left(&self) -> u32 {
        0
    }
    fn secondary_cbuf_index(&self) -> u32 {
        0
    }
    fn secondary_cbuf_offset(&self) -> u32 {
        0
    }
    fn secondary_shift_left(&self) -> u32 {
        0
    }
    fn size_shift(&self) -> u32 {
        self.size_shift
    }
}

fn read_texture_handle(
    qmd: &LaunchParams,
    desc: &impl ComputeTextureHandleDescriptor,
    index: u32,
    read_u32: &mut impl FnMut(u64) -> u32,
) -> (u32, u32) {
    assert_ne!((qmd.const_buffer_enable_mask >> desc.cbuf_index()) & 1, 0);
    let index_offset = index << desc.size_shift();
    let address = qmd.const_buffers[desc.cbuf_index() as usize]
        .address
        .wrapping_add(desc.cbuf_offset().wrapping_add(index_offset) as u64);
    let raw = if desc.has_secondary() {
        assert_ne!(
            (qmd.const_buffer_enable_mask >> desc.secondary_cbuf_index()) & 1,
            0
        );
        let secondary_address = qmd.const_buffers[desc.secondary_cbuf_index() as usize]
            .address
            .wrapping_add(desc.secondary_cbuf_offset().wrapping_add(index_offset) as u64);
        (read_u32(address) << desc.shift_left())
            | (read_u32(secondary_address) << desc.secondary_shift_left())
    } else {
        read_u32(address)
    };
    texture_pair(raw, qmd.linked_tsc)
}

fn collect_texture_handles(
    info: &ShaderInfo,
    dispatch: &DispatchCall,
    mut read_u32: impl FnMut(u64) -> u32,
) -> ComputeTextureHandles {
    let mut result = ComputeTextureHandles::default();
    for desc in &info.texture_buffer_descriptors {
        for index in 0..desc.count {
            result.views.push(ImageViewInOut {
                index: read_texture_handle(
                    &dispatch.launch_description,
                    desc,
                    index,
                    &mut read_u32,
                )
                .0,
                ..Default::default()
            });
        }
    }
    for desc in &info.image_buffer_descriptors {
        for index in 0..desc.count {
            result.views.push(ImageViewInOut {
                index: read_texture_handle(
                    &dispatch.launch_description,
                    desc,
                    index,
                    &mut read_u32,
                )
                .0,
                ..Default::default()
            });
        }
    }
    for desc in &info.texture_descriptors {
        for index in 0..desc.count {
            let (view, sampler) =
                read_texture_handle(&dispatch.launch_description, desc, index, &mut read_u32);
            result.views.push(ImageViewInOut {
                index: view,
                ..Default::default()
            });
            result.sampler_indices.push(sampler);
        }
    }
    for desc in &info.image_descriptors {
        for index in 0..desc.count {
            result.views.push(ImageViewInOut {
                index: read_texture_handle(
                    &dispatch.launch_description,
                    desc,
                    index,
                    &mut read_u32,
                )
                .0,
                blacklist: desc.is_written,
                ..Default::default()
            });
        }
    }
    result
}

fn bind_compute_texture_buffer(
    buffer_cache: &mut VulkanCommonBufferCache,
    texture_cache: &TextureCache,
    view: &ImageViewInOut,
    index: usize,
    is_written: bool,
    is_image: bool,
    explicit_format: Option<ImageFormat>,
) {
    let (gpu_addr, size, mut format) = texture_cache.image_view_buffer_info(view.id).unwrap_or((
        0,
        0,
        crate::surface::PixelFormat::Invalid,
    ));
    if let Some(explicit) = explicit_format.and_then(pixel_format_from_image_format) {
        format = explicit;
    }
    buffer_cache.bind_compute_texture_buffer(index, gpu_addr, size, format, is_written, is_image);
}

impl Drop for ComputePipeline {
    fn drop(&mut self) {
        self.wait_for_build();
        let device = self.device_owner.get().get_logical();
        unsafe {
            let pipeline = *self.pipeline.lock().unwrap();
            if pipeline != vk::Pipeline::null() {
                device.destroy_pipeline(pipeline, None);
            }
            if self.descriptor_update_template != vk::DescriptorUpdateTemplate::null() {
                device.destroy_descriptor_update_template(self.descriptor_update_template, None);
            }
            if self.pipeline_layout != vk::PipelineLayout::null() {
                device.destroy_pipeline_layout(self.pipeline_layout, None);
            }
            if self.descriptor_set_layout != vk::DescriptorSetLayout::null() {
                device.destroy_descriptor_set_layout(self.descriptor_set_layout, None);
            }
            if self.spv_module != vk::ShaderModule::null() {
                device.destroy_shader_module(self.spv_module, None);
            }
        }
    }
}

#[cfg(test)]
fn compute_descriptor_set_layout_bindings(
    info: &ShaderInfo,
) -> Vec<vk::DescriptorSetLayoutBinding> {
    let mut bindings = Vec::new();
    let mut binding = 0u32;
    let mut push_binding = |descriptor_type: vk::DescriptorType, count: u32| {
        if count == 0 {
            return;
        }
        bindings.push(
            vk::DescriptorSetLayoutBinding::builder()
                .binding(binding)
                .descriptor_type(descriptor_type)
                .descriptor_count(count)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
                .build(),
        );
        binding += 1;
    };

    for desc in &info.constant_buffer_descriptors {
        push_binding(vk::DescriptorType::UNIFORM_BUFFER, desc.count);
    }
    for desc in &info.storage_buffers_descriptors {
        push_binding(vk::DescriptorType::STORAGE_BUFFER, desc.count);
    }
    for desc in &info.texture_buffer_descriptors {
        push_binding(vk::DescriptorType::UNIFORM_TEXEL_BUFFER, desc.count);
    }
    for desc in &info.image_buffer_descriptors {
        push_binding(vk::DescriptorType::STORAGE_TEXEL_BUFFER, desc.count);
    }
    for desc in &info.texture_descriptors {
        push_binding(vk::DescriptorType::COMBINED_IMAGE_SAMPLER, desc.count);
    }
    for desc in &info.image_descriptors {
        push_binding(vk::DescriptorType::STORAGE_IMAGE, desc.count);
    }
    bindings
}

#[cfg(test)]
mod tests {
    use super::*;
    use shader_recompiler::shader_info::{
        ConstantBufferDescriptor, ImageBufferDescriptor, ImageDescriptor, StorageBufferDescriptor,
        TextureBufferDescriptor, TextureDescriptor,
    };
    use shader_recompiler::shader_info::{ImageFormat, TextureType};

    #[test]
    fn compute_pipeline_struct_size() {
        // Ensure the struct can be constructed (compilation test)
        let _size = std::mem::size_of::<ComputePipeline>();
    }

    #[test]
    fn compute_descriptor_bindings_follow_upstream_descriptor_order() {
        let mut info = ShaderInfo::default();
        info.constant_buffer_descriptors
            .push(ConstantBufferDescriptor { index: 0, count: 2 });
        info.storage_buffers_descriptors
            .push(StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                is_written: true,
            });
        info.texture_buffer_descriptors
            .push(TextureBufferDescriptor {
                has_secondary: false,
                cbuf_index: 0,
                cbuf_offset: 0,
                shift_left: 0,
                secondary_cbuf_index: 0,
                secondary_cbuf_offset: 0,
                secondary_shift_left: 0,
                count: 3,
                size_shift: 0,
            });
        info.image_buffer_descriptors.push(ImageBufferDescriptor {
            format: ImageFormat::R32Uint,
            is_written: true,
            is_read: true,
            is_integer: true,
            cbuf_index: 0,
            cbuf_offset: 0,
            count: 4,
            size_shift: 0,
        });
        info.texture_descriptors.push(TextureDescriptor {
            texture_type: TextureType::Color2D,
            is_depth: false,
            is_multisample: false,
            is_integer: false,
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count: 5,
            size_shift: 0,
        });
        info.image_descriptors.push(ImageDescriptor {
            texture_type: TextureType::Color2D,
            format: ImageFormat::R32Uint,
            is_written: true,
            is_read: true,
            is_integer: true,
            cbuf_index: 0,
            cbuf_offset: 0,
            count: 6,
            size_shift: 0,
        });

        let bindings = compute_descriptor_set_layout_bindings(&info);
        let types: Vec<_> = bindings
            .iter()
            .map(|binding| binding.descriptor_type)
            .collect();
        let counts: Vec<_> = bindings
            .iter()
            .map(|binding| binding.descriptor_count)
            .collect();

        assert_eq!(
            types,
            vec![
                vk::DescriptorType::UNIFORM_BUFFER,
                vk::DescriptorType::STORAGE_BUFFER,
                vk::DescriptorType::UNIFORM_TEXEL_BUFFER,
                vk::DescriptorType::STORAGE_TEXEL_BUFFER,
                vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                vk::DescriptorType::STORAGE_IMAGE,
            ]
        );
        assert_eq!(counts, vec![2, 1, 3, 4, 5, 6]);
        assert!(bindings
            .iter()
            .all(|binding| binding.stage_flags == vk::ShaderStageFlags::COMPUTE));
    }
}
