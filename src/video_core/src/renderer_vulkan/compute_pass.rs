// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `vk_compute_pass.h` / `vk_compute_pass.cpp`.
//!
//! Reusable compute passes for index buffer assembly, conditional rendering,
//! prefix scans, and ASTC decoding.

use ash::vk;
use std::ptr::NonNull;

use super::descriptor_pool::{DescriptorAllocator, DescriptorBankInfo, DescriptorPool};
use super::scheduler::Scheduler;
use super::staging_buffer_pool::StagingBufferPool;
use super::update_descriptor::{ComputePassDescriptorQueue, DescriptorUpdateEntry};
use crate::engines::maxwell_3d::IndexFormat;
use crate::host_shaders::spirv_shaders::{
    ASTC_DECODER_COMP_SPV, BLOCK_LINEAR_UNSWIZZLE_3D_BCN_COMP_SPV,
    QUERIES_PREFIX_SCAN_SUM_COMP_SPV, QUERIES_PREFIX_SCAN_SUM_NOSUBGROUPS_COMP_SPV,
    RESOLVE_CONDITIONAL_RENDER_COMP_SPV, VULKAN_QUAD_INDEXED_COMP_SPV, VULKAN_UINT8_COMP_SPV,
};
use crate::texture_cache::accelerated_swizzle::{
    make_block_linear_swizzle_2d_params, make_block_linear_swizzle_3d_params,
};
use crate::texture_cache::image_info::ImageInfo;
use crate::texture_cache::types::SwizzleParameters;

// ---------------------------------------------------------------------------
// Constants (from vk_compute_pass.cpp anonymous namespace)
// ---------------------------------------------------------------------------

const ASTC_BINDING_INPUT_BUFFER: u32 = 0;
const ASTC_BINDING_OUTPUT_IMAGE: u32 = 1;
const ASTC_NUM_BINDINGS: usize = 2;

/// Port of `DISPATCH_SIZE` used in Uint8Pass and QuadIndexedPass.
const DISPATCH_SIZE: u32 = 1024;

/// Port of `DISPATCH_SIZE` used in QueriesPrefixScanPass.
const QUERIES_DISPATCH_SIZE: usize = 2048;

#[derive(Clone, Copy)]
struct DescriptorData(*const DescriptorUpdateEntry);

// The queue owns a fixed-capacity payload for the renderer lifetime. Eden
// likewise captures `DescriptorUpdateEntry*` for consumption by the worker.
unsafe impl Send for DescriptorData {}

impl DescriptorData {
    fn as_raw_data(self) -> *const std::ffi::c_void {
        self.0.cast()
    }
}

/// Port of `AstcPushConstants` from anonymous namespace.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AstcPushConstants {
    pub blocks_dims: [u32; 2],
    pub layer_stride: u32,
    pub block_size: u32,
    pub x_shift: u32,
    pub block_height: u32,
    pub block_height_mask: u32,
}

/// Port of `QueriesPrefixScanPushConstants` from anonymous namespace.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct QueriesPrefixScanPushConstants {
    pub min_accumulation_base: u32,
    pub max_accumulation_base: u32,
    pub accumulation_limit: u32,
    pub buffer_offset: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ConditionalRenderingResolvePushConstants {
    pub compare_to_zero: u32,
}

/// Port of `BlockLinearUnswizzle3DPushConstants`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct BlockLinearUnswizzle3DPushConstants {
    pub blocks_dim: [u32; 3],
    pub bytes_per_block_log2: u32,
    pub origin: [u32; 3],
    pub slice_size: u32,
    pub block_size: u32,
    pub x_shift: u32,
    pub block_height: u32,
    pub block_height_mask: u32,
    pub block_depth: u32,
    pub block_depth_mask: u32,
    pub _pad: i32,
    pub destination: [i32; 3],
    pub _pad_end: i32,
}

/// Memory barrier for shader write -> vertex attribute read.
fn write_barrier_vertex() -> vk::MemoryBarrier {
    vk::MemoryBarrier {
        s_type: vk::StructureType::MEMORY_BARRIER,
        p_next: std::ptr::null(),
        src_access_mask: vk::AccessFlags::SHADER_WRITE,
        dst_access_mask: vk::AccessFlags::VERTEX_ATTRIBUTE_READ,
    }
}

/// Memory barrier for shader write -> index read.
fn write_barrier_index() -> vk::MemoryBarrier {
    vk::MemoryBarrier {
        s_type: vk::StructureType::MEMORY_BARRIER,
        p_next: std::ptr::null(),
        src_access_mask: vk::AccessFlags::SHADER_WRITE,
        dst_access_mask: vk::AccessFlags::INDEX_READ,
    }
}

/// Bank info for input/output storage buffer passes (Uint8, QuadIndexed).
const INPUT_OUTPUT_BANK_INFO: DescriptorBankInfo = DescriptorBankInfo {
    uniform_buffers: 0,
    storage_buffers: 2,
    texture_buffers: 0,
    image_buffers: 0,
    textures: 0,
    images: 0,
    score: 2,
};

fn input_output_bindings() -> [vk::DescriptorSetLayoutBinding; 2] {
    [0, 1].map(|binding| vk::DescriptorSetLayoutBinding {
        binding,
        descriptor_type: vk::DescriptorType::STORAGE_BUFFER,
        descriptor_count: 1,
        stage_flags: vk::ShaderStageFlags::COMPUTE,
        p_immutable_samplers: std::ptr::null(),
    })
}

fn input_output_descriptor_template() -> [vk::DescriptorUpdateTemplateEntry; 1] {
    [vk::DescriptorUpdateTemplateEntry {
        dst_binding: 0,
        dst_array_element: 0,
        descriptor_count: 2,
        descriptor_type: vk::DescriptorType::STORAGE_BUFFER,
        offset: 0,
        stride: std::mem::size_of::<DescriptorUpdateEntry>(),
    }]
}

fn queries_scan_bindings() -> [vk::DescriptorSetLayoutBinding; 3] {
    [0, 1, 2].map(|binding| vk::DescriptorSetLayoutBinding {
        binding,
        descriptor_type: vk::DescriptorType::STORAGE_BUFFER,
        descriptor_count: 1,
        stage_flags: vk::ShaderStageFlags::COMPUTE,
        p_immutable_samplers: std::ptr::null(),
    })
}

fn queries_scan_descriptor_template() -> [vk::DescriptorUpdateTemplateEntry; 1] {
    [vk::DescriptorUpdateTemplateEntry {
        dst_binding: 0,
        dst_array_element: 0,
        descriptor_count: 3,
        descriptor_type: vk::DescriptorType::STORAGE_BUFFER,
        offset: 0,
        stride: std::mem::size_of::<DescriptorUpdateEntry>(),
    }]
}

/// Bank info for queries scan pass (3 storage buffers).
const QUERIES_SCAN_BANK_INFO: DescriptorBankInfo = DescriptorBankInfo {
    uniform_buffers: 0,
    storage_buffers: 3,
    texture_buffers: 0,
    image_buffers: 0,
    textures: 0,
    images: 0,
    score: 3,
};

/// Bank info for ASTC pass (1 storage buffer + 1 storage image).
const ASTC_BANK_INFO: DescriptorBankInfo = DescriptorBankInfo {
    uniform_buffers: 0,
    storage_buffers: 1,
    texture_buffers: 0,
    image_buffers: 0,
    textures: 0,
    images: 1,
    score: 2,
};

// ---------------------------------------------------------------------------
// ComputePass (base)
// ---------------------------------------------------------------------------

/// Port of `ComputePass` base class.
///
/// Owns the shader module, pipeline, pipeline layout, and descriptor
/// allocation for a single reusable compute pass.
pub struct ComputePass {
    device: ash::Device,
    pub descriptor_template: vk::DescriptorUpdateTemplate,
    pub layout: vk::PipelineLayout,
    pub pipeline: vk::Pipeline,
    pub descriptor_set_layout: vk::DescriptorSetLayout,
    pub descriptor_allocator: DescriptorAllocator,
    module: vk::ShaderModule,
}

impl ComputePass {
    /// Port of `ComputePass::ComputePass`.
    ///
    /// Creates the descriptor set layout, pipeline layout, descriptor update
    /// template, shader module, and compute pipeline from the given bindings
    /// and SPIR-V code.
    pub fn new(
        device: &ash::Device,
        descriptor_pool: &DescriptorPool,
        bindings: &[vk::DescriptorSetLayoutBinding],
        templates: &[vk::DescriptorUpdateTemplateEntry],
        bank_info: &DescriptorBankInfo,
        push_constants: &[vk::PushConstantRange],
        code: &[u32],
        _optional_subgroup_size: Option<u32>,
    ) -> Result<Self, vk::Result> {
        // Create descriptor set layout
        let layout_ci = vk::DescriptorSetLayoutCreateInfo::builder()
            .bindings(bindings)
            .build();
        let descriptor_set_layout =
            unsafe { device.create_descriptor_set_layout(&layout_ci, None)? };
        let descriptor_allocator = descriptor_pool.allocator(descriptor_set_layout, bank_info)?;

        // Create pipeline layout
        let set_layouts = [descriptor_set_layout];
        let pipeline_layout_ci = vk::PipelineLayoutCreateInfo::builder()
            .set_layouts(&set_layouts)
            .push_constant_ranges(push_constants)
            .build();
        let layout = unsafe { device.create_pipeline_layout(&pipeline_layout_ci, None)? };

        // Create descriptor update template
        let descriptor_template = if !templates.is_empty() {
            let template_ci = vk::DescriptorUpdateTemplateCreateInfo {
                s_type: vk::StructureType::DESCRIPTOR_UPDATE_TEMPLATE_CREATE_INFO,
                p_next: std::ptr::null(),
                flags: vk::DescriptorUpdateTemplateCreateFlags::empty(),
                descriptor_update_entry_count: templates.len() as u32,
                p_descriptor_update_entries: templates.as_ptr(),
                template_type: vk::DescriptorUpdateTemplateType::DESCRIPTOR_SET,
                descriptor_set_layout,
                pipeline_bind_point: vk::PipelineBindPoint::COMPUTE,
                pipeline_layout: layout,
                set: 0,
            };
            unsafe { device.create_descriptor_update_template(&template_ci, None)? }
        } else {
            vk::DescriptorUpdateTemplate::null()
        };

        // Create shader module and pipeline
        let (module, pipeline) = if !code.is_empty() {
            let module_ci = vk::ShaderModuleCreateInfo::builder().code(code).build();
            let module = unsafe { device.create_shader_module(&module_ci, None)? };

            let main_name = std::ffi::CString::new("main").unwrap();
            let stage_ci = vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::COMPUTE)
                .module(module)
                .name(&main_name)
                .build();

            let pipeline_ci = vk::ComputePipelineCreateInfo::builder()
                .stage(stage_ci)
                .layout(layout)
                .build();

            let pipelines = unsafe {
                device
                    .create_compute_pipelines(vk::PipelineCache::null(), &[pipeline_ci], None)
                    .map_err(|e| e.1)?
            };

            (module, pipelines[0])
        } else {
            (vk::ShaderModule::null(), vk::Pipeline::null())
        };

        Ok(ComputePass {
            device: device.clone(),
            descriptor_template,
            layout,
            pipeline,
            descriptor_set_layout,
            descriptor_allocator,
            module,
        })
    }
}

impl Drop for ComputePass {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_pipeline(self.pipeline, None);
            self.device.destroy_shader_module(self.module, None);
            if self.descriptor_template != vk::DescriptorUpdateTemplate::null() {
                self.device
                    .destroy_descriptor_update_template(self.descriptor_template, None);
            }
            self.device.destroy_pipeline_layout(self.layout, None);
            self.device
                .destroy_descriptor_set_layout(self.descriptor_set_layout, None);
        }
    }
}

// ---------------------------------------------------------------------------
// Uint8Pass
// ---------------------------------------------------------------------------

/// Port of `Uint8Pass` class.
///
/// Assembles uint8 indices into a uint16 index buffer using a compute shader.
pub struct Uint8Pass {
    base: ComputePass,
    scheduler: NonNull<Scheduler>,
    staging_buffer_pool: NonNull<StagingBufferPool>,
    compute_pass_descriptor_queue: NonNull<ComputePassDescriptorQueue>,
}

impl Uint8Pass {
    /// Port of `Uint8Pass::Uint8Pass`.
    pub fn new(
        device: &ash::Device,
        scheduler: &mut Scheduler,
        descriptor_pool: &DescriptorPool,
        staging_buffer_pool: &mut StagingBufferPool,
        compute_pass_descriptor_queue: &mut ComputePassDescriptorQueue,
    ) -> Result<Self, vk::Result> {
        let bindings = input_output_bindings();
        let templates = input_output_descriptor_template();
        let base = ComputePass::new(
            device,
            descriptor_pool,
            &bindings,
            &templates,
            &INPUT_OUTPUT_BANK_INFO,
            &[],
            VULKAN_UINT8_COMP_SPV,
            None,
        )?;
        Ok(Self {
            base,
            scheduler: NonNull::from(scheduler),
            staging_buffer_pool: NonNull::from(staging_buffer_pool),
            compute_pass_descriptor_queue: NonNull::from(compute_pass_descriptor_queue),
        })
    }

    /// Port of `Uint8Pass::Assemble`.
    ///
    /// Dispatches the uint8-to-uint16 conversion compute shader.
    /// Returns `(buffer, offset)` pair for the assembled index buffer.
    pub fn assemble(
        &mut self,
        num_vertices: u32,
        src_buffer: vk::Buffer,
        src_offset: u32,
    ) -> (vk::Buffer, vk::DeviceSize) {
        let staging_size = u64::from(num_vertices.wrapping_mul(std::mem::size_of::<u16>() as u32));
        let staging = unsafe { self.staging_buffer_pool.as_mut() }
            .request_device_local_buffer(staging_size)
            .expect("Uint8Pass device-local staging allocation failed");
        let scheduler = unsafe { self.scheduler.as_mut() };
        let descriptor_data = unsafe {
            let queue = self.compute_pass_descriptor_queue.as_mut();
            queue.acquire(scheduler, 2, false);
            queue.add_buffer(src_buffer, u64::from(src_offset), u64::from(num_vertices));
            queue.add_buffer(staging.buffer, staging.offset, staging_size);
            DescriptorData(queue.update_data())
        };
        let num_workgroups = (num_vertices + DISPATCH_SIZE - 1) / DISPATCH_SIZE;
        let descriptor_allocator = self.base.descriptor_allocator.clone();
        scheduler.request_outside_render_pass_operation_context();
        let device = self.base.device.clone();
        let descriptor_template = self.base.descriptor_template;
        let pipeline = self.base.pipeline;
        let layout = self.base.layout;
        scheduler.record(move |cmdbuf| unsafe {
            let descriptor_set = descriptor_allocator
                .commit()
                .expect("Uint8Pass descriptor allocation failed");
            device.update_descriptor_set_with_template(
                descriptor_set,
                descriptor_template,
                descriptor_data.as_raw_data(),
            );
            device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::COMPUTE, pipeline);
            device.cmd_bind_descriptor_sets(
                cmdbuf,
                vk::PipelineBindPoint::COMPUTE,
                layout,
                0,
                &[descriptor_set],
                &[],
            );
            device.cmd_dispatch(cmdbuf, num_workgroups, 1, 1);
            let barrier = write_barrier_vertex();
            device.cmd_pipeline_barrier(
                cmdbuf,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::VERTEX_INPUT,
                vk::DependencyFlags::empty(),
                &[barrier],
                &[],
                &[],
            );
        });
        (staging.buffer, staging.offset)
    }
}

// ---------------------------------------------------------------------------
// QuadIndexedPass
// ---------------------------------------------------------------------------

/// Port of `QuadIndexedPass` class.
///
/// Assembles quad-indexed geometry into triangle indices.
pub struct QuadIndexedPass {
    base: ComputePass,
    scheduler: NonNull<Scheduler>,
    staging_buffer_pool: NonNull<StagingBufferPool>,
    compute_pass_descriptor_queue: NonNull<ComputePassDescriptorQueue>,
}

impl QuadIndexedPass {
    /// Port of `QuadIndexedPass::QuadIndexedPass`.
    pub fn new(
        device: &ash::Device,
        scheduler: &mut Scheduler,
        descriptor_pool: &DescriptorPool,
        staging_buffer_pool: &mut StagingBufferPool,
        compute_pass_descriptor_queue: &mut ComputePassDescriptorQueue,
    ) -> Result<Self, vk::Result> {
        let bindings = input_output_bindings();
        let templates = input_output_descriptor_template();
        let push_constants = [vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::COMPUTE,
            offset: 0,
            size: (std::mem::size_of::<u32>() * 3) as u32,
        }];
        let base = ComputePass::new(
            device,
            descriptor_pool,
            &bindings,
            &templates,
            &INPUT_OUTPUT_BANK_INFO,
            &push_constants,
            VULKAN_QUAD_INDEXED_COMP_SPV,
            None,
        )?;
        Ok(Self {
            base,
            scheduler: NonNull::from(scheduler),
            staging_buffer_pool: NonNull::from(staging_buffer_pool),
            compute_pass_descriptor_queue: NonNull::from(compute_pass_descriptor_queue),
        })
    }

    /// Port of `QuadIndexedPass::Assemble`.
    ///
    /// Converts quad indices to triangle indices via compute dispatch.
    pub fn assemble(
        &mut self,
        index_format: IndexFormat,
        num_vertices: u32,
        base_vertex: u32,
        src_buffer: vk::Buffer,
        src_offset: u32,
        is_strip: bool,
    ) -> (vk::Buffer, vk::DeviceSize) {
        let index_shift = Self::index_shift_for_format(index_format);
        let input_size = num_vertices << index_shift;
        let num_tri_vertices = (if is_strip {
            num_vertices.wrapping_sub(2) / 2
        } else {
            num_vertices / 4
        })
        .wrapping_mul(6);
        let staging_size =
            u64::from(num_tri_vertices.wrapping_mul(std::mem::size_of::<u32>() as u32));
        let staging = unsafe { self.staging_buffer_pool.as_mut() }
            .request_device_local_buffer(staging_size)
            .expect("QuadIndexedPass device-local staging allocation failed");
        let scheduler = unsafe { self.scheduler.as_mut() };
        let descriptor_data = unsafe {
            let queue = self.compute_pass_descriptor_queue.as_mut();
            queue.acquire(scheduler, 2, false);
            queue.add_buffer(src_buffer, u64::from(src_offset), u64::from(input_size));
            queue.add_buffer(staging.buffer, staging.offset, staging_size);
            DescriptorData(queue.update_data())
        };
        let push_constants: [u32; 3] = [base_vertex, index_shift, if is_strip { 1 } else { 0 }];
        let num_workgroups = (num_tri_vertices + DISPATCH_SIZE - 1) / DISPATCH_SIZE;
        let descriptor_allocator = self.base.descriptor_allocator.clone();
        scheduler.request_outside_render_pass_operation_context();
        let device = self.base.device.clone();
        let descriptor_template = self.base.descriptor_template;
        let pipeline = self.base.pipeline;
        let layout = self.base.layout;
        scheduler.record(move |cmdbuf| unsafe {
            let descriptor_set = descriptor_allocator
                .commit()
                .expect("QuadIndexedPass descriptor allocation failed");
            device.update_descriptor_set_with_template(
                descriptor_set,
                descriptor_template,
                descriptor_data.as_raw_data(),
            );
            device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::COMPUTE, pipeline);
            device.cmd_bind_descriptor_sets(
                cmdbuf,
                vk::PipelineBindPoint::COMPUTE,
                layout,
                0,
                &[descriptor_set],
                &[],
            );
            device.cmd_push_constants(
                cmdbuf,
                layout,
                vk::ShaderStageFlags::COMPUTE,
                0,
                bytemuck::bytes_of(&push_constants),
            );
            device.cmd_dispatch(cmdbuf, num_workgroups, 1, 1);

            let barrier = write_barrier_index();
            device.cmd_pipeline_barrier(
                cmdbuf,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::VERTEX_INPUT,
                vk::DependencyFlags::empty(),
                &[barrier],
                &[],
                &[],
            );
        });
        (staging.buffer, staging.offset)
    }

    /// Port of index_shift calculation from QuadIndexedPass::Assemble.
    pub fn index_shift_for_format(index_format: IndexFormat) -> u32 {
        match index_format {
            IndexFormat::UnsignedByte => 0,
            IndexFormat::UnsignedShort => 1,
            IndexFormat::UnsignedInt => 2,
        }
    }
}

// ---------------------------------------------------------------------------
// ConditionalRenderingResolvePass
// ---------------------------------------------------------------------------

/// Port of `ConditionalRenderingResolvePass` class.
///
/// Resolves conditional rendering predicates via compute.
pub struct ConditionalRenderingResolvePass {
    base: ComputePass,
    scheduler: NonNull<Scheduler>,
    compute_pass_descriptor_queue: NonNull<ComputePassDescriptorQueue>,
}

impl ConditionalRenderingResolvePass {
    /// Port of `ConditionalRenderingResolvePass::ConditionalRenderingResolvePass`.
    pub fn new(
        device: &ash::Device,
        scheduler: &mut Scheduler,
        descriptor_pool: &DescriptorPool,
        compute_pass_descriptor_queue: &mut ComputePassDescriptorQueue,
    ) -> Result<Self, vk::Result> {
        let bindings = input_output_bindings();
        let templates = input_output_descriptor_template();
        let push_constants = [vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::COMPUTE,
            offset: 0,
            size: std::mem::size_of::<ConditionalRenderingResolvePushConstants>() as u32,
        }];
        let base = ComputePass::new(
            device,
            descriptor_pool,
            &bindings,
            &templates,
            &INPUT_OUTPUT_BANK_INFO,
            &push_constants,
            RESOLVE_CONDITIONAL_RENDER_COMP_SPV,
            None,
        )?;
        Ok(Self {
            base,
            scheduler: NonNull::from(scheduler),
            compute_pass_descriptor_queue: NonNull::from(compute_pass_descriptor_queue),
        })
    }

    /// Port of `ConditionalRenderingResolvePass::Resolve`.
    ///
    /// Dispatches the conditional rendering resolve compute shader.
    pub fn resolve(
        &mut self,
        dst_buffer: vk::Buffer,
        src_buffer: vk::Buffer,
        src_offset: u32,
        compare_to_zero: bool,
    ) {
        let compare_size = if compare_to_zero { 8 } else { 24 };
        let scheduler = unsafe { self.scheduler.as_mut() };
        let descriptor_data = unsafe {
            let queue = self.compute_pass_descriptor_queue.as_mut();
            queue.acquire(scheduler, 2, false);
            queue.add_buffer(src_buffer, u64::from(src_offset), compare_size);
            queue.add_buffer(dst_buffer, 0, std::mem::size_of::<u32>() as u64);
            DescriptorData(queue.update_data())
        };
        let descriptor_allocator = self.base.descriptor_allocator.clone();
        scheduler.request_outside_render_pass_operation_context();
        let device = self.base.device.clone();
        let descriptor_template = self.base.descriptor_template;
        let pipeline = self.base.pipeline;
        let layout = self.base.layout;
        let uniforms = ConditionalRenderingResolvePushConstants {
            compare_to_zero: u32::from(compare_to_zero),
        };
        scheduler.record(move |cmdbuf| unsafe {
            let descriptor_set = descriptor_allocator
                .commit()
                .expect("conditional rendering descriptor allocation failed");
            let read_barrier = vk::MemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE | vk::AccessFlags::SHADER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ | vk::AccessFlags::SHADER_WRITE)
                .build();
            let write_barrier = vk::MemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                .dst_access_mask(vk::AccessFlags::CONDITIONAL_RENDERING_READ_EXT)
                .build();
            device.update_descriptor_set_with_template(
                descriptor_set,
                descriptor_template,
                descriptor_data.as_raw_data(),
            );
            device.cmd_pipeline_barrier(
                cmdbuf,
                vk::PipelineStageFlags::ALL_GRAPHICS
                    | vk::PipelineStageFlags::COMPUTE_SHADER
                    | vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::DependencyFlags::empty(),
                &[read_barrier],
                &[],
                &[],
            );
            device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::COMPUTE, pipeline);
            device.cmd_bind_descriptor_sets(
                cmdbuf,
                vk::PipelineBindPoint::COMPUTE,
                layout,
                0,
                &[descriptor_set],
                &[],
            );
            device.cmd_push_constants(
                cmdbuf,
                layout,
                vk::ShaderStageFlags::COMPUTE,
                0,
                bytemuck::bytes_of(&uniforms),
            );
            device.cmd_dispatch(cmdbuf, 1, 1, 1);
            device.cmd_pipeline_barrier(
                cmdbuf,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::CONDITIONAL_RENDERING_EXT,
                vk::DependencyFlags::empty(),
                &[write_barrier],
                &[],
                &[],
            );
        });
    }
}

// ---------------------------------------------------------------------------
// QueriesPrefixScanPass
// ---------------------------------------------------------------------------

/// Port of `QueriesPrefixScanPass` class.
///
/// Performs prefix sum scan over query results via compute.
pub struct QueriesPrefixScanPass {
    base: ComputePass,
    scheduler: NonNull<Scheduler>,
    compute_pass_descriptor_queue: NonNull<ComputePassDescriptorQueue>,
    conditional_rendering_supported: bool,
}

impl QueriesPrefixScanPass {
    /// Port of `QueriesPrefixScanPass::QueriesPrefixScanPass`.
    pub fn new(
        device: &ash::Device,
        scheduler: &mut Scheduler,
        descriptor_pool: &DescriptorPool,
        compute_pass_descriptor_queue: &mut ComputePassDescriptorQueue,
        subgroup_scan_supported: bool,
        conditional_rendering_supported: bool,
    ) -> Result<Self, vk::Result> {
        let bindings = queries_scan_bindings();
        let templates = queries_scan_descriptor_template();
        let push_constants = [vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::COMPUTE,
            offset: 0,
            size: std::mem::size_of::<QueriesPrefixScanPushConstants>() as u32,
        }];
        let code = if subgroup_scan_supported {
            QUERIES_PREFIX_SCAN_SUM_COMP_SPV
        } else {
            QUERIES_PREFIX_SCAN_SUM_NOSUBGROUPS_COMP_SPV
        };
        let base = ComputePass::new(
            device,
            descriptor_pool,
            &bindings,
            &templates,
            &QUERIES_SCAN_BANK_INFO,
            &push_constants,
            code,
            None,
        )?;
        Ok(Self {
            base,
            scheduler: NonNull::from(scheduler),
            compute_pass_descriptor_queue: NonNull::from(compute_pass_descriptor_queue),
            conditional_rendering_supported,
        })
    }

    /// Port of `QueriesPrefixScanPass::Run`.
    ///
    /// Runs the prefix scan in batches of up to DISPATCH_SIZE (2048) elements.
    pub fn run(
        &mut self,
        accumulation_buffer: vk::Buffer,
        dst_buffer: vk::Buffer,
        src_buffer: vk::Buffer,
        number_of_sums: usize,
        min_accumulation_limit: usize,
        max_accumulation_limit: usize,
    ) {
        let mut current_runs = number_of_sums;
        let mut offset: usize = 0;

        while current_runs != 0 {
            let runs_to_do = current_runs.min(QUERIES_DISPATCH_SIZE);
            current_runs -= runs_to_do;
            let used_offset = offset;
            offset += runs_to_do;

            let uniforms = QueriesPrefixScanPushConstants {
                min_accumulation_base: min_accumulation_limit as u32,
                max_accumulation_base: max_accumulation_limit as u32,
                accumulation_limit: (runs_to_do - 1) as u32,
                buffer_offset: used_offset as u32,
            };
            let scheduler = unsafe { self.scheduler.as_mut() };
            let descriptor_data = unsafe {
                let queue = self.compute_pass_descriptor_queue.as_mut();
                queue.acquire(scheduler, 3, false);
                let query_range = (number_of_sums * std::mem::size_of::<u64>()) as u64;
                queue.add_buffer(src_buffer, 0, query_range);
                queue.add_buffer(dst_buffer, 0, query_range);
                queue.add_buffer(accumulation_buffer, 0, std::mem::size_of::<u64>() as u64);
                DescriptorData(queue.update_data())
            };
            let descriptor_allocator = self.base.descriptor_allocator.clone();
            scheduler.request_outside_render_pass_operation_context();
            let device = self.base.device.clone();
            let descriptor_template = self.base.descriptor_template;
            let pipeline = self.base.pipeline;
            let layout = self.base.layout;
            let conditional_rendering_supported = self.conditional_rendering_supported;
            scheduler.record(move |cmdbuf| unsafe {
                let descriptor_set = descriptor_allocator
                    .commit()
                    .expect("query prefix-scan descriptor allocation failed");
                let read_barrier = vk::MemoryBarrier::builder()
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::SHADER_READ | vk::AccessFlags::SHADER_WRITE)
                    .build();
                let write_barrier = vk::MemoryBarrier::builder()
                    .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                    .dst_access_mask(
                        vk::AccessFlags::SHADER_READ
                            | vk::AccessFlags::TRANSFER_READ
                            | vk::AccessFlags::VERTEX_ATTRIBUTE_READ
                            | vk::AccessFlags::INDIRECT_COMMAND_READ
                            | vk::AccessFlags::INDEX_READ
                            | vk::AccessFlags::UNIFORM_READ
                            | if conditional_rendering_supported {
                                vk::AccessFlags::CONDITIONAL_RENDERING_READ_EXT
                            } else {
                                vk::AccessFlags::empty()
                            },
                    )
                    .build();
                device.update_descriptor_set_with_template(
                    descriptor_set,
                    descriptor_template,
                    descriptor_data.as_raw_data(),
                );
                device.cmd_pipeline_barrier(
                    cmdbuf,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::DependencyFlags::empty(),
                    &[read_barrier],
                    &[],
                    &[],
                );
                device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::COMPUTE, pipeline);
                device.cmd_bind_descriptor_sets(
                    cmdbuf,
                    vk::PipelineBindPoint::COMPUTE,
                    layout,
                    0,
                    &[descriptor_set],
                    &[],
                );
                device.cmd_push_constants(
                    cmdbuf,
                    layout,
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    bytemuck::bytes_of(&uniforms),
                );
                device.cmd_dispatch(cmdbuf, 1, 1, 1);
                device.cmd_pipeline_barrier(
                    cmdbuf,
                    vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::PipelineStageFlags::ALL_GRAPHICS | vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::DependencyFlags::empty(),
                    &[write_barrier],
                    &[],
                    &[],
                );
            });
        }
    }
}

// ---------------------------------------------------------------------------
// ASTCDecoderPass
// ---------------------------------------------------------------------------

/// Port of `ASTCDecoderPass` class.
///
/// GPU-accelerated ASTC texture decoding via compute shader.
pub struct AstcDecoderPass {
    base: ComputePass,
    compute_pass_descriptor_queue: NonNull<ComputePassDescriptorQueue>,
}

impl AstcDecoderPass {
    /// Port of `ASTCDecoderPass::ASTCDecoderPass`.
    pub fn new(
        device: &ash::Device,
        descriptor_pool: &mut DescriptorPool,
        compute_pass_descriptor_queue: &mut ComputePassDescriptorQueue,
    ) -> Result<Self, vk::Result> {
        let bindings: [vk::DescriptorSetLayoutBinding; ASTC_NUM_BINDINGS] = [
            vk::DescriptorSetLayoutBinding {
                binding: ASTC_BINDING_INPUT_BUFFER,
                descriptor_type: vk::DescriptorType::STORAGE_BUFFER,
                descriptor_count: 1,
                stage_flags: vk::ShaderStageFlags::COMPUTE,
                p_immutable_samplers: std::ptr::null(),
            },
            vk::DescriptorSetLayoutBinding {
                binding: ASTC_BINDING_OUTPUT_IMAGE,
                descriptor_type: vk::DescriptorType::STORAGE_IMAGE,
                descriptor_count: 1,
                stage_flags: vk::ShaderStageFlags::COMPUTE,
                p_immutable_samplers: std::ptr::null(),
            },
        ];
        let templates: [vk::DescriptorUpdateTemplateEntry; ASTC_NUM_BINDINGS] = [
            vk::DescriptorUpdateTemplateEntry {
                dst_binding: ASTC_BINDING_INPUT_BUFFER,
                dst_array_element: 0,
                descriptor_count: 1,
                descriptor_type: vk::DescriptorType::STORAGE_BUFFER,
                offset: 0,
                stride: std::mem::size_of::<
                    crate::renderer_vulkan::update_descriptor::DescriptorUpdateEntry,
                >(),
            },
            vk::DescriptorUpdateTemplateEntry {
                dst_binding: ASTC_BINDING_OUTPUT_IMAGE,
                dst_array_element: 0,
                descriptor_count: 1,
                descriptor_type: vk::DescriptorType::STORAGE_IMAGE,
                offset: std::mem::size_of::<
                    crate::renderer_vulkan::update_descriptor::DescriptorUpdateEntry,
                >(),
                stride: std::mem::size_of::<
                    crate::renderer_vulkan::update_descriptor::DescriptorUpdateEntry,
                >(),
            },
        ];
        let push_constants = [vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::COMPUTE,
            offset: 0,
            size: std::mem::size_of::<AstcPushConstants>() as u32,
        }];
        let base = ComputePass::new(
            device,
            descriptor_pool,
            &bindings,
            &templates,
            &ASTC_BANK_INFO,
            &push_constants,
            ASTC_DECODER_COMP_SPV,
            None,
        )?;
        Ok(AstcDecoderPass {
            base,
            compute_pass_descriptor_queue: NonNull::from(compute_pass_descriptor_queue),
        })
    }

    /// Port of `ASTCDecoderPass::Assemble`.
    pub fn assemble(
        &mut self,
        device: &ash::Device,
        scheduler: &mut Scheduler,
        image: vk::Image,
        aspect_mask: vk::ImageAspectFlags,
        is_initialized: bool,
        info: &ImageInfo,
        guest_size_bytes: usize,
        staging_buffer: vk::Buffer,
        staging_offset: vk::DeviceSize,
        swizzles: &[SwizzleParameters],
        storage_views: &[vk::ImageView],
    ) -> bool {
        let device_handle = device.clone();
        let block_dims = [
            crate::surface::default_block_width(info.format),
            crate::surface::default_block_height(info.format),
        ];
        let pipeline = self.base.pipeline;
        let layout = self.base.layout;
        scheduler.request_outside_render_pass_operation_context();
        let device = device_handle.clone();
        scheduler.record(move |cmdbuf| unsafe {
            let image_barrier = vk::ImageMemoryBarrier::builder()
                .src_access_mask(if is_initialized {
                    vk::AccessFlags::SHADER_WRITE
                } else {
                    vk::AccessFlags::empty()
                })
                .dst_access_mask(vk::AccessFlags::SHADER_READ | vk::AccessFlags::SHADER_WRITE)
                .old_layout(if is_initialized {
                    vk::ImageLayout::GENERAL
                } else {
                    vk::ImageLayout::UNDEFINED
                })
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask,
                    base_mip_level: 0,
                    level_count: vk::REMAINING_MIP_LEVELS,
                    base_array_layer: 0,
                    layer_count: vk::REMAINING_ARRAY_LAYERS,
                })
                .build();
            device.cmd_pipeline_barrier(
                cmdbuf,
                if is_initialized {
                    vk::PipelineStageFlags::ALL_COMMANDS
                } else {
                    vk::PipelineStageFlags::TOP_OF_PIPE
                },
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[image_barrier],
            );
            device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::COMPUTE, pipeline);
        });

        for swizzle in swizzles {
            let Some(&storage_view) = storage_views.get(swizzle.level as usize) else {
                log::warn!(
                    "ASTCDecoderPass::assemble: missing storage view for level {}",
                    swizzle.level
                );
                return false;
            };
            if storage_view == vk::ImageView::null() {
                return false;
            }
            let input_offset = staging_offset + swizzle.buffer_offset as vk::DeviceSize;
            let range_size =
                guest_size_bytes.saturating_sub(swizzle.buffer_offset) as vk::DeviceSize;
            let num_dispatches_x = swizzle.num_tiles.width.div_ceil(8);
            let num_dispatches_y = swizzle.num_tiles.height.div_ceil(8);
            let num_dispatches_z = info.resources.layers as u32;

            let params = make_block_linear_swizzle_2d_params(swizzle, info);
            if params.origin != [0, 0, 0]
                || params.destination != [0, 0, 0]
                || params.bytes_per_block_log2 != 4
            {
                log::warn!(
                    "ASTCDecoderPass::assemble: unsupported swizzle params origin={:?} destination={:?} bpp_log2={}",
                    params.origin,
                    params.destination,
                    params.bytes_per_block_log2
                );
                return false;
            }

            let descriptor_set = {
                match self.base.descriptor_allocator.commit() {
                    Ok(set) => set,
                    Err(err) => {
                        log::warn!(
                            "ASTCDecoderPass::assemble: failed to allocate descriptor set: {err:?}"
                        );
                        return false;
                    }
                }
            };
            let descriptor_buffer = vk::DescriptorBufferInfo {
                buffer: staging_buffer,
                offset: input_offset,
                range: range_size.max(1),
            };
            let descriptor_image = vk::DescriptorImageInfo {
                sampler: vk::Sampler::null(),
                image_view: storage_view,
                image_layout: vk::ImageLayout::GENERAL,
            };
            unsafe {
                self.compute_pass_descriptor_queue
                    .as_mut()
                    .acquire(scheduler, 2, false);
                self.compute_pass_descriptor_queue.as_mut().add_buffer(
                    staging_buffer,
                    input_offset,
                    range_size.max(1),
                );
                self.compute_pass_descriptor_queue
                    .as_mut()
                    .add_image(storage_view);
            }
            let uniforms = AstcPushConstants {
                blocks_dims: block_dims,
                layer_stride: params.layer_stride,
                block_size: params.block_size,
                x_shift: params.x_shift,
                block_height: params.block_height,
                block_height_mask: params.block_height_mask,
            };
            let device = device_handle.clone();
            scheduler.record(move |cmdbuf| unsafe {
                let writes = [
                    vk::WriteDescriptorSet::builder()
                        .dst_set(descriptor_set)
                        .dst_binding(ASTC_BINDING_INPUT_BUFFER)
                        .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                        .buffer_info(std::slice::from_ref(&descriptor_buffer))
                        .build(),
                    vk::WriteDescriptorSet::builder()
                        .dst_set(descriptor_set)
                        .dst_binding(ASTC_BINDING_OUTPUT_IMAGE)
                        .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                        .image_info(std::slice::from_ref(&descriptor_image))
                        .build(),
                ];
                device.update_descriptor_sets(&writes, &[]);
                device.cmd_bind_descriptor_sets(
                    cmdbuf,
                    vk::PipelineBindPoint::COMPUTE,
                    layout,
                    0,
                    &[descriptor_set],
                    &[],
                );
                device.cmd_push_constants(
                    cmdbuf,
                    layout,
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    bytemuck::bytes_of(&uniforms),
                );
                device.cmd_dispatch(cmdbuf, num_dispatches_x, num_dispatches_y, num_dispatches_z);
            });
        }

        let device = device_handle;
        scheduler.record(move |cmdbuf| unsafe {
            let image_barrier = vk::ImageMemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ | vk::AccessFlags::SHADER_WRITE)
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask,
                    base_mip_level: 0,
                    level_count: vk::REMAINING_MIP_LEVELS,
                    base_array_layer: 0,
                    layer_count: vk::REMAINING_ARRAY_LAYERS,
                })
                .build();
            device.cmd_pipeline_barrier(
                cmdbuf,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[image_barrier],
            );
        });
        scheduler.finish();
        true
    }
}

// ---------------------------------------------------------------------------
// BlockLinearUnswizzle3DPass
// ---------------------------------------------------------------------------

/// Port of `BlockLinearUnswizzle3DPass` from `vk_compute_pass.{h,cpp}`.
pub struct BlockLinearUnswizzle3DPass {
    base: ComputePass,
    scheduler: NonNull<Scheduler>,
    #[allow(dead_code)]
    staging_buffer_pool: NonNull<StagingBufferPool>,
    compute_pass_descriptor_queue: NonNull<ComputePassDescriptorQueue>,
}

impl BlockLinearUnswizzle3DPass {
    pub fn new(
        device: &ash::Device,
        scheduler: &mut Scheduler,
        descriptor_pool: &DescriptorPool,
        staging_buffer_pool: &mut StagingBufferPool,
        compute_pass_descriptor_queue: &mut ComputePassDescriptorQueue,
    ) -> Result<Self, vk::Result> {
        let bindings = input_output_bindings();
        let templates = [
            vk::DescriptorUpdateTemplateEntry {
                dst_binding: 0,
                dst_array_element: 0,
                descriptor_count: 1,
                descriptor_type: vk::DescriptorType::STORAGE_BUFFER,
                offset: 0,
                stride: std::mem::size_of::<DescriptorUpdateEntry>(),
            },
            vk::DescriptorUpdateTemplateEntry {
                dst_binding: 1,
                dst_array_element: 0,
                descriptor_count: 1,
                descriptor_type: vk::DescriptorType::STORAGE_BUFFER,
                offset: std::mem::size_of::<DescriptorUpdateEntry>(),
                stride: std::mem::size_of::<DescriptorUpdateEntry>(),
            },
        ];
        let push_constants = [vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::COMPUTE,
            offset: 0,
            size: std::mem::size_of::<BlockLinearUnswizzle3DPushConstants>() as u32,
        }];
        let base = ComputePass::new(
            device,
            descriptor_pool,
            &bindings,
            &templates,
            &INPUT_OUTPUT_BANK_INFO,
            &push_constants,
            BLOCK_LINEAR_UNSWIZZLE_3D_BCN_COMP_SPV,
            None,
        )?;
        Ok(Self {
            base,
            scheduler: NonNull::from(scheduler),
            staging_buffer_pool: NonNull::from(staging_buffer_pool),
            compute_pass_descriptor_queue: NonNull::from(compute_pass_descriptor_queue),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn unswizzle(
        &mut self,
        image: vk::Image,
        aspect: vk::ImageAspectFlags,
        info: &ImageInfo,
        guest_size_bytes: usize,
        output_buffer: vk::Buffer,
        output_buffer_size: vk::DeviceSize,
        swizzled_buffer: vk::Buffer,
        swizzled_offset: vk::DeviceSize,
        swizzles: &[SwizzleParameters],
        z_start: u32,
        z_count: u32,
    ) -> bool {
        let max_batch_slices = z_count.min(info.size.depth);
        if max_batch_slices == 0 || swizzles.len() != 1 {
            return false;
        }
        let swizzle = &swizzles[0];
        let params = make_block_linear_swizzle_3d_params(swizzle, info);
        let blocks_x = info.size.width.div_ceil(4);
        let blocks_y = info.size.height.div_ceil(4);

        let scheduler = unsafe { self.scheduler.as_mut() };
        scheduler.request_outside_render_pass_operation_context();
        for z_offset in (0..z_count).step_by(max_batch_slices as usize) {
            let current_chunk_slices = max_batch_slices.min(z_count - z_offset);
            let current_z_start = z_start + z_offset;
            self.unswizzle_chunk(
                image,
                aspect,
                info.size.width,
                info.size.height,
                guest_size_bytes,
                output_buffer,
                output_buffer_size,
                swizzled_buffer,
                swizzled_offset,
                swizzle,
                params,
                blocks_x,
                blocks_y,
                current_z_start,
                current_chunk_slices,
            );
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn unswizzle_chunk(
        &mut self,
        image: vk::Image,
        aspect: vk::ImageAspectFlags,
        image_width: u32,
        image_height: u32,
        guest_size_bytes: usize,
        output_buffer: vk::Buffer,
        output_buffer_size: vk::DeviceSize,
        swizzled_buffer: vk::Buffer,
        swizzled_offset: vk::DeviceSize,
        swizzle: &SwizzleParameters,
        params: crate::texture_cache::accelerated_swizzle::BlockLinearSwizzle3DParams,
        blocks_x: u32,
        blocks_y: u32,
        z_start: u32,
        z_count: u32,
    ) {
        let push_constants = BlockLinearUnswizzle3DPushConstants {
            blocks_dim: [blocks_x, blocks_y, z_count],
            bytes_per_block_log2: params.bytes_per_block_log2,
            origin: [params.origin[0], params.origin[1], z_start],
            slice_size: params.slice_size,
            block_size: params.block_size,
            x_shift: params.x_shift,
            block_height: params.block_height,
            block_height_mask: params.block_height_mask,
            block_depth: params.block_depth,
            block_depth_mask: params.block_depth_mask,
            _pad: 0,
            destination: [params.destination[0], params.destination[1], 0],
            _pad_end: 0,
        };
        let input_offset = swizzled_offset + swizzle.buffer_offset as vk::DeviceSize;
        let input_size = guest_size_bytes
            .saturating_sub(swizzle.buffer_offset as usize)
            .max(1) as vk::DeviceSize;
        let scheduler = unsafe { self.scheduler.as_mut() };
        let descriptor_data = unsafe {
            let queue = self.compute_pass_descriptor_queue.as_mut();
            queue.acquire(scheduler, 3, false);
            queue.add_buffer(swizzled_buffer, input_offset, input_size);
            queue.add_buffer(output_buffer, 0, output_buffer_size);
            DescriptorData(queue.update_data())
        };
        let dispatch_x = blocks_x.div_ceil(8);
        let dispatch_y = blocks_y.div_ceil(8);
        let dispatch_z = z_count.div_ceil(4);
        let bytes_per_block = 1u64 << push_constants.bytes_per_block_log2;
        let barrier_size =
            u64::from(blocks_x) * u64::from(blocks_y) * bytes_per_block * u64::from(z_count);
        let is_first_chunk = z_start == 0;
        let descriptor_allocator = self.base.descriptor_allocator.clone();
        let device = self.base.device.clone();
        let descriptor_template = self.base.descriptor_template;
        let pipeline = self.base.pipeline;
        let layout = self.base.layout;
        scheduler.record(move |cmdbuf| unsafe {
            if image == vk::Image::null() || output_buffer == vk::Buffer::null() {
                return;
            }
            let descriptor_set = descriptor_allocator
                .commit()
                .expect("BlockLinearUnswizzle3DPass descriptor allocation failed");
            device.update_descriptor_set_with_template(
                descriptor_set,
                descriptor_template,
                descriptor_data.as_raw_data(),
            );
            device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::COMPUTE, pipeline);
            device.cmd_bind_descriptor_sets(
                cmdbuf,
                vk::PipelineBindPoint::COMPUTE,
                layout,
                0,
                &[descriptor_set],
                &[],
            );
            device.cmd_push_constants(
                cmdbuf,
                layout,
                vk::ShaderStageFlags::COMPUTE,
                0,
                bytemuck::bytes_of(&push_constants),
            );
            device.cmd_dispatch(cmdbuf, dispatch_x, dispatch_y, dispatch_z);

            let buffer_barrier = vk::BufferMemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .buffer(output_buffer)
                .offset(0)
                .size(barrier_size)
                .build();
            let pre_barrier = vk::ImageMemoryBarrier::builder()
                .src_access_mask(if is_first_chunk {
                    vk::AccessFlags::empty()
                } else {
                    vk::AccessFlags::TRANSFER_WRITE
                })
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .old_layout(if is_first_chunk {
                    vk::ImageLayout::UNDEFINED
                } else {
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL
                })
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: aspect,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .build();
            device.cmd_pipeline_barrier(
                cmdbuf,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[buffer_barrier],
                &[pre_barrier],
            );
            let copy = vk::BufferImageCopy {
                buffer_offset: 0,
                buffer_row_length: 0,
                buffer_image_height: 0,
                image_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: aspect,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                image_offset: vk::Offset3D {
                    x: 0,
                    y: 0,
                    z: z_start as i32,
                },
                image_extent: vk::Extent3D {
                    width: image_width,
                    height: image_height,
                    depth: z_count,
                },
            };
            device.cmd_copy_buffer_to_image(
                cmdbuf,
                output_buffer,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[copy],
            );
            let post_barrier = vk::ImageMemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ | vk::AccessFlags::SHADER_WRITE)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: aspect,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .build();
            device.cmd_pipeline_barrier(
                cmdbuf,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER | vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[post_barrier],
            );
        });
    }
}

// Implement bytemuck traits for push constants that need it
unsafe impl bytemuck::Zeroable for AstcPushConstants {}
unsafe impl bytemuck::Pod for AstcPushConstants {}
unsafe impl bytemuck::Zeroable for QueriesPrefixScanPushConstants {}
unsafe impl bytemuck::Pod for QueriesPrefixScanPushConstants {}
unsafe impl bytemuck::Zeroable for ConditionalRenderingResolvePushConstants {}
unsafe impl bytemuck::Pod for ConditionalRenderingResolvePushConstants {}
unsafe impl bytemuck::Zeroable for BlockLinearUnswizzle3DPushConstants {}
unsafe impl bytemuck::Pod for BlockLinearUnswizzle3DPushConstants {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn astc_push_constants_size() {
        assert_eq!(
            std::mem::size_of::<AstcPushConstants>(),
            7 * std::mem::size_of::<u32>()
        );
    }

    #[test]
    fn queries_prefix_scan_push_constants_size() {
        assert_eq!(
            std::mem::size_of::<QueriesPrefixScanPushConstants>(),
            4 * std::mem::size_of::<u32>()
        );
    }

    #[test]
    fn block_linear_unswizzle_3d_push_constants_layout() {
        assert_eq!(
            std::mem::size_of::<BlockLinearUnswizzle3DPushConstants>(),
            76
        );
        let value = BlockLinearUnswizzle3DPushConstants::default();
        let base = std::ptr::addr_of!(value) as usize;
        assert_eq!(std::ptr::addr_of!(value.destination) as usize - base, 60);
    }

    #[test]
    fn bank_info_constants() {
        assert_eq!(INPUT_OUTPUT_BANK_INFO.storage_buffers, 2);
        assert_eq!(QUERIES_SCAN_BANK_INFO.storage_buffers, 3);
        assert_eq!(ASTC_BANK_INFO.images, 1);
    }

    #[test]
    fn indexed_conversion_layout_matches_upstream() {
        assert_eq!(
            QuadIndexedPass::index_shift_for_format(IndexFormat::UnsignedByte),
            0
        );
        assert_eq!(
            QuadIndexedPass::index_shift_for_format(IndexFormat::UnsignedShort),
            1
        );
        assert_eq!(
            QuadIndexedPass::index_shift_for_format(IndexFormat::UnsignedInt),
            2
        );

        let bindings = input_output_bindings();
        assert_eq!(bindings.len(), 2);
        assert!(bindings
            .iter()
            .all(|binding| binding.descriptor_type == vk::DescriptorType::STORAGE_BUFFER));
        let templates = input_output_descriptor_template();
        assert_eq!(templates[0].dst_binding, 0);
        assert_eq!(templates[0].descriptor_count, 2);
        assert_eq!(
            templates[0].stride,
            std::mem::size_of::<DescriptorUpdateEntry>()
        );
    }
}
