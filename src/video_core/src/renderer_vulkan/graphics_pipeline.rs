// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden `vk_graphics_pipeline.h` / `vk_graphics_pipeline.cpp`.
//!
//! Shader translation, runtime-info construction, and shader-module creation
//! belong to `pipeline_cache.rs`, matching Eden `vk_pipeline_cache.cpp`.

use ash::vk;
use common::thread_worker::ThreadWorker;
use log::warn;
use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::buffer_cache::buffer_cache_base::{
    BufferCacheRuntime, UniformBufferSizes, NUM_GRAPHICS_UNIFORM_BUFFERS, NUM_STAGES,
};
use crate::engines::draw_manager::Maxwell3DDrawView;
use crate::engines::maxwell_3d::{CullFace, VertexAttribSize, VertexAttribType};
use crate::gpu::RenderTargetFormat;
use crate::gpu_logging::{get_instance, is_active};
use crate::memory_manager::MemoryManagerHandle;
use crate::shader_cache::NUM_PROGRAMS;
use crate::shader_notify::ShaderNotifyHandle;
use crate::surface::{
    pixel_format_from_depth_format, pixel_format_from_render_target_format, PixelFormat,
};
use crate::texture_cache::texture_cache_base::ImageViewInOut;
use crate::texture_cache::types::NULL_IMAGE_VIEW_ID;
use crate::textures::texture::texture_pair;
use crate::textures::texture::MsaaMode;
use crate::vulkan_common::vulkan_device::{Device, DeviceReference};
use shader_recompiler::backend::spirv::emit_spirv::{
    RENDERAREA_LAYOUT_OFFSET, RESCALING_LAYOUT_DOWN_FACTOR_OFFSET, RESCALING_LAYOUT_WORDS_OFFSET,
};
use shader_recompiler::shader_info::{num_descriptors, Info as ShaderInfo};
#[cfg(test)]
use shader_recompiler::{CompiledShader, ShaderStage};
use smallvec::SmallVec;

use super::buffer_cache::VulkanCommonBufferCache;
use super::descriptor_buffer::DescriptorBufferRing;
use super::descriptor_pool::{DescriptorAllocator, DescriptorPool};
use super::fixed_pipeline_state::{pack_logic_op, DynamicState, FixedPipelineState};
use super::maxwell_to_vk;
use super::pipeline_helper::{
    num_descriptor_entries, pixel_format_from_image_format, push_image_descriptors,
    write_descriptor_buffer, DescriptorBufferLayout, DescriptorLayoutBuilder,
    RescalingPushConstant, NUM_TEXTURE_AND_IMAGE_SCALING_WORDS,
};
use super::pipeline_statistics::PipelineStatistics;
use super::render_pass_cache::{RenderPassCache, RenderPassKey};
use super::scheduler::Scheduler;
use super::texture_cache::TextureCache;
use super::update_descriptor::{DescriptorUpdateEntry, UpdateDescriptorQueue};

const NUM_VK_GRAPHICS_STAGES: usize = 5;

type GraphicsImageViews = SmallVec<[ImageViewInOut; 64]>;
type GraphicsSamplers = SmallVec<[crate::texture_cache::types::SamplerId; 64]>;

/// Stable non-owning counterparts of the reference members held by upstream
/// `GraphicsPipeline`.
#[derive(Clone, Copy)]
pub(crate) struct GraphicsPipelineRuntime {
    scheduler: NonNull<Scheduler>,
    buffer_cache: NonNull<VulkanCommonBufferCache>,
    texture_cache: NonNull<TextureCache>,
    guest_descriptor_queue: NonNull<UpdateDescriptorQueue>,
    descriptor_buffer_ring: NonNull<DescriptorBufferRing>,
    descriptor_pool: NonNull<DescriptorPool>,
    render_pass_cache: NonNull<RenderPassCache>,
}

impl GraphicsPipelineRuntime {
    pub(crate) fn new(
        scheduler: &mut Scheduler,
        buffer_cache: &mut VulkanCommonBufferCache,
        texture_cache: &mut TextureCache,
        guest_descriptor_queue: &mut UpdateDescriptorQueue,
        descriptor_buffer_ring: &mut DescriptorBufferRing,
        descriptor_pool: &mut DescriptorPool,
        render_pass_cache: &mut RenderPassCache,
    ) -> Self {
        Self {
            scheduler: NonNull::from(scheduler),
            buffer_cache: NonNull::from(buffer_cache),
            texture_cache: NonNull::from(texture_cache),
            guest_descriptor_queue: NonNull::from(guest_descriptor_queue),
            descriptor_buffer_ring: NonNull::from(descriptor_buffer_ring),
            descriptor_pool: NonNull::from(descriptor_pool),
            render_pass_cache: NonNull::from(render_pass_cache),
        }
    }

    unsafe fn descriptor_pool(&self) -> &DescriptorPool {
        unsafe { self.descriptor_pool.as_ref() }
    }

    pub(crate) unsafe fn render_pass_cache(&self) -> &RenderPassCache {
        unsafe { self.render_pass_cache.as_ref() }
    }
}

// SAFETY: every pointer targets stable storage owned by RasterizerVulkan (or
// its renderer-owned Scheduler). PipelineCache joins its workers before those
// owners are destroyed, and configuration runs on the GPU thread.
unsafe impl Send for GraphicsPipelineRuntime {}
unsafe impl Sync for GraphicsPipelineRuntime {}

/// Draw-scoped engine state installed by `GraphicsPipeline::set_engine` and
/// consumed by the immediately following `configure`, matching upstream's
/// `SetEngine`/`Configure` ordering without retaining stack pointers.
struct GraphicsPipelineEngine {
    draw: NonNull<Maxwell3DDrawView<'static>>,
    dirty_flags: NonNull<[bool; 256]>,
    gpu_memory: GraphicsGpuMemory,
    push_descriptor: Option<ash::extensions::khr::PushDescriptor>,
    fallback_sampler: vk::Sampler,
}

/// Owned state captured by the asynchronous constructor job. Eden's worker
/// lambda captures `this`; Rust captures only the immutable fields consumed by
/// `GraphicsPipeline::MakePipeline` so moving the finished object into a map
/// cannot invalidate a worker pointer.
struct GraphicsPipelineBuildSnapshot {
    device_owner: DeviceReference,
    key: GraphicsPipelineKey,
    pipeline_cache: vk::PipelineCache,
    pipeline_layout: vk::PipelineLayout,
    fragment_has_color0_output: bool,
    uses_descriptor_buffer: bool,
    stage_infos: Arc<[ShaderInfo; NUM_VK_GRAPHICS_STAGES]>,
    shader_modules: [vk::ShaderModule; NUM_VK_GRAPHICS_STAGES],
    num_image_elements: usize,
}

#[derive(Default)]
struct DescriptorPayloadState {
    payload: Vec<DescriptorUpdateEntry>,
    buffer_offset: vk::DeviceSize,
    buffer_chunk: u32,
    buffer_generation: u64,
}

unsafe impl Send for GraphicsPipelineBuildSnapshot {}

unsafe impl Send for GraphicsPipelineEngine {}

type GpuReader<'a> = dyn Fn(u64, &mut [u8]) + 'a;
type GpuUnsafeReader<'a> = dyn Fn(u64, &mut [u8]) -> bool + 'a;

enum GraphicsGpuMemory {
    Memory(MemoryManagerHandle),
    LegacyReaders {
        read: *const GpuReader<'static>,
        read_unsafe: *const GpuUnsafeReader<'static>,
    },
}

unsafe impl Send for GraphicsGpuMemory {}

impl GraphicsGpuMemory {
    fn read(&self, addr: u64, output: &mut [u8]) {
        match self {
            // SAFETY: the rasterizer retains the channel's owning
            // `Arc<Mutex<MemoryManager>>` for the complete configure call and
            // serializes this GPU-thread access, matching upstream's stored
            // `Tegra::MemoryManager*`.
            Self::Memory(memory) => unsafe {
                memory.as_ref().read_block(addr, output);
            },
            Self::LegacyReaders { read, .. } => unsafe { (&**read)(addr, output) },
        }
    }

    fn read_unsafe(&self, addr: u64, output: &mut [u8]) -> bool {
        match self {
            // SAFETY: same channel-owner invariant as `read`.
            Self::Memory(memory) => unsafe { memory.as_ref().read_block_unsafe(addr, output) },
            Self::LegacyReaders { read_unsafe, .. } => unsafe { (&**read_unsafe)(addr, output) },
        }
    }
}

struct PreparedGraphicsDescriptors {
    rescaling_data: [u32; NUM_TEXTURE_AND_IMAGE_SCALING_WORDS as usize],
    render_area_data: [f32; 4],
}

#[derive(Clone, Copy)]
struct DescriptorData(*const DescriptorUpdateEntry);

// The update queue retains each acquired payload until the scheduler has
// consumed its frame. This is the Rust counterpart of the raw descriptor-data
// pointer captured by upstream `GraphicsPipeline::ConfigureDraw`.
unsafe impl Send for DescriptorData {}

impl DescriptorData {
    fn as_ptr(self) -> *const DescriptorUpdateEntry {
        self.0
    }
}

fn bytes_of<T: Sized>(value: &T) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts((value as *const T).cast::<u8>(), std::mem::size_of::<T>())
    }
}

fn slice_bytes<T>(values: &[T]) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), std::mem::size_of_val(values))
    }
}

fn should_update_descriptor_set(
    bind_pipeline: bool,
    previous: &[DescriptorUpdateEntry],
    current: &[DescriptorUpdateEntry],
) -> bool {
    bind_pipeline
        || previous.len() != current.len()
        || slice_bytes(previous) != slice_bytes(current)
}

fn graphics_pipeline_bind_log_info(key: &GraphicsPipelineKey) -> String {
    format!("hash={:#016x}", key.hash_value())
}

fn graphics_pipeline_creation_log_info(stage_count: usize, attachment_count: usize) -> String {
    format!("GraphicsPipeline created: stages={stage_count}, attachments={attachment_count}")
}

trait ConfigureSpec {
    const ENABLED_STAGES: [bool; NUM_VK_GRAPHICS_STAGES];
    const HAS_STORAGE_BUFFERS: bool;
    const HAS_TEXTURE_BUFFERS: bool;
    const HAS_IMAGE_BUFFERS: bool;
    const HAS_IMAGES: bool;
}

struct SimpleVertexFragmentSpec;
struct SimpleVertexSpec;
struct SimpleStorageSpec;
struct SimpleImageSpec;
struct DefaultSpec;

impl ConfigureSpec for SimpleVertexFragmentSpec {
    const ENABLED_STAGES: [bool; NUM_VK_GRAPHICS_STAGES] = [true, false, false, false, true];
    const HAS_STORAGE_BUFFERS: bool = false;
    const HAS_TEXTURE_BUFFERS: bool = false;
    const HAS_IMAGE_BUFFERS: bool = false;
    const HAS_IMAGES: bool = false;
}

impl ConfigureSpec for SimpleVertexSpec {
    const ENABLED_STAGES: [bool; NUM_VK_GRAPHICS_STAGES] = [true, false, false, false, false];
    const HAS_STORAGE_BUFFERS: bool = false;
    const HAS_TEXTURE_BUFFERS: bool = false;
    const HAS_IMAGE_BUFFERS: bool = false;
    const HAS_IMAGES: bool = false;
}

impl ConfigureSpec for SimpleStorageSpec {
    const ENABLED_STAGES: [bool; NUM_VK_GRAPHICS_STAGES] = [true, false, false, false, true];
    const HAS_STORAGE_BUFFERS: bool = true;
    const HAS_TEXTURE_BUFFERS: bool = false;
    const HAS_IMAGE_BUFFERS: bool = false;
    const HAS_IMAGES: bool = false;
}

impl ConfigureSpec for SimpleImageSpec {
    const ENABLED_STAGES: [bool; NUM_VK_GRAPHICS_STAGES] = [true, false, false, false, true];
    const HAS_STORAGE_BUFFERS: bool = false;
    const HAS_TEXTURE_BUFFERS: bool = false;
    const HAS_IMAGE_BUFFERS: bool = false;
    const HAS_IMAGES: bool = true;
}

impl ConfigureSpec for DefaultSpec {
    const ENABLED_STAGES: [bool; NUM_VK_GRAPHICS_STAGES] = [true, true, true, true, true];
    const HAS_STORAGE_BUFFERS: bool = true;
    const HAS_TEXTURE_BUFFERS: bool = true;
    const HAS_IMAGE_BUFFERS: bool = true;
    const HAS_IMAGES: bool = true;
}

type ConfigureFunc = fn(&GraphicsPipeline, GraphicsPipelineEngine, bool) -> bool;

fn passes<S: ConfigureSpec>(
    modules: &[vk::ShaderModule; NUM_VK_GRAPHICS_STAGES],
    stage_infos: &[ShaderInfo; NUM_VK_GRAPHICS_STAGES],
) -> bool {
    for stage in 0..NUM_VK_GRAPHICS_STAGES {
        if !S::ENABLED_STAGES[stage] && modules[stage] != vk::ShaderModule::null() {
            return false;
        }
        let info = &stage_infos[stage];
        if !S::HAS_STORAGE_BUFFERS && !info.storage_buffers_descriptors.is_empty() {
            return false;
        }
        if !S::HAS_TEXTURE_BUFFERS && !info.texture_buffer_descriptors.is_empty() {
            return false;
        }
        if !S::HAS_IMAGE_BUFFERS && !info.image_buffer_descriptors.is_empty() {
            return false;
        }
        if !S::HAS_IMAGES && !info.image_descriptors.is_empty() {
            return false;
        }
    }
    true
}

fn configure_spec<S: ConfigureSpec>(
    pipeline: &GraphicsPipeline,
    engine: GraphicsPipelineEngine,
    is_indexed: bool,
) -> bool {
    pipeline.configure_impl::<S>(engine, is_indexed)
}

fn find_spec(
    modules: &[vk::ShaderModule; NUM_VK_GRAPHICS_STAGES],
    stage_infos: &[ShaderInfo; NUM_VK_GRAPHICS_STAGES],
) -> ConfigureFunc {
    if passes::<SimpleVertexSpec>(modules, stage_infos) {
        configure_spec::<SimpleVertexSpec>
    } else if passes::<SimpleVertexFragmentSpec>(modules, stage_infos) {
        configure_spec::<SimpleVertexFragmentSpec>
    } else if passes::<SimpleStorageSpec>(modules, stage_infos) {
        configure_spec::<SimpleStorageSpec>
    } else if passes::<SimpleImageSpec>(modules, stage_infos) {
        configure_spec::<SimpleImageSpec>
    } else {
        configure_spec::<DefaultSpec>
    }
}

fn configure_func(
    modules: &[vk::ShaderModule; NUM_VK_GRAPHICS_STAGES],
    stage_infos: &[ShaderInfo; NUM_VK_GRAPHICS_STAGES],
) -> ConfigureFunc {
    find_spec(modules, stage_infos)
}

/// Port of anonymous-namespace `DecodeFormat` from
/// `vk_graphics_pipeline.cpp`.
fn decode_format(encoded_format: u8) -> PixelFormat {
    if encoded_format as u32 == RenderTargetFormat::None as u32 {
        PixelFormat::Invalid
    } else {
        pixel_format_from_render_target_format(encoded_format as u32)
    }
}

/// Port of anonymous-namespace `MakeRenderPassKey` from
/// `vk_graphics_pipeline.cpp`.
pub(crate) fn make_render_pass_key(state: &FixedPipelineState, device: &Device) -> RenderPassKey {
    let mut key = RenderPassKey::default();
    for (index, &encoded_format) in state.color_formats.iter().enumerate() {
        key.color_formats[index] = decode_format(encoded_format);
    }
    if state.depth_enabled() {
        key.depth_format = pixel_format_from_depth_format(state.depth_format());
    }
    let msaa_mode = MsaaMode::from_raw(state.msaa_mode_raw()).unwrap_or_else(|| {
        debug_assert!(false, "Invalid msaa_mode={}", state.msaa_mode_raw());
        MsaaMode::Msaa1x1
    });
    key.samples = maxwell_to_vk::msaa_mode(msaa_mode);
    let has_color = key
        .color_formats
        .iter()
        .any(|&format| format != PixelFormat::Invalid);
    key.resolve_color =
        key.samples != vk::SampleCountFlags::TYPE_1 && has_color && device.is_tiler();
    key
}

/// Mechanical transport of upstream `Device::SupportsDynamicState3*` results
/// through Reden's split Vulkan owners.
#[derive(Debug, Clone, Copy, Default)]
pub struct DynamicState3Support {
    pub depth_clamp_enable: bool,
    pub logic_op_enable: bool,
    pub line_rasterization_mode: bool,
    pub conservative_rasterization_mode: bool,
    pub line_stipple_enable: bool,
    pub alpha_to_coverage_enable: bool,
    pub alpha_to_one_enable: bool,
}

/// Cache key for graphics pipeline lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphicsPipelineKey {
    /// Upstream shader unique hashes, indexed by Maxwell shader program slot.
    pub unique_hashes: [u64; NUM_PROGRAMS],
    /// Fixed (non-dynamic) pipeline state.
    pub fixed_state: FixedPipelineState,
}

impl Default for GraphicsPipelineKey {
    fn default() -> Self {
        Self {
            unique_hashes: [0; NUM_PROGRAMS],
            fixed_state: FixedPipelineState::default(),
        }
    }
}

impl GraphicsPipelineKey {
    /// Port of upstream `GraphicsPipelineCacheKey::Size`.
    pub fn serialized_size(&self) -> usize {
        std::mem::size_of::<[u64; NUM_PROGRAMS]>() + self.fixed_state.serialized_size()
    }

    pub fn to_cache_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.serialized_size());
        for hash in self.unique_hashes {
            bytes.extend_from_slice(&hash.to_le_bytes());
        }
        self.fixed_state.write_prefix_bytes(&mut bytes);
        debug_assert_eq!(bytes.len(), self.serialized_size());
        bytes
    }

    pub fn read_from_file(file: &mut std::fs::File) -> std::io::Result<Self> {
        use std::io::Read;

        let mut unique_hashes = [0u64; NUM_PROGRAMS];
        for hash in &mut unique_hashes {
            let mut buf = [0u8; 8];
            file.read_exact(&mut buf)?;
            *hash = u64::from_le_bytes(buf);
        }
        let fixed_state = FixedPipelineState::read_from_file(file)?;
        Ok(Self {
            unique_hashes,
            fixed_state,
        })
    }
}

/// A compiled Vulkan graphics pipeline.
pub struct GraphicsPipeline {
    device_owner: DeviceReference,
    key: GraphicsPipelineKey,
    pipeline_cache: vk::PipelineCache,
    transitions: RefCell<Vec<(GraphicsPipelineKey, Weak<GraphicsPipeline>)>>,
    pipeline: Arc<Mutex<vk::Pipeline>>,
    pub pipeline_layout: vk::PipelineLayout,
    pub descriptor_set_layout: vk::DescriptorSetLayout,
    pub descriptor_update_template: vk::DescriptorUpdateTemplate,
    pub uses_push_descriptor: bool,
    descriptor_buffer_layout: DescriptorBufferLayout,
    uses_descriptor_buffer: bool,
    /// Total payload entries reserved by upstream `GraphicsPipeline`.
    pub num_descriptor_entries: u32,
    descriptor_allocator: Option<DescriptorAllocator>,
    scheduler: NonNull<Scheduler>,
    buffer_cache: NonNull<VulkanCommonBufferCache>,
    texture_cache: NonNull<TextureCache>,
    guest_descriptor_queue: NonNull<UpdateDescriptorQueue>,
    descriptor_buffer_ring: NonNull<DescriptorBufferRing>,
    engine: RefCell<Option<GraphicsPipelineEngine>>,
    configure_func: ConfigureFunc,
    descriptor_payload_state: RefCell<DescriptorPayloadState>,
    num_image_elements: usize,
    num_textures: u32,
    fragment_has_color0_output: bool,
    pub stage_infos: Arc<[ShaderInfo; 5]>,
    pub enabled_uniform_buffer_masks: [u32; NUM_STAGES as usize],
    pub uniform_buffer_sizes: UniformBufferSizes,
    pub uses_render_area: bool,
    pub uses_rescaling_uniform: bool,
    /// Upstream `std::array<vk::ShaderModule, NUM_STAGES> spv_modules`.
    pub shader_modules: [vk::ShaderModule; NUM_VK_GRAPHICS_STAGES],
    build_condvar: Arc<Condvar>,
    build_mutex: Arc<Mutex<()>>,
    is_built: Arc<AtomicBool>,
}

// Vulkan pipeline objects are opaque device handles. Upstream builds them on
// worker threads and transfers the resulting `unique_ptr<GraphicsPipeline>` to
// the pipeline cache; the Rust preload path mirrors that ownership transfer.
unsafe impl Send for GraphicsPipeline {}

impl Drop for GraphicsPipeline {
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
            device.destroy_pipeline_layout(self.pipeline_layout, None);
            device.destroy_descriptor_set_layout(self.descriptor_set_layout, None);
            for module in self.shader_modules {
                if module != vk::ShaderModule::null() {
                    device.destroy_shader_module(module, None);
                }
            }
        }
    }
}

impl GraphicsPipeline {
    /// Port of upstream `GraphicsPipeline::UsesExtendedDynamicState`.
    pub fn uses_extended_dynamic_state(&self) -> bool {
        self.key.fixed_state.extended_dynamic_state()
    }

    fn create_descriptor_layout(
        device: &Device,
        key: &GraphicsPipelineKey,
        stage_infos: &[ShaderInfo; NUM_VK_GRAPHICS_STAGES],
        descriptor_buffer_ring: &DescriptorBufferRing,
        descriptor_pool: &DescriptorPool,
        scheduler: &Scheduler,
    ) -> Option<GraphicsDescriptorLayout> {
        let mut builder = DescriptorLayoutBuilder::new(device);
        for (stage_index, info) in stage_infos.iter().enumerate() {
            let stage_flags = graphics_stage_flags(stage_index);
            builder.add(info, stage_flags);
        }

        let uses_push_descriptor = builder.can_use_push_descriptor();
        let mut uses_descriptor_buffer =
            builder.can_use_descriptor_buffer() && descriptor_buffer_ring.is_valid();
        let mut descriptor_set_layout = builder
            .create_descriptor_set_layout(uses_push_descriptor, uses_descriptor_buffer)
            .ok()?;
        let mut descriptor_buffer_layout = if uses_descriptor_buffer {
            builder.make_descriptor_buffer_layout(descriptor_set_layout)
        } else {
            DescriptorBufferLayout::default()
        };
        if uses_descriptor_buffer
            && !descriptor_buffer_ring.can_allocate(descriptor_buffer_layout.size)
        {
            log::warn!(
                "Graphics pipeline {:016X} needs {} descriptor bytes per draw, falling back to sets",
                graphics_pipeline_key_cache_hash(key),
                descriptor_buffer_layout.size
            );
            unsafe {
                device
                    .get_logical()
                    .destroy_descriptor_set_layout(descriptor_set_layout, None);
            }
            uses_descriptor_buffer = false;
            descriptor_buffer_layout = DescriptorBufferLayout::default();
            descriptor_set_layout = builder
                .create_descriptor_set_layout(uses_push_descriptor, false)
                .ok()?;
        }

        let pipeline_layout = match builder.create_pipeline_layout(descriptor_set_layout) {
            Ok(layout) => layout,
            Err(_) => {
                unsafe {
                    if descriptor_set_layout != vk::DescriptorSetLayout::null() {
                        device
                            .get_logical()
                            .destroy_descriptor_set_layout(descriptor_set_layout, None);
                    }
                }
                return None;
            }
        };
        let descriptor_update_template = if uses_descriptor_buffer {
            vk::DescriptorUpdateTemplate::null()
        } else {
            match builder.create_template(
                descriptor_set_layout,
                pipeline_layout,
                uses_push_descriptor,
            ) {
                Ok(template) => template,
                Err(_) => {
                    unsafe {
                        device
                            .get_logical()
                            .destroy_pipeline_layout(pipeline_layout, None);
                        if descriptor_set_layout != vk::DescriptorSetLayout::null() {
                            device
                                .get_logical()
                                .destroy_descriptor_set_layout(descriptor_set_layout, None);
                        }
                    }
                    return None;
                }
            }
        };
        let descriptor_allocator = if !uses_push_descriptor && !uses_descriptor_buffer {
            match descriptor_pool.allocator_for_infos(
                device,
                scheduler,
                descriptor_set_layout,
                stage_infos,
            ) {
                Ok(allocator) => Some(allocator),
                Err(error) => {
                    log::warn!("Failed to create graphics descriptor allocator: {error:?}");
                    unsafe {
                        if descriptor_update_template != vk::DescriptorUpdateTemplate::null() {
                            device.get_logical().destroy_descriptor_update_template(
                                descriptor_update_template,
                                None,
                            );
                        }
                        device
                            .get_logical()
                            .destroy_pipeline_layout(pipeline_layout, None);
                        device
                            .get_logical()
                            .destroy_descriptor_set_layout(descriptor_set_layout, None);
                    }
                    return None;
                }
            }
        } else {
            None
        };
        Some(GraphicsDescriptorLayout {
            pipeline_layout,
            descriptor_set_layout,
            descriptor_update_template,
            uses_push_descriptor,
            uses_descriptor_buffer,
            descriptor_buffer_layout,
            descriptor_allocator,
        })
    }

    pub(crate) fn new_unbuilt(
        device_owner: DeviceReference,
        pipeline_cache: vk::PipelineCache,
        shader_notify: ShaderNotifyHandle,
        key: &GraphicsPipelineKey,
        stage_infos: [ShaderInfo; NUM_VK_GRAPHICS_STAGES],
        shader_modules: [vk::ShaderModule; NUM_VK_GRAPHICS_STAGES],
        is_built: bool,
        runtime: GraphicsPipelineRuntime,
    ) -> Option<Self> {
        shader_notify.mark_shader_building();
        let descriptor_layout = unsafe {
            Self::create_descriptor_layout(
                device_owner.get(),
                key,
                &stage_infos,
                runtime.descriptor_buffer_ring.as_ref(),
                runtime.descriptor_pool(),
                runtime.scheduler.as_ref(),
            )?
        };
        let (enabled_uniform_buffer_masks, uniform_buffer_sizes) =
            buffer_cache_metadata(&stage_infos);
        let uses_render_area = stage_infos.iter().any(|info| info.uses_render_area);
        let uses_rescaling_uniform = stage_infos.iter().any(|info| info.uses_rescaling_uniform);
        let num_descriptor_entries = stage_infos.iter().map(num_descriptor_entries).sum();
        let (num_image_elements, num_textures, fragment_has_color0_output) =
            graphics_resource_metadata(&stage_infos);
        let configure_func = configure_func(&shader_modules, &stage_infos);
        Some(Self {
            device_owner,
            key: key.clone(),
            pipeline_cache,
            transitions: RefCell::new(Vec::new()),
            pipeline: Arc::new(Mutex::new(vk::Pipeline::null())),
            pipeline_layout: descriptor_layout.pipeline_layout,
            descriptor_set_layout: descriptor_layout.descriptor_set_layout,
            descriptor_update_template: descriptor_layout.descriptor_update_template,
            uses_push_descriptor: descriptor_layout.uses_push_descriptor,
            descriptor_buffer_layout: descriptor_layout.descriptor_buffer_layout,
            uses_descriptor_buffer: descriptor_layout.uses_descriptor_buffer,
            num_descriptor_entries,
            descriptor_allocator: descriptor_layout.descriptor_allocator,
            scheduler: runtime.scheduler,
            buffer_cache: runtime.buffer_cache,
            texture_cache: runtime.texture_cache,
            guest_descriptor_queue: runtime.guest_descriptor_queue,
            descriptor_buffer_ring: runtime.descriptor_buffer_ring,
            engine: RefCell::new(None),
            configure_func,
            descriptor_payload_state: RefCell::new(DescriptorPayloadState::default()),
            num_image_elements,
            num_textures,
            fragment_has_color0_output,
            stage_infos: Arc::new(stage_infos),
            enabled_uniform_buffer_masks,
            uniform_buffer_sizes,
            uses_render_area,
            uses_rescaling_uniform,
            shader_modules,
            build_condvar: Arc::new(Condvar::new()),
            build_mutex: Arc::new(Mutex::new(())),
            is_built: Arc::new(AtomicBool::new(is_built)),
        })
    }

    /// Port of `GraphicsPipeline::Validate`.
    fn validate(&self) {
        Self::validate_stage_infos(&self.stage_infos, self.num_image_elements);
    }

    fn validate_stage_infos(
        stage_infos: &[ShaderInfo; NUM_VK_GRAPHICS_STAGES],
        expected_num_images: usize,
    ) {
        let num_images: usize = stage_infos
            .iter()
            .map(|info| {
                num_descriptors(&info.texture_buffer_descriptors) as usize
                    + num_descriptors(&info.image_buffer_descriptors) as usize
                    + num_descriptors(&info.texture_descriptors) as usize
                    + num_descriptors(&info.image_descriptors) as usize
            })
            .sum();
        assert_eq!(num_images, expected_num_images);
    }

    /// Port of `GraphicsPipeline::AddTransition`.
    pub fn add_transition(&self, transition: &Rc<GraphicsPipeline>) {
        self.transitions
            .borrow_mut()
            .push((transition.key.clone(), Rc::downgrade(transition)));
    }

    /// Port of `GraphicsPipeline::Next`.
    pub fn next(
        current: &Rc<GraphicsPipeline>,
        current_key: &GraphicsPipelineKey,
    ) -> Option<Rc<GraphicsPipeline>> {
        if &current.key == current_key {
            return Some(Rc::clone(current));
        }
        current
            .transitions
            .borrow()
            .iter()
            .find(|(key, _)| key == current_key)
            .and_then(|(_, pipeline)| pipeline.upgrade())
    }

    /// Port of `GraphicsPipeline::IsBuilt`.
    pub fn is_built(&self) -> bool {
        self.is_built.load(Ordering::Relaxed)
    }

    /// Port of `GraphicsPipeline::SupportsAlphaToCoverage`.
    pub fn supports_alpha_to_coverage(&self) -> bool {
        self.fragment_has_color0_output
    }

    /// Port of `GraphicsPipeline::SupportsAlphaToOne`.
    pub fn supports_alpha_to_one(&self) -> bool {
        self.supports_alpha_to_coverage()
    }

    /// Port of `GraphicsPipeline::HasDynamicVertexInput`.
    pub fn has_dynamic_vertex_input(&self) -> bool {
        self.key.fixed_state.dynamic_vertex_input()
    }

    /// Port of `GraphicsPipeline::SetEngine`.
    pub fn set_engine(
        &self,
        draw: &mut Maxwell3DDrawView<'_>,
        dirty_flags: &mut [bool; 256],
        gpu_memory: MemoryManagerHandle,
        push_descriptor: Option<ash::extensions::khr::PushDescriptor>,
        fallback_sampler: vk::Sampler,
    ) {
        let draw = NonNull::from(draw).cast::<Maxwell3DDrawView<'static>>();
        *self.engine.borrow_mut() = Some(GraphicsPipelineEngine {
            draw,
            dirty_flags: NonNull::from(dirty_flags),
            gpu_memory: GraphicsGpuMemory::Memory(gpu_memory),
            push_descriptor,
            fallback_sampler,
        });
    }

    /// Compatibility bridge for Reden's legacy batched draw entry point,
    /// which supplies readers rather than a channel `MemoryManager` owner.
    pub fn set_engine_with_readers(
        &self,
        draw: &mut Maxwell3DDrawView<'_>,
        dirty_flags: &mut [bool; 256],
        read: &GpuReader<'_>,
        read_unsafe: &GpuUnsafeReader<'_>,
        push_descriptor: Option<ash::extensions::khr::PushDescriptor>,
        fallback_sampler: vk::Sampler,
    ) {
        // SAFETY: `configure` consumes and clears this state immediately in
        // the same `prepare_draw` stack frame, before either reader expires.
        let read = unsafe {
            std::mem::transmute::<*const GpuReader<'_>, *const GpuReader<'static>>(
                read as *const GpuReader<'_>,
            )
        };
        let read_unsafe = unsafe {
            std::mem::transmute::<*const GpuUnsafeReader<'_>, *const GpuUnsafeReader<'static>>(
                read_unsafe as *const GpuUnsafeReader<'_>,
            )
        };
        let draw = NonNull::from(draw).cast::<Maxwell3DDrawView<'static>>();
        *self.engine.borrow_mut() = Some(GraphicsPipelineEngine {
            draw,
            dirty_flags: NonNull::from(dirty_flags),
            gpu_memory: GraphicsGpuMemory::LegacyReaders { read, read_unsafe },
            push_descriptor,
            fallback_sampler,
        });
    }

    /// Port of `GraphicsPipeline::Configure`.
    pub fn configure(&self, is_indexed: bool) -> bool {
        let engine = self
            .engine
            .borrow_mut()
            .take()
            .expect("GraphicsPipeline::SetEngine must precede Configure");
        (self.configure_func)(self, engine, is_indexed)
    }

    /// Port of `GraphicsPipeline::ConfigureImpl`.
    fn configure_impl<S: ConfigureSpec>(
        &self,
        mut engine: GraphicsPipelineEngine,
        is_indexed: bool,
    ) -> bool {
        // SAFETY: `set_engine` is called immediately before `configure` while
        // both draw-scoped values remain exclusively borrowed by the rasterizer.
        let draw = unsafe { engine.draw.as_ref() };
        let dirty_flags = unsafe { engine.dirty_flags.as_mut() };
        let read_gpu = |addr: u64, output: &mut [u8]| {
            engine.gpu_memory.read(addr, output);
        };
        let read_gpu_unsafe =
            |addr: u64, output: &mut [u8]| engine.gpu_memory.read_unsafe(addr, output);
        // SAFETY: PipelineCache initializes these stable pointers before the
        // pipeline enters any runtime cache. RasterizerVulkan holds the four
        // owners for the entire configure call and serializes their mutation.
        let (
            scheduler,
            buffer_cache,
            texture_cache,
            guest_descriptor_queue,
            descriptor_buffer_ring,
        ) = unsafe {
            (
                &mut *self.scheduler.as_ptr(),
                &mut *self.buffer_cache.as_ptr(),
                &mut *self.texture_cache.as_ptr(),
                &mut *self.guest_descriptor_queue.as_ptr(),
                &mut *self.descriptor_buffer_ring.as_ptr(),
            )
        };
        let mut views = GraphicsImageViews::new();
        let mut samplers = GraphicsSamplers::new();
        views.reserve(self.num_image_elements);
        samplers.reserve(self.num_textures as usize);

        let descriptor_sync_regs = draw.descriptor_sync_regs();
        texture_cache
            .base
            .synchronize_graphics_descriptors(descriptor_sync_regs);

        // SAFETY: PipelineCache retains this Rc-owned pipeline, so its
        // per-pipeline size array remains address-stable through every draw.
        unsafe {
            buffer_cache.set_uniform_buffers_state(
                &self.enabled_uniform_buffer_masks,
                &self.uniform_buffer_sizes,
            );
        }

        let via_header_index = descriptor_sync_regs.sampler_binding_via_header;
        let read_u32 = |addr: u64| -> u32 {
            let mut bytes = [0u8; 4];
            read_gpu(addr, &mut bytes);
            u32::from_le_bytes(bytes)
        };
        let read_stage_handle = |stage: usize,
                                 cbuf_index: u32,
                                 cbuf_offset: u32,
                                 size_shift: u32,
                                 element: u32,
                                 has_secondary: bool,
                                 shift_left: u32,
                                 secondary_cbuf_index: u32,
                                 secondary_cbuf_offset: u32,
                                 secondary_shift_left: u32|
         -> (u32, u32) {
            let index_offset = element.wrapping_shl(size_shift);
            let cbuf = draw.const_buffer_binding(stage, cbuf_index as usize);
            assert!(cbuf.enabled);
            let addr = cbuf
                .address
                .wrapping_add(cbuf_offset.wrapping_add(index_offset) as u64);
            if !has_secondary {
                return texture_pair(read_u32(addr), via_header_index);
            }
            let secondary = draw.const_buffer_binding(stage, secondary_cbuf_index as usize);
            assert!(secondary.enabled);
            let secondary_addr = secondary
                .address
                .wrapping_add(secondary_cbuf_offset.wrapping_add(index_offset) as u64);
            texture_pair(
                (read_u32(addr) << shift_left) | (read_u32(secondary_addr) << secondary_shift_left),
                via_header_index,
            )
        };

        {
            let mut config_stage = |stage: usize| {
                let info = &self.stage_infos[stage];
                buffer_cache.unbind_graphics_storage_buffers(stage);
                if S::HAS_STORAGE_BUFFERS {
                    for (ssbo_index, desc) in info.storage_buffers_descriptors.iter().enumerate() {
                        assert_eq!(desc.count, 1);
                        buffer_cache.bind_graphics_storage_buffer(
                            stage,
                            ssbo_index,
                            desc.cbuf_index,
                            desc.cbuf_offset,
                            desc.is_written,
                        );
                    }
                }

                let mut add_view = |tic_id: u32, blacklist: bool| {
                    views.push(ImageViewInOut {
                        index: tic_id,
                        blacklist,
                        id: NULL_IMAGE_VIEW_ID,
                    });
                };
                if S::HAS_TEXTURE_BUFFERS {
                    for desc in &info.texture_buffer_descriptors {
                        for element in 0..desc.count {
                            let (tic_id, _) = read_stage_handle(
                                stage,
                                desc.cbuf_index,
                                desc.cbuf_offset,
                                desc.size_shift,
                                element,
                                desc.has_secondary,
                                desc.shift_left,
                                desc.secondary_cbuf_index,
                                desc.secondary_cbuf_offset,
                                desc.secondary_shift_left,
                            );
                            add_view(tic_id, false);
                        }
                    }
                }
                if S::HAS_IMAGE_BUFFERS {
                    for desc in &info.image_buffer_descriptors {
                        for element in 0..desc.count {
                            let (tic_id, _) = read_stage_handle(
                                stage,
                                desc.cbuf_index,
                                desc.cbuf_offset,
                                desc.size_shift,
                                element,
                                false,
                                0,
                                0,
                                0,
                                0,
                            );
                            add_view(tic_id, false);
                        }
                    }
                }
                for desc in &info.texture_descriptors {
                    for element in 0..desc.count {
                        let (tic_id, tsc_id) = read_stage_handle(
                            stage,
                            desc.cbuf_index,
                            desc.cbuf_offset,
                            desc.size_shift,
                            element,
                            desc.has_secondary,
                            desc.shift_left,
                            desc.secondary_cbuf_index,
                            desc.secondary_cbuf_offset,
                            desc.secondary_shift_left,
                        );
                        add_view(tic_id, false);
                        samplers.push(texture_cache.get_sampler_id(tsc_id, false));
                    }
                }
                if S::HAS_IMAGES {
                    for desc in &info.image_descriptors {
                        for element in 0..desc.count {
                            let (tic_id, _) = read_stage_handle(
                                stage,
                                desc.cbuf_index,
                                desc.cbuf_offset,
                                desc.size_shift,
                                element,
                                false,
                                0,
                                0,
                                0,
                                0,
                            );
                            add_view(tic_id, desc.is_written);
                        }
                    }
                }
            };
            if S::ENABLED_STAGES[0] {
                config_stage(0);
            }
            if S::ENABLED_STAGES[1] {
                config_stage(1);
            }
            if S::ENABLED_STAGES[2] {
                config_stage(2);
            }
            if S::ENABLED_STAGES[3] {
                config_stage(3);
            }
            if S::ENABLED_STAGES[4] {
                config_stage(4);
            }
        }
        assert_eq!(views.len(), self.num_image_elements);
        assert_eq!(samplers.len(), self.num_textures as usize);
        texture_cache.fill_image_views(&mut views, false, S::HAS_IMAGES);

        let mut view_cursor = 0usize;
        {
            let mut bind_stage_info = |stage: usize| {
                let info = &self.stage_infos[stage];
                buffer_cache.unbind_graphics_texture_buffers(stage);
                let mut binding_index = 0usize;
                if S::HAS_TEXTURE_BUFFERS {
                    for desc in &info.texture_buffer_descriptors {
                        for _ in 0..desc.count {
                            Self::bind_graphics_texture_buffer_view(
                                buffer_cache,
                                texture_cache,
                                stage,
                                binding_index,
                                views[view_cursor],
                                false,
                                false,
                                None,
                            );
                            binding_index += 1;
                            view_cursor += 1;
                        }
                    }
                }
                if S::HAS_IMAGE_BUFFERS {
                    for desc in &info.image_buffer_descriptors {
                        for _ in 0..desc.count {
                            Self::bind_graphics_texture_buffer_view(
                                buffer_cache,
                                texture_cache,
                                stage,
                                binding_index,
                                views[view_cursor],
                                desc.is_written,
                                true,
                                Some(desc.format),
                            );
                            binding_index += 1;
                            view_cursor += 1;
                        }
                    }
                }
                view_cursor += num_descriptors(&info.texture_descriptors) as usize;
                if S::HAS_IMAGES {
                    view_cursor += num_descriptors(&info.image_descriptors) as usize;
                }
            };
            if S::ENABLED_STAGES[0] {
                bind_stage_info(0);
            }
            if S::ENABLED_STAGES[1] {
                bind_stage_info(1);
            }
            if S::ENABLED_STAGES[2] {
                bind_stage_info(2);
            }
            if S::ENABLED_STAGES[3] {
                bind_stage_info(3);
            }
            if S::ENABLED_STAGES[4] {
                bind_stage_info(4);
            }
        }

        if draw.transform_feedback_enabled() {
            scheduler.request_outside_render_pass_operation_context();
        }
        buffer_cache.update_graphics_buffers(is_indexed);
        buffer_cache.bind_host_geometry_buffers(is_indexed);

        let expected_descriptor_count = self.num_descriptor_entries as usize;
        guest_descriptor_queue.acquire(
            scheduler,
            expected_descriptor_count,
            self.uses_descriptor_buffer,
        );
        let mut rescaling = RescalingPushConstant::new();
        let mut sampler_cursor = 0usize;
        view_cursor = 0;
        {
            let mut prepare_stage = |stage: usize| {
                let info = &self.stage_infos[stage];
                buffer_cache.bind_host_stage_buffers(stage);
                push_image_descriptors(
                    texture_cache,
                    guest_descriptor_queue,
                    info,
                    &mut rescaling,
                    &samplers,
                    &mut sampler_cursor,
                    &views,
                    &mut view_cursor,
                    engine.fallback_sampler,
                );
            };
            if S::ENABLED_STAGES[0] {
                prepare_stage(0);
            }
            if S::ENABLED_STAGES[1] {
                prepare_stage(1);
            }
            if S::ENABLED_STAGES[2] {
                prepare_stage(2);
            }
            if S::ENABLED_STAGES[3] {
                prepare_stage(3);
            }
            if S::ENABLED_STAGES[4] {
                prepare_stage(4);
            }
        }
        if buffer_cache.any_buffer_uploaded {
            buffer_cache.runtime.post_copy_barrier();
            buffer_cache.any_buffer_uploaded = false;
        }
        let surface_clip = draw.surface_clip();
        let prepared = PreparedGraphicsDescriptors {
            rescaling_data: *rescaling.data(),
            render_area_data: [
                surface_clip.width as f32,
                surface_clip.height as f32,
                0.0,
                0.0,
            ],
        };

        if !texture_cache.update_render_targets(
            &draw.render_targets(),
            dirty_flags,
            &read_gpu_unsafe,
            false,
            None,
        ) {
            return false;
        }
        texture_cache.check_feedback_loop(&views);
        if self.is_built() && *self.pipeline.lock().unwrap() == vk::Pipeline::null() {
            return false;
        }
        self.configure_draw(
            scheduler,
            texture_cache,
            guest_descriptor_queue,
            descriptor_buffer_ring,
            prepared,
            engine.push_descriptor,
        )
    }

    fn bind_graphics_texture_buffer_view(
        buffer_cache: &mut VulkanCommonBufferCache,
        texture_cache: &TextureCache,
        stage: usize,
        index: usize,
        view: ImageViewInOut,
        is_written: bool,
        is_image: bool,
        explicit_format: Option<shader_recompiler::shader_info::ImageFormat>,
    ) {
        let (gpu_addr, size, mut format) = texture_cache
            .image_view_buffer_info(view.id)
            .expect("filled graphics image view must exist in the texture cache");
        if let Some(explicit) = explicit_format.and_then(pixel_format_from_image_format) {
            format = explicit;
        }
        buffer_cache.bind_graphics_texture_buffer(
            stage, index, gpu_addr, size, format, is_written, is_image,
        );
    }

    /// Port of `GraphicsPipeline::ConfigureDraw`.
    fn configure_draw(
        &self,
        scheduler: &mut Scheduler,
        texture_cache: &mut TextureCache,
        guest_descriptor_queue: &mut UpdateDescriptorQueue,
        descriptor_buffer_ring: &mut DescriptorBufferRing,
        prepared: PreparedGraphicsDescriptors,
        push_descriptor: Option<ash::extensions::khr::PushDescriptor>,
    ) -> bool {
        let descriptor_data = DescriptorData(guest_descriptor_queue.update_data());
        let descriptor_entries = unsafe {
            std::slice::from_raw_parts(
                descriptor_data.as_ptr(),
                self.num_descriptor_entries as usize,
            )
        };
        let mut descriptor_buffer_offset = 0;
        let mut descriptor_buffer_chunk = 0;
        if self.descriptor_set_layout != vk::DescriptorSetLayout::null()
            && self.uses_descriptor_buffer
        {
            let mut state = self.descriptor_payload_state.borrow_mut();
            let reuse_allocation = state.buffer_generation
                == descriptor_buffer_ring.current_generation()
                && state.payload.len() == descriptor_entries.len()
                && slice_bytes(&state.payload) == slice_bytes(descriptor_entries);
            if reuse_allocation {
                descriptor_buffer_offset = state.buffer_offset;
                descriptor_buffer_chunk = state.buffer_chunk;
                descriptor_buffer_ring.touch_frame(scheduler);
            } else {
                let allocation =
                    descriptor_buffer_ring.allocate(scheduler, self.descriptor_buffer_layout.size);
                if allocation.host.is_null() {
                    log::debug!("Failed to reserve descriptor memory, skipping draw");
                    return false;
                }
                unsafe {
                    write_descriptor_buffer(
                        self.device_owner.get(),
                        &self.descriptor_buffer_layout,
                        descriptor_entries.as_ptr(),
                        allocation.host,
                    );
                }
                descriptor_buffer_offset = allocation.offset;
                descriptor_buffer_chunk = allocation.chunk;
                state.buffer_offset = allocation.offset;
                state.buffer_chunk = allocation.chunk;
                state.buffer_generation = allocation.generation;
                state.payload.clear();
                state.payload.extend_from_slice(descriptor_entries);
            }
        }
        let target = match texture_cache.get_framebuffer() {
            Ok(target) => target,
            Err(error) => {
                warn!("GraphicsPipeline::Configure failed to get framebuffer: {error:?}");
                return false;
            }
        };
        scheduler.request_renderpass(&target);

        let is_built = self.is_built.load(Ordering::Relaxed);
        if !is_built {
            let build_condvar = Arc::clone(&self.build_condvar);
            let build_mutex = Arc::clone(&self.build_mutex);
            let build_state = Arc::clone(&self.is_built);
            scheduler.record(move |_| {
                let lock = build_mutex.lock().unwrap();
                let _guard = build_condvar
                    .wait_while(lock, |_| !build_state.load(Ordering::Relaxed))
                    .unwrap();
            });
        }
        let is_rescaling = texture_cache.base.is_rescaling;
        let update_rescaling = scheduler.update_rescaling(is_rescaling);
        let pipeline = Arc::clone(&self.pipeline);
        let bind_pipeline = scheduler.update_graphics_pipeline(Some(self));
        if self.descriptor_set_layout != vk::DescriptorSetLayout::null()
            && self.uses_descriptor_buffer
        {
            scheduler.update_descriptor_buffer_chunk(descriptor_buffer_chunk);
        }

        if bind_pipeline
            && is_active()
            && *common::settings::values().gpu_log_vulkan_calls.get_value()
        {
            let pipeline_info = graphics_pipeline_bind_log_info(&self.key);
            get_instance().log_pipeline_bind(false, &pipeline_info);
        }

        let update_descriptors = if self.descriptor_set_layout != vk::DescriptorSetLayout::null()
            && !self.uses_push_descriptor
            && !self.uses_descriptor_buffer
        {
            let mut state = self.descriptor_payload_state.borrow_mut();
            let update =
                should_update_descriptor_set(bind_pipeline, &state.payload, descriptor_entries);
            if update {
                state.payload.clear();
                state.payload.extend_from_slice(descriptor_entries);
            }
            update
        } else {
            true
        };
        let scale_down_factor = if is_rescaling {
            common::settings::values().resolution_info.down_factor
        } else {
            1.0
        };
        let device = self.device_owner;
        let pipeline_layout = self.pipeline_layout;
        let descriptor_set_layout = self.descriptor_set_layout;
        let descriptor_update_template = self.descriptor_update_template;
        let descriptor_allocator = self
            .descriptor_allocator
            .as_ref()
            .map(DescriptorAllocator::reference);
        let uses_push_descriptor = self.uses_push_descriptor;
        let uses_descriptor_buffer = self.uses_descriptor_buffer;
        // Eden can cache this binding because its scheduler state and command-buffer
        // lifetime are the same object graph. Ruzu records commands for a worker-owned
        // command buffer; validation showed that the cached state can otherwise outlive
        // the command buffer it describes (VUID-08065). Bind immediately before setting
        // the offset so the recorded command stream is self-contained.
        let descriptor_buffer_binding = (self.descriptor_set_layout
            != vk::DescriptorSetLayout::null()
            && uses_descriptor_buffer)
            .then(|| {
                let info = descriptor_buffer_ring.binding_info(descriptor_buffer_chunk);
                (info.address, info.usage)
            });
        let uses_render_area = self.uses_render_area;
        let rescaling_data = prepared.rescaling_data;
        let render_area_data = prepared.render_area_data;
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
                    .expect("descriptor-buffer pipeline requires VK_EXT_descriptor_buffer")
                    .cmd_bind_descriptor_buffers(cmdbuf, &[binding_info]);
            }
            if bind_pipeline {
                let pipeline = *pipeline.lock().unwrap();
                if pipeline == vk::Pipeline::null() {
                    return;
                }
                logical.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::GRAPHICS, pipeline);
            }
            logical.cmd_push_constants(
                cmdbuf,
                pipeline_layout,
                vk::ShaderStageFlags::ALL_GRAPHICS,
                RESCALING_LAYOUT_WORDS_OFFSET,
                bytes_of(&rescaling_data),
            );
            if update_rescaling {
                logical.cmd_push_constants(
                    cmdbuf,
                    pipeline_layout,
                    vk::ShaderStageFlags::ALL_GRAPHICS,
                    RESCALING_LAYOUT_DOWN_FACTOR_OFFSET,
                    bytes_of(&scale_down_factor),
                );
            }
            if uses_render_area {
                logical.cmd_push_constants(
                    cmdbuf,
                    pipeline_layout,
                    vk::ShaderStageFlags::ALL_GRAPHICS,
                    RENDERAREA_LAYOUT_OFFSET,
                    bytes_of(&render_area_data),
                );
            }
            if descriptor_set_layout == vk::DescriptorSetLayout::null() {
                return;
            }
            let descriptor_data = descriptor_data.as_ptr().cast::<std::ffi::c_void>();
            if uses_descriptor_buffer {
                vulkan_device
                    .descriptor_buffer_extension()
                    .expect("descriptor-buffer pipeline requires VK_EXT_descriptor_buffer")
                    .cmd_set_descriptor_buffer_offsets(
                        cmdbuf,
                        vk::PipelineBindPoint::GRAPHICS,
                        pipeline_layout,
                        0,
                        &[0],
                        &[descriptor_buffer_offset],
                    );
            } else if uses_push_descriptor {
                push_descriptor
                    .as_ref()
                    .expect("push-descriptor pipeline requires VK_KHR_push_descriptor")
                    .cmd_push_descriptor_set_with_template(
                        cmdbuf,
                        descriptor_update_template,
                        pipeline_layout,
                        0,
                        descriptor_data,
                    );
            } else if update_descriptors {
                let descriptor_set = descriptor_allocator
                    .as_ref()
                    .expect("descriptor-set pipeline requires an initialized allocator")
                    .commit()
                    .expect("failed to commit graphics descriptor set");
                logical.update_descriptor_set_with_template(
                    descriptor_set,
                    descriptor_update_template,
                    descriptor_data,
                );
                logical.cmd_bind_descriptor_sets(
                    cmdbuf,
                    vk::PipelineBindPoint::GRAPHICS,
                    pipeline_layout,
                    0,
                    &[descriptor_set],
                    &[],
                );
            }
        });
        true
    }

    /// Port of `GraphicsPipeline::MakePipeline`.
    fn make_pipeline(&self, render_pass: vk::RenderPass) -> Option<vk::Pipeline> {
        Self::make_pipeline_from_snapshot(
            &GraphicsPipelineBuildSnapshot {
                device_owner: self.device_owner,
                key: self.key.clone(),
                pipeline_cache: self.pipeline_cache,
                pipeline_layout: self.pipeline_layout,
                fragment_has_color0_output: self.fragment_has_color0_output,
                uses_descriptor_buffer: self.uses_descriptor_buffer,
                stage_infos: Arc::clone(&self.stage_infos),
                shader_modules: self.shader_modules,
                num_image_elements: self.num_image_elements,
            },
            &self.pipeline,
            render_pass,
        )
    }

    fn make_pipeline_from_snapshot(
        build: &GraphicsPipelineBuildSnapshot,
        pipeline_state: &Arc<Mutex<vk::Pipeline>>,
        render_pass: vk::RenderPass,
    ) -> Option<vk::Pipeline> {
        let device = build.device_owner.get();
        let fixed_state = &build.key.fixed_state;
        let dynamic = if fixed_state.extended_dynamic_state() {
            DynamicState {
                raw1: fixed_state.dynamic_state.raw1,
                raw2: 0,
            }
        } else {
            fixed_state.dynamic_state
        };
        let entry_name = std::ffi::CString::new("main").unwrap();
        let shader_stages = shader_stage_create_infos(&build.shader_modules, &entry_name);
        let vertex_info = &build.stage_infos[0];
        let (vertex_bindings, vertex_divisors, vertex_attributes) =
            build_vertex_input_state_from_state(
                fixed_state,
                vertex_info,
                device,
                device.get_max_vertex_input_bindings(),
            );
        assert!(vertex_attributes.len() <= device.get_max_vertex_input_attributes() as usize);
        let mut vertex_divisor_state = vk::PipelineVertexInputDivisorStateCreateInfoEXT::builder()
            .vertex_binding_divisors(&vertex_divisors);
        let mut vertex_input_builder = vk::PipelineVertexInputStateCreateInfo::builder()
            .vertex_binding_descriptions(&vertex_bindings)
            .vertex_attribute_descriptions(&vertex_attributes);
        if !vertex_divisors.is_empty() {
            vertex_input_builder = vertex_input_builder.push_next(&mut vertex_divisor_state);
        }
        let vertex_input = vertex_input_builder.build();

        let input_assembly_topology =
            input_assembly_topology_for_state(fixed_state, &build.shader_modules);
        let primitive_restart_enable = device.is_molten_vk()
            || primitive_restart_enable_for_pipeline(
                dynamic.primitive_restart_enable(),
                input_assembly_topology,
                device.is_topology_list_primitive_restart_supported(),
                device.is_patch_list_primitive_restart_supported(),
            );
        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::builder()
            .topology(input_assembly_topology)
            .primitive_restart_enable(primitive_restart_enable)
            .build();
        let tessellation = vk::PipelineTessellationStateCreateInfo::builder()
            .patch_control_points(patch_control_points_for_state(fixed_state))
            .build();

        let swizzles = fixed_state.viewport_swizzles.map(unpack_viewport_swizzle);
        let mut swizzle_state =
            vk::PipelineViewportSwizzleStateCreateInfoNV::builder().viewport_swizzles(&swizzles);
        let mut depth_clip_control = vk::PipelineViewportDepthClipControlCreateInfoEXT::builder()
            .negative_one_to_one(fixed_state.ndc_minus_one_to_one());
        let num_viewports = device
            .get_max_viewports()
            .min(crate::engines::maxwell_3d::NUM_VIEWPORTS as u32);
        let mut viewport_state_builder = vk::PipelineViewportStateCreateInfo::builder()
            .viewport_count(num_viewports)
            .scissor_count(num_viewports);
        if device.is_nv_viewport_swizzle_supported() {
            viewport_state_builder = viewport_state_builder.push_next(&mut swizzle_state);
        }
        if device.is_ext_depth_clip_control_supported() {
            viewport_state_builder = viewport_state_builder.push_next(&mut depth_clip_control);
        }
        let viewport_state = viewport_state_builder.build();

        let smooth_lines_supported =
            device.is_ext_line_rasterization_supported() && device.supports_smooth_lines();
        let stippled_lines_supported = device.is_ext_line_rasterization_supported()
            && device.supports_stippled_rectangular_lines();
        let mut line_state = vk::PipelineRasterizationLineStateCreateInfoEXT::builder()
            .line_rasterization_mode(if fixed_state.smooth_lines() && smooth_lines_supported {
                vk::LineRasterizationModeEXT::RECTANGULAR_SMOOTH
            } else {
                vk::LineRasterizationModeEXT::RECTANGULAR
            })
            .stippled_line_enable(dynamic.line_stipple_enable() && stippled_lines_supported)
            .line_stipple_factor(fixed_state.line_stipple_factor)
            .line_stipple_pattern(fixed_state.line_stipple_pattern as u16);
        let mut conservative_state =
            vk::PipelineRasterizationConservativeStateCreateInfoEXT::builder()
                .conservative_rasterization_mode(if fixed_state.conservative_raster_enable() {
                    vk::ConservativeRasterizationModeEXT::OVERESTIMATE
                } else {
                    vk::ConservativeRasterizationModeEXT::DISABLED
                })
                .extra_primitive_overestimation_size(0.0);
        let requested_last = fixed_state.provoking_vertex_last();
        let provoking_mode = if requested_last {
            if device.supports_provoking_vertex_last_mode() {
                vk::ProvokingVertexModeEXT::LAST_VERTEX
            } else {
                vk::ProvokingVertexModeEXT::FIRST_VERTEX
            }
        } else if device.supports_provoking_vertex_first_mode() {
            vk::ProvokingVertexModeEXT::FIRST_VERTEX
        } else {
            vk::ProvokingVertexModeEXT::LAST_VERTEX
        };
        let mut provoking_state =
            vk::PipelineRasterizationProvokingVertexStateCreateInfoEXT::builder()
                .provoking_vertex_mode(provoking_mode);
        let mut rasterization_builder = vk::PipelineRasterizationStateCreateInfo::builder()
            .depth_clamp_enable(!dynamic.depth_clamp_disabled())
            .rasterizer_discard_enable(!dynamic.rasterize_enable())
            .polygon_mode(maxwell_to_vk::polygon_mode(fixed_state.polygon_mode()))
            .cull_mode(if dynamic.cull_enable() {
                map_cull_face(dynamic.cull_face())
            } else {
                vk::CullModeFlags::NONE
            })
            .front_face(maxwell_to_vk::front_face(dynamic.front_face()))
            .depth_bias_enable(dynamic.depth_bias_enable())
            .line_width(1.0);
        if is_line(input_assembly_topology) && device.is_ext_line_rasterization_supported() {
            rasterization_builder = rasterization_builder.push_next(&mut line_state);
        }
        if device.is_ext_conservative_rasterization_supported() {
            rasterization_builder = rasterization_builder.push_next(&mut conservative_state);
        }
        if device.is_ext_provoking_vertex_supported() {
            rasterization_builder = rasterization_builder.push_next(&mut provoking_state);
        }
        let rasterization = rasterization_builder.build();

        let sample_shading = *common::settings::values().sample_shading.get_value();
        let supports_alpha_output = build.fragment_has_color0_output;
        let multisample = vk::PipelineMultisampleStateCreateInfo::builder()
            .rasterization_samples(maxwell_to_vk::msaa_mode(
                MsaaMode::from_raw(fixed_state.msaa_mode_raw()).unwrap_or_else(|| {
                    debug_assert!(false, "Invalid msaa_mode={}", fixed_state.msaa_mode_raw());
                    MsaaMode::Msaa1x1
                }),
            ))
            .sample_shading_enable(sample_shading > 0)
            .min_sample_shading(sample_shading as f32 / 100.0)
            .alpha_to_coverage_enable(
                supports_alpha_output && fixed_state.alpha_to_coverage_enabled(),
            )
            .alpha_to_one_enable(
                supports_alpha_output
                    && device.supports_alpha_to_one()
                    && fixed_state.alpha_to_one_enabled(),
            )
            .build();

        let depth_bounds_enabled =
            dynamic.depth_bounds_enable() && device.is_depth_bounds_supported();
        if dynamic.depth_bounds_enable() && !device.is_depth_bounds_supported() {
            warn!("Depth bounds is enabled but not supported");
        }
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::builder()
            .depth_test_enable(dynamic.depth_test_enable())
            .depth_write_enable(dynamic.depth_write_enable())
            .depth_compare_op(if dynamic.depth_test_enable() {
                maxwell_to_vk::comparison_op(dynamic.depth_test_func())
            } else {
                vk::CompareOp::ALWAYS
            })
            .depth_bounds_test_enable(depth_bounds_enabled)
            .stencil_test_enable(dynamic.stencil_enable())
            .front(stencil_face_state(dynamic.front_stencil(), dynamic.raw2))
            .back(stencil_face_state(dynamic.back_stencil(), dynamic.raw2))
            .min_depth_bounds(fixed_state.depth_bounds_min as f32)
            .max_depth_bounds(fixed_state.depth_bounds_max as f32)
            .build();

        let blend_attachments = (0..num_attachments(fixed_state))
            .map(|index| {
                let attachment = fixed_state.attachments[index];
                let mask = attachment.mask();
                let mut write_mask = vk::ColorComponentFlags::empty();
                if mask[0] {
                    write_mask |= vk::ColorComponentFlags::R;
                }
                if mask[1] {
                    write_mask |= vk::ColorComponentFlags::G;
                }
                if mask[2] {
                    write_mask |= vk::ColorComponentFlags::B;
                }
                if mask[3] {
                    write_mask |= vk::ColorComponentFlags::A;
                }
                vk::PipelineColorBlendAttachmentState::builder()
                    .blend_enable(attachment.is_enabled())
                    .src_color_blend_factor(maxwell_to_vk::blend_factor(
                        attachment.source_rgb_factor(),
                    ))
                    .dst_color_blend_factor(maxwell_to_vk::blend_factor(
                        attachment.dest_rgb_factor(),
                    ))
                    .color_blend_op(maxwell_to_vk::blend_equation(attachment.equation_rgb()))
                    .src_alpha_blend_factor(maxwell_to_vk::blend_factor(
                        attachment.source_alpha_factor(),
                    ))
                    .dst_alpha_blend_factor(maxwell_to_vk::blend_factor(
                        attachment.dest_alpha_factor(),
                    ))
                    .alpha_blend_op(maxwell_to_vk::blend_equation(attachment.equation_alpha()))
                    .color_write_mask(write_mask)
                    .build()
            })
            .collect::<Vec<_>>();
        let color_blend = vk::PipelineColorBlendStateCreateInfo::builder()
            .logic_op_enable(dynamic.logic_op_enable())
            .logic_op(vk::LogicOp::from_raw(
                pack_logic_op(dynamic.logic_op()) as i32
            ))
            .attachments(&blend_attachments)
            .build();

        let dynamic_support = DynamicState3Support {
            depth_clamp_enable: device.supports_dynamic_state3_depth_clamp_enable(),
            logic_op_enable: device.supports_dynamic_state3_logic_op_enable(),
            line_rasterization_mode: device.supports_dynamic_state3_line_rasterization_mode(),
            conservative_rasterization_mode: device
                .supports_dynamic_state3_conservative_rasterization_mode(),
            line_stipple_enable: device.supports_dynamic_state3_line_stipple_enable(),
            alpha_to_coverage_enable: device.supports_dynamic_state3_alpha_to_coverage_enable(),
            alpha_to_one_enable: device.supports_dynamic_state3_alpha_to_one_enable(),
        };
        let dynamic_states = dynamic_states_for_fixed_state(fixed_state, dynamic_support);
        let dynamic_state = vk::PipelineDynamicStateCreateInfo::builder()
            .dynamic_states(&dynamic_states)
            .build();

        let mut flags = vk::PipelineCreateFlags::empty();
        if device.is_khr_pipeline_executable_properties_enabled()
            && *common::settings::values().renderer_debug.get_value()
        {
            flags |= vk::PipelineCreateFlags::CAPTURE_STATISTICS_KHR;
        }
        if build.uses_descriptor_buffer {
            flags |= vk::PipelineCreateFlags::DESCRIPTOR_BUFFER_EXT;
        }
        let pipeline_info = vk::GraphicsPipelineCreateInfo::builder()
            .flags(flags)
            .stages(&shader_stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .tessellation_state(&tessellation)
            .viewport_state(&viewport_state)
            .rasterization_state(&rasterization)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic_state)
            .layout(build.pipeline_layout)
            .render_pass(render_pass)
            .subpass(0)
            .build();

        match unsafe {
            build
                .device_owner
                .get()
                .get_logical()
                .create_graphics_pipelines(build.pipeline_cache, &[pipeline_info], None)
        } {
            Ok(pipelines) => {
                let created = pipelines[0];
                *pipeline_state.lock().unwrap() = created;
                if is_active() {
                    let pipeline_info = graphics_pipeline_creation_log_info(
                        shader_stages.len(),
                        blend_attachments.len(),
                    );
                    get_instance().log_pipeline_state_change(&pipeline_info);
                }
                Some(created)
            }
            Err((_, error)) => {
                warn!("GraphicsPipeline: pipeline creation failed: {error:?}");
                None
            }
        }
    }

    fn finish_make_pipeline(
        &self,
        render_pass_cache: &RenderPassCache,
        pipeline_statistics: Option<&PipelineStatistics>,
    ) -> bool {
        let render_pass_key = make_render_pass_key(&self.key.fixed_state, self.device_owner.get());
        let Ok(render_pass) = render_pass_cache.get(&render_pass_key) else {
            return false;
        };
        self.validate();
        let Some(created) = self.make_pipeline(render_pass) else {
            return false;
        };
        if let Some(statistics) = pipeline_statistics {
            statistics.collect(self.device_owner.get(), created);
        }
        true
    }

    pub(crate) fn finish_build_sync(
        &self,
        render_pass_cache: &RenderPassCache,
        pipeline_statistics: Option<&PipelineStatistics>,
    ) {
        let _ = self.finish_make_pipeline(render_pass_cache, pipeline_statistics);
        self.is_built.store(true, Ordering::Release);
        self.build_condvar.notify_one();
    }

    pub(crate) fn queue_make_pipeline(
        &self,
        worker: &ThreadWorker,
        runtime: GraphicsPipelineRuntime,
        shader_notify: ShaderNotifyHandle,
        pipeline_statistics: Option<Arc<PipelineStatistics>>,
    ) {
        // The worker owns an immutable build snapshot. This is the Rust
        // counterpart of Eden's constructor lambda capturing `this`; Vulkan
        // handles and build synchronization remain shared with this object.
        let build = GraphicsPipelineBuildSnapshot {
            device_owner: self.device_owner,
            key: self.key.clone(),
            pipeline_cache: self.pipeline_cache,
            pipeline_layout: self.pipeline_layout,
            fragment_has_color0_output: self.fragment_has_color0_output,
            uses_descriptor_buffer: self.uses_descriptor_buffer,
            stage_infos: Arc::clone(&self.stage_infos),
            shader_modules: self.shader_modules,
            num_image_elements: self.num_image_elements,
        };
        let pipeline = Arc::clone(&self.pipeline);
        let build_condvar = Arc::clone(&self.build_condvar);
        let build_mutex = Arc::clone(&self.build_mutex);
        let is_built = Arc::clone(&self.is_built);
        worker.queue_stateless_work(move || {
            let render_pass_key =
                make_render_pass_key(&build.key.fixed_state, build.device_owner.get());
            let created = unsafe { runtime.render_pass_cache() }
                .get(&render_pass_key)
                .ok()
                .and_then(|render_pass| {
                    GraphicsPipeline::validate_stage_infos(
                        &build.stage_infos,
                        build.num_image_elements,
                    );
                    GraphicsPipeline::make_pipeline_from_snapshot(&build, &pipeline, render_pass)
                });
            if let Some(created) = created {
                if let Some(statistics) = &pipeline_statistics {
                    statistics.collect(build.device_owner.get(), created);
                }
            }
            {
                let _lock = build_mutex.lock().unwrap();
                is_built.store(true, Ordering::Release);
            }
            build_condvar.notify_one();
            shader_notify.mark_shader_complete();
        });
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

fn buffer_cache_metadata(
    stage_infos: &[ShaderInfo; 5],
) -> ([u32; NUM_STAGES as usize], UniformBufferSizes) {
    let mut masks = [0u32; NUM_STAGES as usize];
    let mut sizes = [[0u32; NUM_GRAPHICS_UNIFORM_BUFFERS as usize]; NUM_STAGES as usize];
    for stage in 0..NUM_STAGES as usize {
        let info = &stage_infos[stage];
        masks[stage] = info.constant_buffer_mask;
        sizes[stage].copy_from_slice(&info.constant_buffer_used_sizes);
    }
    (masks, sizes)
}

/// Constructor accounting performed by upstream
/// `GraphicsPipeline::GraphicsPipeline` while copying `stage_infos`.
fn graphics_resource_metadata(stage_infos: &[ShaderInfo; 5]) -> (usize, u32, bool) {
    let mut num_image_elements = 0usize;
    let mut num_textures = 0u32;
    for info in stage_infos {
        num_image_elements += num_descriptors(&info.texture_buffer_descriptors) as usize;
        num_image_elements += num_descriptors(&info.image_buffer_descriptors) as usize;
        let stage_textures = num_descriptors(&info.texture_descriptors);
        num_textures += stage_textures;
        num_image_elements += stage_textures as usize;
        num_image_elements += num_descriptors(&info.image_descriptors) as usize;
    }
    let fragment_has_color0_output = stage_infos[NUM_VK_GRAPHICS_STAGES - 1].stores_frag_color[0];
    (num_image_elements, num_textures, fragment_has_color0_output)
}

fn graphics_stage_flags(stage_index: usize) -> vk::ShaderStageFlags {
    match stage_index {
        0 => vk::ShaderStageFlags::VERTEX,
        1 => vk::ShaderStageFlags::TESSELLATION_CONTROL,
        2 => vk::ShaderStageFlags::TESSELLATION_EVALUATION,
        3 => vk::ShaderStageFlags::GEOMETRY,
        4 => vk::ShaderStageFlags::FRAGMENT,
        _ => vk::ShaderStageFlags::empty(),
    }
}

fn shader_stage_create_infos(
    shader_modules: &[vk::ShaderModule; NUM_VK_GRAPHICS_STAGES],
    entry_name: &std::ffi::CStr,
) -> Vec<vk::PipelineShaderStageCreateInfo> {
    shader_modules
        .iter()
        .enumerate()
        .filter_map(|(stage_index, &module)| {
            if module == vk::ShaderModule::null() {
                return None;
            }
            Some(
                vk::PipelineShaderStageCreateInfo::builder()
                    .stage(graphics_stage_flags(stage_index))
                    .module(module)
                    .name(entry_name)
                    .build(),
            )
        })
        .collect()
}

/// Port of `SupportsPrimitiveRestart` in upstream `vk_graphics_pipeline.cpp`.
fn supports_primitive_restart(topology: vk::PrimitiveTopology) -> bool {
    !matches!(
        topology,
        vk::PrimitiveTopology::POINT_LIST
            | vk::PrimitiveTopology::LINE_LIST
            | vk::PrimitiveTopology::TRIANGLE_LIST
            | vk::PrimitiveTopology::LINE_LIST_WITH_ADJACENCY
            | vk::PrimitiveTopology::TRIANGLE_LIST_WITH_ADJACENCY
            | vk::PrimitiveTopology::PATCH_LIST
    )
}

/// Port of anonymous-namespace `IsLine`.
fn is_line(topology: vk::PrimitiveTopology) -> bool {
    matches!(
        topology,
        vk::PrimitiveTopology::LINE_LIST | vk::PrimitiveTopology::LINE_STRIP
    )
}

/// Port of anonymous-namespace `UnpackViewportSwizzle`.
fn unpack_viewport_swizzle(swizzle: u16) -> vk::ViewportSwizzleNV {
    vk::ViewportSwizzleNV {
        x: maxwell_to_vk::viewport_swizzle(u32::from(swizzle) & 0x7),
        y: maxwell_to_vk::viewport_swizzle((u32::from(swizzle) >> 4) & 0x7),
        z: maxwell_to_vk::viewport_swizzle((u32::from(swizzle) >> 8) & 0x7),
        w: maxwell_to_vk::viewport_swizzle((u32::from(swizzle) >> 12) & 0x7),
    }
}

/// Port of upstream `NumAttachments`.
fn num_attachments(state: &FixedPipelineState) -> usize {
    state
        .color_formats
        .iter()
        .rposition(|&format| format != 0)
        .map_or(0, |index| index + 1)
}

fn primitive_restart_supported_for_topology(
    topology: vk::PrimitiveTopology,
    topology_list_primitive_restart_supported: bool,
    patch_list_primitive_restart_supported: bool,
) -> bool {
    (topology != vk::PrimitiveTopology::PATCH_LIST && topology_list_primitive_restart_supported)
        || supports_primitive_restart(topology)
        || (topology == vk::PrimitiveTopology::PATCH_LIST && patch_list_primitive_restart_supported)
}

/// Port of `input_assembly_ci.primitiveRestartEnable` selection in
/// `vk_graphics_pipeline.cpp`. Upstream does not force this to false when
/// `VK_EXT_extended_dynamic_state2` is active; the dynamic state is added
/// separately to the pipeline dynamic-state list.
fn primitive_restart_enable_for_pipeline(
    dynamic_primitive_restart_enable: bool,
    topology: vk::PrimitiveTopology,
    topology_list_primitive_restart_supported: bool,
    patch_list_primitive_restart_supported: bool,
) -> bool {
    dynamic_primitive_restart_enable
        && primitive_restart_supported_for_topology(
            topology,
            topology_list_primitive_restart_supported,
            patch_list_primitive_restart_supported,
        )
}

/// Port of upstream `GraphicsPipeline::MakePipeline` tessellation/topology
/// compatibility fixup before creating `VkPipelineInputAssemblyStateCreateInfo`.
fn input_assembly_topology_for_state(
    fixed_state: &FixedPipelineState,
    shader_modules: &[vk::ShaderModule; NUM_VK_GRAPHICS_STAGES],
) -> vk::PrimitiveTopology {
    let has_tess_stages = shader_modules[1] != vk::ShaderModule::null()
        || shader_modules[2] != vk::ShaderModule::null();
    let mut topology = maxwell_to_vk::primitive_topology(fixed_state.topology());
    if topology == vk::PrimitiveTopology::PATCH_LIST {
        if !has_tess_stages {
            topology = vk::PrimitiveTopology::POINT_LIST;
        }
    } else if has_tess_stages {
        topology = vk::PrimitiveTopology::PATCH_LIST;
    }
    topology
}

/// Port of upstream `GraphicsPipeline::MakePipeline`
/// `VkPipelineTessellationStateCreateInfo::patchControlPoints`.
fn patch_control_points_for_state(fixed_state: &FixedPipelineState) -> u32 {
    fixed_state.patch_control_points()
}

fn stencil_face_state(
    face: &super::fixed_pipeline_state::StencilFace,
    raw: u32,
) -> vk::StencilOpState {
    vk::StencilOpState {
        fail_op: maxwell_to_vk::stencil_op(face.action_stencil_fail(raw)),
        pass_op: maxwell_to_vk::stencil_op(face.action_depth_pass(raw)),
        depth_fail_op: maxwell_to_vk::stencil_op(face.action_depth_fail(raw)),
        compare_op: maxwell_to_vk::comparison_op(face.test_func(raw)),
        compare_mask: 0,
        write_mask: 0,
        reference: 0,
    }
}

struct GraphicsDescriptorLayout {
    pipeline_layout: vk::PipelineLayout,
    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_update_template: vk::DescriptorUpdateTemplate,
    uses_push_descriptor: bool,
    uses_descriptor_buffer: bool,
    descriptor_buffer_layout: DescriptorBufferLayout,
    descriptor_allocator: Option<DescriptorAllocator>,
}

fn graphics_pipeline_key_cache_hash(key: &GraphicsPipelineKey) -> u64 {
    key.hash_value()
}

fn map_cull_face(face: CullFace) -> vk::CullModeFlags {
    match face {
        CullFace::Front => vk::CullModeFlags::FRONT,
        CullFace::Back => vk::CullModeFlags::BACK,
        CullFace::FrontAndBack => vk::CullModeFlags::FRONT_AND_BACK,
    }
}

fn build_vertex_input_state_from_state_with_format(
    fixed_state: &FixedPipelineState,
    vertex_info: &ShaderInfo,
    max_vertex_input_bindings: u32,
    mut resolve_vertex_format: impl FnMut(VertexAttribType, VertexAttribSize) -> vk::Format,
) -> (
    Vec<vk::VertexInputBindingDescription>,
    Vec<vk::VertexInputBindingDivisorDescriptionEXT>,
    Vec<vk::VertexInputAttributeDescription>,
) {
    if fixed_state.dynamic_vertex_input() {
        return (Vec::new(), Vec::new(), Vec::new());
    }

    let num_vertex_bindings = fixed_state
        .vertex_strides
        .len()
        .min(max_vertex_input_bindings as usize);
    let mut bindings = Vec::with_capacity(num_vertex_bindings);
    let mut divisors = Vec::new();
    for (index, &stride) in fixed_state
        .vertex_strides
        .iter()
        .take(num_vertex_bindings)
        .enumerate()
    {
        let divisor = fixed_state.binding_divisors[index];
        let input_rate = if divisor != 0 {
            vk::VertexInputRate::INSTANCE
        } else {
            vk::VertexInputRate::VERTEX
        };
        let binding = index as u32;
        bindings.push(vk::VertexInputBindingDescription {
            binding,
            stride: stride as u32,
            input_rate,
        });
        if divisor != 0 {
            divisors.push(vk::VertexInputBindingDivisorDescriptionEXT { binding, divisor });
        }
    }
    let mut attributes = Vec::new();
    for (location, attrib) in fixed_state.attributes.iter().enumerate() {
        let attrib_type = VertexAttribType::from_raw(attrib.attrib_type());
        let attrib_size = VertexAttribSize::from_raw(attrib.attrib_size());
        if !attrib.is_enabled() || !vertex_info.loads.generic_any(location) {
            continue;
        }
        let format = resolve_vertex_format(attrib_type, attrib_size);
        attributes.push(vk::VertexInputAttributeDescription {
            location: location as u32,
            binding: attrib.buffer(),
            format,
            offset: attrib.offset(),
        });
    }

    (bindings, divisors, attributes)
}

fn build_vertex_input_state_from_state(
    fixed_state: &FixedPipelineState,
    vertex_info: &ShaderInfo,
    device: &Device,
    max_vertex_input_bindings: u32,
) -> (
    Vec<vk::VertexInputBindingDescription>,
    Vec<vk::VertexInputBindingDivisorDescriptionEXT>,
    Vec<vk::VertexInputAttributeDescription>,
) {
    build_vertex_input_state_from_state_with_format(
        fixed_state,
        vertex_info,
        max_vertex_input_bindings,
        |attrib_type, attrib_size| maxwell_to_vk::vertex_format(device, attrib_type, attrib_size),
    )
}

fn dynamic_states_for_fixed_state(
    fixed_state: &FixedPipelineState,
    support: DynamicState3Support,
) -> Vec<vk::DynamicState> {
    let mut dynamic_states = vec![
        vk::DynamicState::VIEWPORT,
        vk::DynamicState::SCISSOR,
        vk::DynamicState::DEPTH_BIAS,
        vk::DynamicState::BLEND_CONSTANTS,
        vk::DynamicState::DEPTH_BOUNDS,
        vk::DynamicState::STENCIL_COMPARE_MASK,
        vk::DynamicState::STENCIL_WRITE_MASK,
        vk::DynamicState::STENCIL_REFERENCE,
        vk::DynamicState::LINE_WIDTH,
    ];
    if fixed_state.extended_dynamic_state() {
        dynamic_states.extend([
            vk::DynamicState::CULL_MODE,
            vk::DynamicState::FRONT_FACE,
            vk::DynamicState::DEPTH_TEST_ENABLE,
            vk::DynamicState::DEPTH_WRITE_ENABLE,
            vk::DynamicState::DEPTH_COMPARE_OP,
            vk::DynamicState::DEPTH_BOUNDS_TEST_ENABLE,
            vk::DynamicState::STENCIL_TEST_ENABLE,
            vk::DynamicState::STENCIL_OP,
            vk::DynamicState::PRIMITIVE_TOPOLOGY,
        ]);
        if !fixed_state.dynamic_vertex_input() {
            dynamic_states.push(vk::DynamicState::VERTEX_INPUT_BINDING_STRIDE);
        }
    }
    if fixed_state.dynamic_vertex_input() {
        dynamic_states.push(vk::DynamicState::VERTEX_INPUT_EXT);
    }
    if fixed_state.extended_dynamic_state_2() {
        dynamic_states.extend([
            vk::DynamicState::DEPTH_BIAS_ENABLE,
            vk::DynamicState::PRIMITIVE_RESTART_ENABLE,
            vk::DynamicState::RASTERIZER_DISCARD_ENABLE,
        ]);
    }
    if fixed_state.extended_dynamic_state_2_logic_op() {
        dynamic_states.push(vk::DynamicState::LOGIC_OP_EXT);
    }
    if fixed_state.extended_dynamic_state_3_blend() {
        dynamic_states.extend([
            vk::DynamicState::COLOR_BLEND_ENABLE_EXT,
            vk::DynamicState::COLOR_BLEND_EQUATION_EXT,
            vk::DynamicState::COLOR_WRITE_MASK_EXT,
        ]);
    }
    if !fixed_state.extended_dynamic_state_3_blend() && fixed_state.color_write_enable_dynamic() {
        dynamic_states.push(vk::DynamicState::COLOR_WRITE_ENABLE_EXT);
    }
    if fixed_state.extended_dynamic_state_3_enables() {
        if support.depth_clamp_enable {
            dynamic_states.push(vk::DynamicState::DEPTH_CLAMP_ENABLE_EXT);
        }
        if support.logic_op_enable {
            dynamic_states.push(vk::DynamicState::LOGIC_OP_ENABLE_EXT);
        }
        if support.line_rasterization_mode {
            dynamic_states.push(vk::DynamicState::LINE_RASTERIZATION_MODE_EXT);
        }
        if support.conservative_rasterization_mode {
            dynamic_states.push(vk::DynamicState::CONSERVATIVE_RASTERIZATION_MODE_EXT);
        }
        if support.line_stipple_enable {
            dynamic_states.push(vk::DynamicState::LINE_STIPPLE_ENABLE_EXT);
        }
        if support.alpha_to_coverage_enable {
            dynamic_states.push(vk::DynamicState::ALPHA_TO_COVERAGE_ENABLE_EXT);
        }
        if support.alpha_to_one_enable {
            dynamic_states.push(vk::DynamicState::ALPHA_TO_ONE_ENABLE_EXT);
        }
    }
    dynamic_states
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::maxwell_3d::PrimitiveTopology;
    use ash::vk::Handle;
    use std::mem::ManuallyDrop;

    #[test]
    fn graphics_gpu_memory_uses_upstream_non_owning_pointer() {
        let memory = std::sync::Arc::new(parking_lot::Mutex::new(
            crate::memory_manager::MemoryManager::new(0),
        ));
        let guard = memory.lock();
        let access = GraphicsGpuMemory::Memory(MemoryManagerHandle::from_ref(&guard));
        let mut output = [0u8; 4];

        // Keep the Rust ownership mutex locked deliberately. Upstream reads
        // through the already-bound `Tegra::MemoryManager*`; this must not try
        // to acquire the same non-reentrant mutex for every descriptor word.
        assert!(access.read_unsafe(0x1000, &mut output));
        assert_eq!(output, [0; 4]);
    }

    #[test]
    fn graphics_descriptor_work_lists_keep_upstream_inline_capacity() {
        let mut views = GraphicsImageViews::new();
        views.resize(64, ImageViewInOut::default());
        assert!(!views.spilled());

        let mut samplers = GraphicsSamplers::new();
        samplers.resize(64, Default::default());
        assert!(!samplers.spilled());
    }

    #[test]
    fn configure_func_selects_upstream_specializations_in_order() {
        use shader_recompiler::shader_info::TextureType;
        use shader_recompiler::shader_info::{
            ImageDescriptor, ImageFormat, StorageBufferDescriptor, TextureBufferDescriptor,
        };

        let module = vk::ShaderModule::from_raw(1);
        let mut modules = [vk::ShaderModule::null(); NUM_VK_GRAPHICS_STAGES];
        let mut infos: [ShaderInfo; NUM_VK_GRAPHICS_STAGES] = Default::default();
        modules[0] = module;
        assert_eq!(
            configure_func(&modules, &infos) as usize,
            configure_spec::<SimpleVertexSpec> as ConfigureFunc as usize
        );

        modules[4] = module;
        assert_eq!(
            configure_func(&modules, &infos) as usize,
            configure_spec::<SimpleVertexFragmentSpec> as ConfigureFunc as usize
        );

        infos[0]
            .storage_buffers_descriptors
            .push(StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                is_written: false,
            });
        assert_eq!(
            configure_func(&modules, &infos) as usize,
            configure_spec::<SimpleStorageSpec> as ConfigureFunc as usize
        );

        infos[0].storage_buffers_descriptors.clear();
        infos[0].image_descriptors.push(ImageDescriptor {
            texture_type: TextureType::Color2D,
            format: ImageFormat::Typeless,
            is_written: false,
            is_read: true,
            is_integer: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            count: 1,
            size_shift: 0,
        });
        assert_eq!(
            configure_func(&modules, &infos) as usize,
            configure_spec::<SimpleImageSpec> as ConfigureFunc as usize
        );

        infos[0].image_descriptors.clear();
        infos[0]
            .texture_buffer_descriptors
            .push(TextureBufferDescriptor {
                has_secondary: false,
                cbuf_index: 0,
                cbuf_offset: 0,
                shift_left: 0,
                secondary_cbuf_index: 0,
                secondary_cbuf_offset: 0,
                secondary_shift_left: 0,
                count: 1,
                size_shift: 0,
            });
        assert_eq!(
            configure_func(&modules, &infos) as usize,
            configure_spec::<DefaultSpec> as ConfigureFunc as usize
        );

        infos[0].texture_buffer_descriptors.clear();
        modules[1] = module;
        assert_eq!(
            configure_func(&modules, &infos) as usize,
            configure_spec::<DefaultSpec> as ConfigureFunc as usize
        );
    }

    fn make_test_pipeline(key: GraphicsPipelineKey) -> ManuallyDrop<GraphicsPipeline> {
        // Test-only placeholder. These methods do not touch the Vulkan device,
        // and `ManuallyDrop` avoids running the real destruction path.
        ManuallyDrop::new(GraphicsPipeline {
            device_owner: DeviceReference::dangling_for_test(),
            key,
            pipeline_cache: vk::PipelineCache::null(),
            transitions: RefCell::new(Vec::new()),
            pipeline: Arc::new(Mutex::new(vk::Pipeline::null())),
            pipeline_layout: vk::PipelineLayout::null(),
            descriptor_set_layout: vk::DescriptorSetLayout::null(),
            descriptor_update_template: vk::DescriptorUpdateTemplate::null(),
            uses_push_descriptor: false,
            descriptor_buffer_layout: DescriptorBufferLayout::default(),
            uses_descriptor_buffer: false,
            num_descriptor_entries: 0,
            descriptor_allocator: None,
            scheduler: NonNull::dangling(),
            buffer_cache: NonNull::dangling(),
            texture_cache: NonNull::dangling(),
            guest_descriptor_queue: NonNull::dangling(),
            descriptor_buffer_ring: NonNull::dangling(),
            engine: RefCell::new(None),
            configure_func: configure_spec::<DefaultSpec>,
            descriptor_payload_state: RefCell::new(DescriptorPayloadState::default()),
            num_image_elements: 0,
            num_textures: 0,
            fragment_has_color0_output: false,
            stage_infos: Arc::new(Default::default()),
            enabled_uniform_buffer_masks: [0; NUM_STAGES as usize],
            uniform_buffer_sizes: [[0; NUM_GRAPHICS_UNIFORM_BUFFERS as usize]; NUM_STAGES as usize],
            uses_render_area: false,
            uses_rescaling_uniform: false,
            shader_modules: [vk::ShaderModule::null(); NUM_VK_GRAPHICS_STAGES],
            build_condvar: Arc::new(Condvar::new()),
            build_mutex: Arc::new(Mutex::new(())),
            is_built: Arc::new(AtomicBool::new(true)),
        })
    }

    #[test]
    fn decode_format_matches_upstream_none_and_color_mapping() {
        assert_eq!(
            decode_format(RenderTargetFormat::None as u8),
            PixelFormat::Invalid
        );
        assert_eq!(
            decode_format(RenderTargetFormat::A2B10G10R10Unorm as u8),
            PixelFormat::A2B10G10R10Unorm
        );
    }

    #[test]
    fn graphics_resource_metadata_matches_upstream_constructor_accounting() {
        use shader_recompiler::shader_info::{TextureDescriptor, TextureType};

        let mut fragment = ShaderInfo::default();
        fragment.stores_frag_color[0] = true;
        fragment.texture_descriptors.push(TextureDescriptor {
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
            count: 3,
            size_shift: 0,
        });
        let mut stages: [ShaderInfo; NUM_VK_GRAPHICS_STAGES] = Default::default();
        stages[NUM_VK_GRAPHICS_STAGES - 1] = fragment;

        assert_eq!(graphics_resource_metadata(&stages), (3, 3, true));
    }

    #[test]
    fn descriptor_payload_update_matches_configure_draw_cache_rule() {
        let first = [DescriptorUpdateEntry::default()];
        let mut changed = first;
        changed[0].buffer = vk::DescriptorBufferInfo {
            buffer: vk::Buffer::from_raw(1),
            offset: 2,
            range: 3,
        };

        assert!(!should_update_descriptor_set(false, &first, &first));
        assert!(should_update_descriptor_set(true, &first, &first));
        assert!(should_update_descriptor_set(false, &[], &first));
        assert!(should_update_descriptor_set(false, &first, &changed));
    }

    #[test]
    fn gpu_log_payloads_match_upstream_graphics_pipeline_strings() {
        let key = GraphicsPipelineKey {
            unique_hashes: [0, 123, 0, 0, 0, 456],
            fixed_state: FixedPipelineState::default(),
        };
        assert_eq!(
            graphics_pipeline_bind_log_info(&key),
            format!("hash={:#016x}", key.hash_value())
        );
        assert_eq!(
            graphics_pipeline_creation_log_info(3, 2),
            "GraphicsPipeline created: stages=3, attachments=2"
        );
    }

    #[test]
    fn test_pipeline_key_equality() {
        let key_a = GraphicsPipelineKey {
            unique_hashes: [0, 123, 0, 0, 0, 456],
            fixed_state: FixedPipelineState::default(),
        };
        let key_b = GraphicsPipelineKey {
            unique_hashes: [0, 123, 0, 0, 0, 456],
            fixed_state: FixedPipelineState::default(),
        };
        assert_eq!(key_a, key_b);
    }

    #[test]
    fn test_transition_graph_returns_self_and_added_transition() {
        let key_a = GraphicsPipelineKey {
            unique_hashes: [0, 123, 0, 0, 0, 456],
            fixed_state: FixedPipelineState::default(),
        };
        let key_b = GraphicsPipelineKey {
            unique_hashes: [0, 789, 0, 0, 0, 321],
            fixed_state: FixedPipelineState::default(),
        };

        let pipeline = Rc::new(ManuallyDrop::into_inner(make_test_pipeline(key_a.clone())));
        let transition = Rc::new(ManuallyDrop::into_inner(make_test_pipeline(key_b.clone())));
        pipeline.add_transition(&transition);

        assert!(Rc::ptr_eq(
            &GraphicsPipeline::next(&pipeline, &key_a).unwrap(),
            &pipeline
        ));
        assert!(Rc::ptr_eq(
            &GraphicsPipeline::next(&pipeline, &key_b).unwrap(),
            &transition
        ));
        assert!(pipeline.is_built());

        // Both fixtures intentionally carry a dangling test-only DeviceReference.
        // They may exercise pure cache methods, but must never enter the real
        // Vulkan destruction path.
        std::mem::forget(pipeline);
        std::mem::forget(transition);
    }

    #[test]
    fn primitive_restart_topology_gate_matches_upstream() {
        assert!(primitive_restart_supported_for_topology(
            vk::PrimitiveTopology::TRIANGLE_STRIP,
            false,
            false,
        ));
        assert!(!primitive_restart_supported_for_topology(
            vk::PrimitiveTopology::TRIANGLE_LIST,
            false,
            false,
        ));
        assert!(primitive_restart_supported_for_topology(
            vk::PrimitiveTopology::TRIANGLE_LIST,
            true,
            false,
        ));
        assert!(!primitive_restart_supported_for_topology(
            vk::PrimitiveTopology::PATCH_LIST,
            true,
            false,
        ));
        assert!(primitive_restart_supported_for_topology(
            vk::PrimitiveTopology::PATCH_LIST,
            false,
            true,
        ));
    }

    #[test]
    fn attachment_count_matches_upstream_depth_only_and_sparse_targets() {
        let mut state = FixedPipelineState::default();
        assert_eq!(num_attachments(&state), 0);

        state.color_formats[3] = 1;
        assert_eq!(num_attachments(&state), 4);
    }

    #[test]
    fn disk_state_topology_matches_upstream_tessellation_fixup() {
        let mut state = FixedPipelineState::default();
        let mut modules = [vk::ShaderModule::null(); NUM_VK_GRAPHICS_STAGES];

        state.set_topology(PrimitiveTopology::Patches);
        assert_eq!(
            input_assembly_topology_for_state(&state, &modules),
            vk::PrimitiveTopology::POINT_LIST
        );

        modules[1] = vk::ShaderModule::from_raw(1);
        assert_eq!(
            input_assembly_topology_for_state(&state, &modules),
            vk::PrimitiveTopology::PATCH_LIST
        );

        state.set_topology(PrimitiveTopology::Triangles);
        assert_eq!(
            input_assembly_topology_for_state(&state, &modules),
            vk::PrimitiveTopology::PATCH_LIST
        );
    }

    #[test]
    fn disk_state_tessellation_patch_control_points_match_upstream() {
        let mut state = FixedPipelineState::default();

        state.set_patch_control_points_minus_one(0);
        assert_eq!(patch_control_points_for_state(&state), 1);

        state.set_patch_control_points_minus_one(31);
        assert_eq!(patch_control_points_for_state(&state), 32);
    }

    #[test]
    fn disk_state_stencil_face_uses_packed_dynamic_state() {
        let mut dynamic = super::super::fixed_pipeline_state::DynamicState::default();
        dynamic.set_stencil_face(
            0,
            crate::engines::maxwell_3d::StencilOp::Replace,
            crate::engines::maxwell_3d::StencilOp::Incr,
            crate::engines::maxwell_3d::StencilOp::Decr,
            crate::engines::maxwell_3d::ComparisonOp::Greater,
        );
        let face = stencil_face_state(dynamic.front_stencil(), dynamic.raw2);
        assert_eq!(face.fail_op, vk::StencilOp::REPLACE);
        assert_eq!(face.depth_fail_op, vk::StencilOp::INCREMENT_AND_WRAP);
        assert_eq!(face.pass_op, vk::StencilOp::DECREMENT_AND_WRAP);
        assert_eq!(face.compare_op, vk::CompareOp::GREATER);
        assert_eq!(face.compare_mask, 0);
        assert_eq!(face.write_mask, 0);
        assert_eq!(face.reference, 0);
    }

    #[test]
    fn primitive_restart_pipeline_state_matches_upstream_dynamic_state2_behavior() {
        assert!(primitive_restart_enable_for_pipeline(
            true,
            vk::PrimitiveTopology::TRIANGLE_STRIP,
            false,
            false,
        ));
        assert!(primitive_restart_enable_for_pipeline(
            true,
            vk::PrimitiveTopology::TRIANGLE_LIST,
            true,
            false,
        ));
        assert!(!primitive_restart_enable_for_pipeline(
            false,
            vk::PrimitiveTopology::TRIANGLE_STRIP,
            true,
            true,
        ));
    }

    #[test]
    fn state_vertex_input_keeps_upstream_binding_order_and_divisors() {
        let mut fixed_state = FixedPipelineState::default();
        fixed_state.vertex_strides[0] = 0;
        fixed_state.vertex_strides[1] = 16;
        fixed_state.binding_divisors[0] = 3;
        fixed_state.attributes[0].set_enabled(true);
        fixed_state.attributes[0].set_buffer(1);
        fixed_state.attributes[0].set_offset(4);
        fixed_state.attributes[0].set_type(VertexAttribType::Float as u32);
        fixed_state.attributes[0].set_size(VertexAttribSize::R32G32 as u32);

        let mut info = ShaderInfo::default();
        info.loads.set(32, true);
        let shader = CompiledShader {
            spirv_words: Vec::new(),
            info,
            stage: ShaderStage::VertexB,
        };

        let (bindings, divisors, attributes) = build_vertex_input_state_from_state_with_format(
            &fixed_state,
            &shader.info,
            32,
            |_, _| vk::Format::R32G32_SFLOAT,
        );

        assert_eq!(bindings.len(), 32);
        assert_eq!(bindings[0].binding, 0);
        assert_eq!(bindings[0].stride, 0);
        assert_eq!(bindings[0].input_rate, vk::VertexInputRate::INSTANCE);
        assert_eq!(bindings[1].binding, 1);
        assert_eq!(bindings[1].stride, 16);
        assert_eq!(bindings[1].input_rate, vk::VertexInputRate::VERTEX);
        assert_eq!(divisors.len(), 1);
        assert_eq!(divisors[0].binding, 0);
        assert_eq!(divisors[0].divisor, 3);
        assert_eq!(attributes.len(), 1);
        assert_eq!(attributes[0].location, 0);
        assert_eq!(attributes[0].binding, 1);
        assert_eq!(attributes[0].offset, 4);

        let (limited_bindings, _, _) = build_vertex_input_state_from_state_with_format(
            &fixed_state,
            &shader.info,
            16,
            |_, _| vk::Format::R32G32_SFLOAT,
        );
        assert_eq!(limited_bindings.len(), 16);
        assert_eq!(limited_bindings[15].binding, 15);
    }

    #[test]
    fn state_dynamic_states_follow_upstream_extension_order() {
        let mut fixed_state = FixedPipelineState::default();
        fixed_state.set_extended_dynamic_state(true);
        fixed_state.set_dynamic_vertex_input(true);
        fixed_state.set_extended_dynamic_state_2(true);
        fixed_state.set_extended_dynamic_state_2_logic_op(true);
        fixed_state.set_extended_dynamic_state_3_blend(true);
        fixed_state.set_extended_dynamic_state_3_enables(true);

        let states = dynamic_states_for_fixed_state(
            &fixed_state,
            DynamicState3Support {
                depth_clamp_enable: true,
                logic_op_enable: true,
                line_rasterization_mode: true,
                conservative_rasterization_mode: true,
                line_stipple_enable: true,
                alpha_to_coverage_enable: true,
                alpha_to_one_enable: true,
            },
        );
        let cull_mode_pos = states
            .iter()
            .position(|&state| state == vk::DynamicState::CULL_MODE)
            .unwrap();
        let topology_pos = states
            .iter()
            .position(|&state| state == vk::DynamicState::PRIMITIVE_TOPOLOGY)
            .unwrap();
        let vertex_input_pos = states
            .iter()
            .position(|&state| state == vk::DynamicState::VERTEX_INPUT_EXT)
            .unwrap();

        assert!(cull_mode_pos < topology_pos);
        assert!(topology_pos < vertex_input_pos);
        assert!(!states.contains(&vk::DynamicState::VERTEX_INPUT_BINDING_STRIDE));
        assert!(states.contains(&vk::DynamicState::LOGIC_OP_EXT));
        assert!(states.contains(&vk::DynamicState::COLOR_BLEND_ENABLE_EXT));
        assert!(states.contains(&vk::DynamicState::DEPTH_CLAMP_ENABLE_EXT));
        assert!(states.contains(&vk::DynamicState::LOGIC_OP_ENABLE_EXT));
        assert!(states.contains(&vk::DynamicState::LINE_RASTERIZATION_MODE_EXT));
        assert!(states.contains(&vk::DynamicState::CONSERVATIVE_RASTERIZATION_MODE_EXT));
        assert!(states.contains(&vk::DynamicState::LINE_STIPPLE_ENABLE_EXT));
        assert!(states.contains(&vk::DynamicState::ALPHA_TO_COVERAGE_ENABLE_EXT));
        assert!(states.contains(&vk::DynamicState::ALPHA_TO_ONE_ENABLE_EXT));
    }

    #[test]
    fn extended_dynamic_state_declares_stride_for_static_vertex_input() {
        let mut fixed_state = FixedPipelineState::default();
        fixed_state.set_extended_dynamic_state(true);
        fixed_state.set_dynamic_vertex_input(false);

        // `FixedPipelineState::refresh` deliberately leaves these zero under
        // EDS1 because vkCmdBindVertexBuffers2 supplies the guest strides.
        assert_eq!(fixed_state.vertex_strides[0], 0);
        let states = dynamic_states_for_fixed_state(&fixed_state, DynamicState3Support::default());
        assert!(states.contains(&vk::DynamicState::VERTEX_INPUT_BINDING_STRIDE));
    }

    #[test]
    fn eds3_only_declares_supported_granular_states_and_color_write_fallback() {
        let mut fixed_state = FixedPipelineState::default();
        fixed_state.set_extended_dynamic_state_3_enables(true);
        fixed_state.set_color_write_enable_dynamic(true);

        let states = dynamic_states_for_fixed_state(&fixed_state, DynamicState3Support::default());

        assert!(states.contains(&vk::DynamicState::COLOR_WRITE_ENABLE_EXT));
        for state in [
            vk::DynamicState::DEPTH_CLAMP_ENABLE_EXT,
            vk::DynamicState::LOGIC_OP_ENABLE_EXT,
            vk::DynamicState::LINE_RASTERIZATION_MODE_EXT,
            vk::DynamicState::CONSERVATIVE_RASTERIZATION_MODE_EXT,
            vk::DynamicState::LINE_STIPPLE_ENABLE_EXT,
            vk::DynamicState::ALPHA_TO_COVERAGE_ENABLE_EXT,
            vk::DynamicState::ALPHA_TO_ONE_ENABLE_EXT,
        ] {
            assert!(!states.contains(&state), "unsupported state {state:?}");
        }
    }
}
