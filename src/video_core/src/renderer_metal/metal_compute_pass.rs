// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal counterparts of Eden's vk_compute_pass.{h,cpp} compute passes.
//! Binding namespaces and explicit lengths replace Vulkan descriptors; output
//! storage and execution remain staging-pool/scheduler owned.

use std::ptr::NonNull;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBarrierScope, MTLComputeCommandEncoder, MTLComputePipelineState, MTLDevice, MTLLibrary,
    MTLSize,
};
use shader_recompiler::backend::msl::MslVersion;
use thiserror::Error;

use super::metal_buffer::{MetalBuffer, MetalBufferError};
use super::metal_device::MetalDevice;
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};
use super::metal_shader::{compile_msl_library, MetalShaderError};
use super::metal_staging_buffer_pool::{
    MetalStagingBufferError, MetalStagingBufferPool, StagingBufferRef, StagingBufferUsage,
};
use crate::engines::maxwell_3d::IndexFormat;

#[derive(Debug, Error)]
pub enum MetalComputePassError {
    #[error("visibility query range is misaligned, empty or out of bounds")]
    VisibilityRange,
    #[error(transparent)]
    Buffer(#[from] MetalBufferError),
    #[error("native conditional arguments require aligned, in-bounds records and predicate")]
    ConditionalArgumentsRange,
    #[error("native conditional resolve requires aligned, in-bounds buffer ranges")]
    ConditionalRange,
    #[error("native index conversion source range is out of bounds")]
    SourceRange,
    #[error("native index conversion output size overflows")]
    OutputSize,
    #[error("native index conversion pipeline compilation failed: {0}")]
    Pipeline(String),
    #[error(transparent)]
    Shader(#[from] MetalShaderError),
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
    #[error(transparent)]
    Staging(#[from] MetalStagingBufferError),
}

struct ComputePass {
    pipeline: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
}

/// Native reduction equivalent of Eden's QueriesPrefixScanPass for Metal's
/// per-draw visibility slots. Reports use the final prefix only. Separate output
/// buffers preserve earlier reports while later reports continue accumulation.
pub struct VisibilityResolvePass {
    device: MetalDevice,
    pass: ComputePass,
}

impl VisibilityResolvePass {
    pub fn new(device: &MetalDevice) -> Result<Self, MetalComputePassError> {
        Ok(Self {
            device: device.clone(),
            pass: ComputePass::new(device,
                include_str!("../host_shaders/metal_visibility_resolve.metal"), "visibility_resolve")?,
        })
    }

    pub fn run(
        &self,
        scheduler: &mut MetalScheduler,
        source: &MetalBuffer,
        offset: usize,
        count: usize,
        accumulation: &MetalBuffer,
    ) -> Result<Arc<MetalBuffer>, MetalComputePassError> {
        if count == 0 || offset % 8 != 0 || count > u32::MAX as usize
            || offset / 8 > u32::MAX as usize || accumulation.length() < 8
            || count.checked_mul(8).and_then(|bytes| offset.checked_add(bytes))
                .is_none_or(|end| end > source.length())
        {
            return Err(MetalComputePassError::VisibilityRange);
        }
        scheduler.request_outside_render_pass_operation_context();
        let mut remaining = count;
        let mut previous_pass: Option<Arc<MetalBuffer>> = None;
        loop {
            let groups = remaining.div_ceil(256);
            let output = Arc::new(if groups == 1 {
                // Fence callbacks also read the final value after completion.
                MetalBuffer::new(&self.device, 8)?
            } else {
                MetalBuffer::new_private(&self.device, groups * 8)?
            });
            let input = previous_pass.as_deref().unwrap_or(source);
            let input_offset = if previous_pass.is_some() { 0 } else { offset };
            // uint3 has 16-byte size/alignment in MSL, including its padding word.
            let params = [0u32, remaining as u32, u32::from(groups == 1), 0];
            scheduler.with_compute_encoder(|encoder| {
                encoder.memoryBarrierWithScope(MTLBarrierScope::Buffers);
                encoder.setComputePipelineState(&self.pass.pipeline);
                unsafe {
                    encoder.setBuffer_offset_atIndex(Some(input.handle()), input_offset, 0);
                    encoder.setBuffer_offset_atIndex(Some(output.handle()), 0, 1);
                    encoder.setBuffer_offset_atIndex(Some(accumulation.handle()), 0, 2);
                    encoder.setBytes_length_atIndex(NonNull::from(&params).cast(), 16, 3);
                    encoder.dispatchThreadgroups_threadsPerThreadgroup(
                        MTLSize { width: groups, height: 1, depth: 1 },
                        MTLSize { width: 256, height: 1, depth: 1 },
                    );
                }
                encoder.memoryBarrierWithScope(MTLBarrierScope::Buffers);
            })?;
            if groups == 1 { return Ok(output); }
            previous_pass = Some(output);
            remaining = groups;
        }
    }
}

impl ComputePass {
    fn new(device: &MetalDevice, source: &str, entry: &str) -> Result<Self, MetalComputePassError> {
        let library = compile_msl_library(device.device(), source, MslVersion::V2_3)?;
        let function = library
            .newFunctionWithName(&NSString::from_str(entry))
            .ok_or_else(|| MetalShaderError::MissingEntryPoint(entry.into()))?;
        let pipeline = device
            .device()
            .newComputePipelineStateWithFunction_error(&function)
            .map_err(|error| MetalComputePassError::Pipeline(error.to_string()))?;
        Ok(Self { pipeline })
    }
}

pub struct Uint8Pass {
    pass: ComputePass,
}

impl Uint8Pass {
    pub fn new(device: &MetalDevice) -> Result<Self, MetalComputePassError> {
        Ok(Self {
            pass: ComputePass::new(
                device,
                include_str!("../host_shaders/metal_uint8.metal"),
                "assemble_uint8",
            )?,
        })
    }

    pub fn assemble(
        &self,
        scheduler: &mut MetalScheduler,
        staging_pool: &mut MetalStagingBufferPool,
        num_vertices: u32,
        src_buffer: &MetalBuffer,
        src_offset: usize,
    ) -> Result<StagingBufferRef, MetalComputePassError> {
        if src_offset
            .checked_add(num_vertices as usize)
            .is_none_or(|end| end > src_buffer.length())
        {
            return Err(MetalComputePassError::SourceRange);
        }
        let size = (num_vertices as usize)
            .checked_mul(2)
            .ok_or(MetalComputePassError::OutputSize)?;
        let staging = staging_pool.request(
            scheduler,
            size.max(4),
            StagingBufferUsage::DeviceLocal,
            false,
        )?;
        if num_vertices == 0 {
            return Ok(staging);
        }
        scheduler.request_outside_render_pass_operation_context();
        scheduler.with_compute_encoder(|encoder| {
            const DISPATCH_SIZE: usize = 1024;
            encoder.setComputePipelineState(&self.pass.pipeline);
            // SAFETY: source range was checked; destination covers all output
            // elements and setBytes copies the initialized count immediately.
            unsafe {
                encoder.setBuffer_offset_atIndex(Some(src_buffer.handle()), src_offset, 0);
                encoder.setBuffer_offset_atIndex(Some(staging.buffer.handle()), staging.offset, 1);
                encoder.setBytes_length_atIndex(NonNull::from(&num_vertices).cast(), 4, 2);
                encoder.dispatchThreads_threadsPerThreadgroup(
                    MTLSize {
                        width: num_vertices as usize,
                        height: 1,
                        depth: 1,
                    },
                    MTLSize {
                        width: DISPATCH_SIZE
                            .min(self.pass.pipeline.maxTotalThreadsPerThreadgroup()),
                        height: 1,
                        depth: 1,
                    },
                );
            }
            encoder.memoryBarrierWithScope(MTLBarrierScope::Buffers);
        })?;
        Ok(staging)
    }
}

pub struct QuadIndexedPass {
    pass: ComputePass,
}

impl QuadIndexedPass {
    pub fn new(device: &MetalDevice) -> Result<Self, MetalComputePassError> {
        Ok(Self {
            pass: ComputePass::new(
                device,
                include_str!("../host_shaders/metal_quad_indexed.metal"),
                "assemble_quad_indexed",
            )?,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn assemble(
        &self,
        scheduler: &mut MetalScheduler,
        staging_pool: &mut MetalStagingBufferPool,
        index_format: IndexFormat,
        num_vertices: u32,
        base_vertex: u32,
        src_buffer: &MetalBuffer,
        src_offset: usize,
        is_strip: bool,
    ) -> Result<StagingBufferRef, MetalComputePassError> {
        let index_shift = match index_format {
            IndexFormat::UnsignedByte => 0,
            IndexFormat::UnsignedShort => 1,
            IndexFormat::UnsignedInt => 2,
        };
        let input_size = (num_vertices as usize)
            .checked_shl(index_shift)
            .ok_or(MetalComputePassError::SourceRange)?;
        if src_offset
            .checked_add(input_size)
            .is_none_or(|end| end > src_buffer.length())
        {
            return Err(MetalComputePassError::SourceRange);
        }
        let num_primitives = if is_strip {
            num_vertices.saturating_sub(2) / 2
        } else {
            num_vertices / 4
        };
        let size = (num_primitives as usize)
            .checked_mul(6 * 4)
            .ok_or(MetalComputePassError::OutputSize)?;
        let staging = staging_pool.request(
            scheduler,
            size.max(4),
            StagingBufferUsage::DeviceLocal,
            false,
        )?;
        if num_primitives == 0 {
            return Ok(staging);
        }
        let params = [
            base_vertex,
            index_shift,
            u32::from(is_strip),
            num_primitives,
        ];
        scheduler.request_outside_render_pass_operation_context();
        scheduler.with_compute_encoder(|encoder| {
            const DISPATCH_SIZE: usize = 1024;
            encoder.setComputePipelineState(&self.pass.pipeline);
            // SAFETY: source range and output allocation cover complete quads;
            // the additional length word replaces GLSL's output_indexes.length().
            unsafe {
                encoder.setBuffer_offset_atIndex(Some(src_buffer.handle()), src_offset, 0);
                encoder.setBuffer_offset_atIndex(Some(staging.buffer.handle()), staging.offset, 1);
                encoder.setBytes_length_atIndex(NonNull::from(&params).cast(), 16, 2);
                encoder.dispatchThreads_threadsPerThreadgroup(
                    MTLSize {
                        width: num_primitives as usize,
                        height: 1,
                        depth: 1,
                    },
                    MTLSize {
                        width: DISPATCH_SIZE
                            .min(self.pass.pipeline.maxTotalThreadsPerThreadgroup()),
                        height: 1,
                        depth: 1,
                    },
                );
            }
            encoder.memoryBarrierWithScope(MTLBarrierScope::Buffers);
        })?;
        Ok(staging)
    }
}

/// Eden's ConditionalRenderingResolvePass, with a staging-relative destination
/// offset rather than Vulkan's standalone predicate buffer at offset zero.
pub struct ConditionalRenderingResolvePass {
    pass: ComputePass,
}

impl ConditionalRenderingResolvePass {
    pub fn new(device: &MetalDevice) -> Result<Self, MetalComputePassError> {
        Ok(Self {
            pass: ComputePass::new(
                device,
                include_str!("../host_shaders/metal_resolve_conditional_render.metal"),
                "resolve_conditional_render",
            )?,
        })
    }

    pub fn resolve(
        &self,
        scheduler: &mut MetalScheduler,
        dst_buffer: &MetalBuffer,
        dst_offset: usize,
        src_buffer: &MetalBuffer,
        src_offset: usize,
        compare_to_zero: bool,
    ) -> Result<(), MetalComputePassError> {
        let compare_size = if compare_to_zero { 8 } else { 24 };
        if src_offset % 4 != 0
            || dst_offset % 4 != 0
            || src_offset
                .checked_add(compare_size)
                .is_none_or(|end| end > src_buffer.length())
            || dst_offset
                .checked_add(4)
                .is_none_or(|end| end > dst_buffer.length())
        {
            return Err(MetalComputePassError::ConditionalRange);
        }
        let uniform = u32::from(compare_to_zero);
        scheduler.request_outside_render_pass_operation_context();
        scheduler.with_compute_encoder(|encoder| {
            // Tracked resources order previous encoders; this barrier also covers
            // preceding shader writes when the scheduler reuses a compute encoder.
            encoder.memoryBarrierWithScope(MTLBarrierScope::Buffers);
            encoder.setComputePipelineState(&self.pass.pipeline);
            // SAFETY: ranges and uint alignment are checked above. setBytes copies
            // the uniform, and the command buffer retains both bound resources.
            unsafe {
                encoder.setBuffer_offset_atIndex(Some(src_buffer.handle()), src_offset, 0);
                encoder.setBuffer_offset_atIndex(Some(dst_buffer.handle()), dst_offset, 1);
                encoder.setBytes_length_atIndex(NonNull::from(&uniform).cast(), 4, 2);
                let one = MTLSize {
                    width: 1,
                    height: 1,
                    depth: 1,
                };
                encoder.dispatchThreadgroups_threadsPerThreadgroup(one, one);
            }
            encoder.memoryBarrierWithScope(MTLBarrierScope::Buffers);
        })?;
        Ok(())
    }
}

/// Binary layouts from Metal's render/compute command encoder headers.
/// A disabled raster command has zero instances; a disabled compute/mesh
/// dispatch has zero X threadgroups, preventing guest shader side effects.
#[derive(Clone, Copy, Debug)]
pub enum ConditionalArgumentLayout {
    Draw,
    DrawIndexed,
    Dispatch,
}

impl ConditionalArgumentLayout {
    pub fn byte_size(self) -> usize {
        match self {
            Self::Draw => 16,
            Self::DrawIndexed => 20,
            Self::Dispatch => 12,
        }
    }

    fn disabled_word(self) -> u32 {
        match self {
            Self::Draw | Self::DrawIndexed => 1,
            Self::Dispatch => 0,
        }
    }
}

/// Native compute prerequisite for Eden's host conditional rendering commands.
/// Metal has no Vulkan-style begin/end conditional-render region: consumers
/// use separate, GPU-generated indirect arguments instead.
pub struct ConditionalRenderingArgumentsPass {
    pass: ComputePass,
}

impl ConditionalRenderingArgumentsPass {
    pub fn new(device: &MetalDevice) -> Result<Self, MetalComputePassError> {
        Ok(Self {
            pass: ComputePass::new(
                device,
                include_str!("../host_shaders/metal_conditional_arguments.metal"),
                "conditional_arguments",
            )?,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn resolve(
        &self,
        scheduler: &mut MetalScheduler,
        staging_pool: &mut MetalStagingBufferPool,
        predicate: &MetalBuffer,
        predicate_offset: usize,
        inverted: bool,
        source: &MetalBuffer,
        source_offset: usize,
        source_stride: u32,
        command_count: u32,
        layout: ConditionalArgumentLayout,
    ) -> Result<StagingBufferRef, MetalComputePassError> {
        let record_size = layout.byte_size();
        let source_size = if command_count == 0 {
            Some(0)
        } else {
            ((command_count - 1) as usize)
                .checked_mul(source_stride as usize)
                .and_then(|size| size.checked_add(record_size))
        };
        if predicate_offset % 4 != 0
            || source_offset % 4 != 0
            || source_stride % 4 != 0
            || (source_stride as usize) < record_size
            || predicate_offset
                .checked_add(4)
                .is_none_or(|end| end > predicate.length())
            || source_size
                .and_then(|size| source_offset.checked_add(size))
                .is_none_or(|end| end > source.length())
        {
            return Err(MetalComputePassError::ConditionalArgumentsRange);
        }
        let output_size = (command_count as usize)
            .checked_mul(record_size)
            .ok_or(MetalComputePassError::OutputSize)?;
        let output = staging_pool.request(
            scheduler,
            output_size.max(4),
            StagingBufferUsage::DeviceLocal,
            false,
        )?;
        if command_count == 0 {
            return Ok(output);
        }
        let params = [
            record_size as u32 / 4,
            layout.disabled_word(),
            source_stride / 4,
            command_count,
            u32::from(inverted),
        ];
        scheduler.request_outside_render_pass_operation_context();
        scheduler.with_compute_encoder(|encoder| {
            encoder.memoryBarrierWithScope(MTLBarrierScope::Buffers);
            encoder.setComputePipelineState(&self.pass.pipeline);
            // SAFETY: all source/destination records and predicate ranges were
            // checked. Each thread writes a disjoint record. setBytes copies
            // five initialized u32 fields; command buffers retain bound buffers.
            unsafe {
                encoder.setBuffer_offset_atIndex(Some(predicate.handle()), predicate_offset, 0);
                encoder.setBuffer_offset_atIndex(Some(source.handle()), source_offset, 1);
                encoder.setBuffer_offset_atIndex(Some(output.buffer.handle()), output.offset, 2);
                encoder.setBytes_length_atIndex(NonNull::from(&params).cast(), 20, 3);
                encoder.dispatchThreads_threadsPerThreadgroup(
                    MTLSize {
                        width: command_count as usize,
                        height: 1,
                        depth: 1,
                    },
                    MTLSize {
                        width: self.pass.pipeline.threadExecutionWidth(),
                        height: 1,
                        depth: 1,
                    },
                );
            }
            encoder.memoryBarrierWithScope(MTLBarrierScope::Buffers);
        })?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn visibility_reduction_preserves_u64_carries_partial_groups_and_gpu_writes() {
        let device = MetalDevice::new().unwrap();
        let pass = VisibilityResolvePass::new(&device).unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let accumulation = MetalBuffer::new(&device, 8).unwrap();
        accumulation.write(0, &0xffff_ffff_0000_0007u64.to_ne_bytes()).unwrap();
        let mut outputs = Vec::new();
        for count in [1, 255, 256, 257, 2048, 65537] {
            let words: Vec<u64> = (0..count).map(|index|
                if index % 3 == 0 { u64::MAX } else { 0x1000_0000_1u64 + index as u64 }).collect();
            let upload = MetalBuffer::new(&device, count * 8 + 16).unwrap();
            upload.write(0, &vec![0xa5; upload.length()]).unwrap();
            upload.write(8, bytemuck::cast_slice(&words)).unwrap();
            let source = MetalBuffer::new_private(&device, upload.length()).unwrap();
            upload.encode_copy(&mut scheduler, &source, 0, 0, upload.length()).unwrap();
            let value = pass.run(&mut scheduler, &source, 8, count, &accumulation).unwrap();
            let expected = words.into_iter().fold(0xffff_ffff_0000_0007u64, u64::wrapping_add);
            outputs.push((value, expected));
        }
        for (offset, count) in [(1, 1), (0, 0), (0, 2), (usize::MAX, 1)] {
            assert!(matches!(pass.run(&mut scheduler, &accumulation, offset, count, &accumulation),
                Err(MetalComputePassError::VisibilityRange)));
        }
        scheduler.finish_all().unwrap();
        for (value, expected) in outputs {
            let mut bytes = [0; 8];
            value.read(0, &mut bytes).unwrap();
            assert_eq!(u64::from_ne_bytes(bytes), expected);
        }
    }

    #[test]
    fn conditional_arguments_preserve_layouts_and_gpu_snapshots() {
        use objc2_metal::{
            MTLDispatchThreadgroupsIndirectArguments, MTLDrawIndexedPrimitivesIndirectArguments,
            MTLDrawPrimitivesIndirectArguments,
        };
        assert_eq!(
            ConditionalArgumentLayout::Draw.byte_size(),
            std::mem::size_of::<MTLDrawPrimitivesIndirectArguments>()
        );
        assert_eq!(
            ConditionalArgumentLayout::DrawIndexed.byte_size(),
            std::mem::size_of::<MTLDrawIndexedPrimitivesIndirectArguments>()
        );
        assert_eq!(
            ConditionalArgumentLayout::Dispatch.byte_size(),
            std::mem::size_of::<MTLDispatchThreadgroupsIndirectArguments>()
        );
        assert_eq!(
            std::mem::offset_of!(MTLDrawPrimitivesIndirectArguments, instanceCount),
            4
        );
        assert_eq!(
            std::mem::offset_of!(MTLDrawIndexedPrimitivesIndirectArguments, baseVertex),
            12
        );
        let device = MetalDevice::new().unwrap();
        let pass = ConditionalRenderingArgumentsPass::new(&device).unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut pool = MetalStagingBufferPool::new(&device).unwrap();
        let source = MetalBuffer::new_private(&device, 68).unwrap();
        let predicate = MetalBuffer::new_private(&device, 8).unwrap();
        let mut outputs = Vec::new();
        let tick = scheduler.current_tick();
        for (layout, records, disabled_word) in [
            (
                ConditionalArgumentLayout::Draw,
                [[3u32, 7, 0x80000001, 0xfffffff0, 0], [9, 0, 11, 13, 0]],
                1,
            ),
            (
                ConditionalArgumentLayout::DrawIndexed,
                [[3, 7, 11, u32::MAX, 0x80000000], [9, 2, 13, 0xfffffff0, 17]],
                1,
            ),
            (
                ConditionalArgumentLayout::Dispatch,
                [[3, 2, 5, 0, 0], [0, 7, 9, 0, 0]],
                0,
            ),
        ] {
            let record_size = layout.byte_size();
            let upload = MetalBuffer::new(&device, 68).unwrap();
            let mut original = vec![0xa5; 68];
            for (i, words) in records.iter().enumerate() {
                for (w, word) in words.iter().take(record_size / 4).enumerate() {
                    original[4 + i * 32 + w * 4..8 + i * 32 + w * 4]
                        .copy_from_slice(&word.to_ne_bytes());
                }
            }
            upload.write(0, &original).unwrap();
            upload
                .encode_copy(&mut scheduler, &source, 0, 0, 68)
                .unwrap();
            for (predicate_value, inverted, enabled) in [
                (0u32, false, false),
                (0, true, true),
                (0x80000000, false, true),
                (0x80000000, true, false),
            ] {
                let predicate_upload = MetalBuffer::new(&device, 4).unwrap();
                predicate_upload
                    .write(0, &predicate_value.to_ne_bytes())
                    .unwrap();
                predicate_upload
                    .encode_copy(&mut scheduler, &predicate, 0, 4, 4)
                    .unwrap();
                let output = pass
                    .resolve(
                        &mut scheduler,
                        &mut pool,
                        &predicate,
                        4,
                        inverted,
                        &source,
                        4,
                        32,
                        2,
                        layout,
                    )
                    .unwrap();
                let mut expected = Vec::new();
                for words in records {
                    for (i, word) in words.into_iter().take(record_size / 4).enumerate() {
                        let value = if !enabled && i == disabled_word {
                            0
                        } else {
                            word
                        };
                        expected.extend_from_slice(&value.to_ne_bytes());
                    }
                }
                assert_eq!(output.size, expected.len());
                assert!(
                    outputs
                        .iter()
                        .all(|(previous, _): &(StagingBufferRef, Vec<u8>)| !Arc::ptr_eq(
                            &previous.buffer,
                            &output.buffer
                        )),
                    "live arguments recycled"
                );
                outputs.push((output, expected));
            }
        }
        assert_eq!(scheduler.current_tick(), tick);
        let downloads: Vec<_> = outputs
            .iter()
            .map(|(output, expected)| {
                let download = MetalBuffer::new(&device, expected.len()).unwrap();
                output
                    .buffer
                    .encode_copy(&mut scheduler, &download, output.offset, 0, expected.len())
                    .unwrap();
                download
            })
            .collect();
        scheduler.finish_all().unwrap();
        for ((_, expected), download) in outputs.iter().zip(downloads) {
            let mut actual = vec![0; expected.len()];
            download.read(0, &mut actual).unwrap();
            assert_eq!(&actual, expected);
        }
    }

    #[test]
    fn conditional_dispatch_suppresses_shader_side_effects() {
        let device = MetalDevice::new().unwrap();
        let pass = ConditionalRenderingArgumentsPass::new(&device).unwrap();
        let counter_pass = ComputePass::new(
            &device,
            r#"
#include <metal_stdlib>
using namespace metal;
kernel void count_invocations(device atomic_uint* result [[buffer(0)]]) {
    atomic_fetch_add_explicit(result, 1u, memory_order_relaxed);
}
"#,
            "count_invocations",
        )
        .unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut pool = MetalStagingBufferPool::new(&device).unwrap();
        let predicate = MetalBuffer::new_private(&device, 4).unwrap();
        let source = MetalBuffer::new(&device, 12).unwrap();
        source
            .write(0, &bytemuck::cast_slice(&[3u32, 2, 1]))
            .unwrap();
        let counts = MetalBuffer::new(&device, 16).unwrap();
        counts.write(0, &[0; 16]).unwrap();
        let tick = scheduler.current_tick();
        for (i, (value, inverted)) in [(0u32, false), (0, true), (17, false), (17, true)]
            .into_iter()
            .enumerate()
        {
            let upload = MetalBuffer::new(&device, 4).unwrap();
            upload.write(0, &value.to_ne_bytes()).unwrap();
            upload
                .encode_copy(&mut scheduler, &predicate, 0, 0, 4)
                .unwrap();
            let output = pass
                .resolve(
                    &mut scheduler,
                    &mut pool,
                    &predicate,
                    0,
                    inverted,
                    &source,
                    0,
                    12,
                    1,
                    ConditionalArgumentLayout::Dispatch,
                )
                .unwrap();
            scheduler.with_compute_encoder(|encoder| unsafe {
                encoder.setComputePipelineState(&counter_pass.pipeline);
                encoder.setBuffer_offset_atIndex(Some(counts.handle()), i * 4, 0);
                encoder.dispatchThreadgroupsWithIndirectBuffer_indirectBufferOffset_threadsPerThreadgroup(
                    output.buffer.handle(), output.offset,
                    MTLSize {width: 1, height: 1, depth: 1});
                encoder.memoryBarrierWithScope(MTLBarrierScope::Buffers);
            }).unwrap();
        }
        assert_eq!(scheduler.current_tick(), tick);
        scheduler.finish_all().unwrap();
        let mut bytes = [0; 16];
        counts.read(0, &mut bytes).unwrap();
        let results: Vec<_> = bytes
            .chunks_exact(4)
            .map(|w| u32::from_ne_bytes(w.try_into().unwrap()))
            .collect();
        assert_eq!(results, [0, 6, 6, 0]);
    }

    #[test]
    fn conditional_arguments_reject_short_or_misaligned_records() {
        let device = MetalDevice::new().unwrap();
        let pass = ConditionalRenderingArgumentsPass::new(&device).unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut pool = MetalStagingBufferPool::new(&device).unwrap();
        let predicate = MetalBuffer::new_private(&device, 4).unwrap();
        let source = MetalBuffer::new_private(&device, 40).unwrap();
        for (pred_offset, src_offset, stride, count) in [
            (1, 0, 20, 1),
            (4, 0, 20, 1),
            (0, 1, 20, 1),
            (0, 0, 21, 1),
            (0, 0, 16, 2),
            (0, 0, 20, 3),
            (0, usize::MAX - 3, 20, 1),
        ] {
            assert!(matches!(
                pass.resolve(
                    &mut scheduler,
                    &mut pool,
                    &predicate,
                    pred_offset,
                    false,
                    &source,
                    src_offset,
                    stride,
                    count,
                    ConditionalArgumentLayout::DrawIndexed
                ),
                Err(MetalComputePassError::ConditionalArgumentsRange)
            ));
        }
        assert_eq!(scheduler.current_tick(), 1);
    }

    #[test]
    fn conditional_draws_suppress_vertex_side_effects_and_preserve_ids() {
        use objc2_metal::{
            MTLIndexType, MTLPixelFormat, MTLPrimitiveType, MTLRenderCommandEncoder,
            MTLRenderPassDescriptor, MTLRenderPipelineDescriptor, MTLTextureDescriptor,
            MTLTextureUsage,
        };
        let device = MetalDevice::new().unwrap();
        let pass = ConditionalRenderingArgumentsPass::new(&device).unwrap();
        let library = compile_msl_library(
            device.device(),
            r#"
#include <metal_stdlib>
using namespace metal;
vertex void count_vertices(device atomic_uint* result [[buffer(0)]],
    uint vertex_id [[vertex_id]], uint instance_id [[instance_id]]) {
    atomic_fetch_add_explicit(result, 1u, memory_order_relaxed);
    atomic_fetch_add_explicit(result + 1, vertex_id, memory_order_relaxed);
    atomic_fetch_add_explicit(result + 2, instance_id, memory_order_relaxed);
}
"#,
            MslVersion::V2_3,
        )
        .unwrap();
        let function = library
            .newFunctionWithName(&NSString::from_str("count_vertices"))
            .unwrap();
        let descriptor = MTLRenderPipelineDescriptor::new();
        descriptor.setVertexFunction(Some(&function));
        descriptor.setRasterizationEnabled(false);
        // SAFETY: attachment zero is valid and the one-pixel target has the
        // matching renderable format and nonzero dimensions.
        unsafe {
            descriptor
                .colorAttachments()
                .objectAtIndexedSubscript(0)
                .setPixelFormat(MTLPixelFormat::RGBA8Unorm);
        }
        let pipeline = device
            .device()
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .unwrap();
        let td = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::RGBA8Unorm,
                1,
                1,
                false,
            )
        };
        td.setUsage(MTLTextureUsage::RenderTarget);
        let texture = device.device().newTextureWithDescriptor(&td).unwrap();
        let render_pass = MTLRenderPassDescriptor::new();
        unsafe {
            render_pass
                .colorAttachments()
                .objectAtIndexedSubscript(0)
                .setTexture(Some(&texture));
        }
        let mut scheduler = MetalScheduler::new(&device);
        let mut pool = MetalStagingBufferPool::new(&device).unwrap();
        let predicate = MetalBuffer::new_private(&device, 4).unwrap();
        let counts = MetalBuffer::new(&device, 8 * 12).unwrap();
        counts.write(0, &[0; 8 * 12]).unwrap();
        let indices = MetalBuffer::new(&device, 16).unwrap();
        indices
            .write(0, bytemuck::cast_slice(&[999u32, 4, 5, 6]))
            .unwrap();
        let tick = scheduler.current_tick();
        for (layout_index, (layout, words)) in [
            (ConditionalArgumentLayout::Draw, vec![3u32, 2, 4, 7]),
            (
                ConditionalArgumentLayout::DrawIndexed,
                vec![3u32, 2, 1, (-4i32) as u32, 7],
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let source = MetalBuffer::new(&device, words.len() * 4).unwrap();
            source.write(0, bytemuck::cast_slice(&words)).unwrap();
            for (i, (value, inverted)) in [(0u32, false), (0, true), (17, false), (17, true)]
                .into_iter()
                .enumerate()
            {
                let upload = MetalBuffer::new(&device, 4).unwrap();
                upload.write(0, &value.to_ne_bytes()).unwrap();
                upload
                    .encode_copy(&mut scheduler, &predicate, 0, 0, 4)
                    .unwrap();
                let args = pass
                    .resolve(
                        &mut scheduler,
                        &mut pool,
                        &predicate,
                        0,
                        inverted,
                        &source,
                        0,
                        layout.byte_size() as u32,
                        1,
                        layout,
                    )
                    .unwrap();
                scheduler.begin_render_pass(&render_pass).unwrap();
                scheduler.with_render_encoder(|encoder| unsafe {
                    encoder.setRenderPipelineState(&pipeline);
                    encoder.setVertexBuffer_offset_atIndex(Some(counts.handle()),
                        (layout_index * 4 + i) * 12, 0);
                    match layout {
                        ConditionalArgumentLayout::Draw => {
                            encoder.drawPrimitives_indirectBuffer_indirectBufferOffset(
                                MTLPrimitiveType::Point, args.buffer.handle(), args.offset);
                        }
                        ConditionalArgumentLayout::DrawIndexed => {
                            encoder.drawIndexedPrimitives_indexType_indexBuffer_indexBufferOffset_indirectBuffer_indirectBufferOffset(
                                MTLPrimitiveType::Point, MTLIndexType::UInt32, indices.handle(), 0,
                                args.buffer.handle(), args.offset);
                        }
                        ConditionalArgumentLayout::Dispatch => unreachable!(),
                    }
                }).unwrap();
            }
        }
        assert_eq!(scheduler.current_tick(), tick);
        scheduler.finish_all().unwrap();
        let mut bytes = [0u8; 8 * 12];
        counts.read(0, &mut bytes).unwrap();
        let results: Vec<_> = bytes
            .chunks_exact(4)
            .map(|w| u32::from_ne_bytes(w.try_into().unwrap()))
            .collect();
        assert_eq!(
            results,
            [0, 0, 0, 6, 30, 45, 6, 30, 45, 0, 0, 0, 0, 0, 0, 6, 6, 45, 6, 6, 45, 0, 0, 0,]
        );
    }

    #[test]
    fn conditional_resolve_preserves_gpu_producers_and_compares_both_words() {
        let device = MetalDevice::new().unwrap();
        let pass = ConditionalRenderingResolvePass::new(&device).unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        // The middle two words are not part of either comparison.
        let cases: [([u32; 6], bool, u32); 9] = [
            ([0, 0, 7, 8, 0, 0], true, 0),
            ([1, 0, 7, 8, 0, 0], true, 0),
            ([0, 1, 7, 8, 0, 0], true, 0),
            ([1, 1, 7, 8, 0, 0], true, 1),
            ([u32::MAX, 0x80000000, 7, 8, 0, 0], true, 1),
            ([0, 0, 7, 8, 0, 0], false, 1),
            ([u32::MAX, 0x80000000, 7, 8, u32::MAX, 0x80000000], false, 1),
            ([1, 2, 7, 8, 3, 2], false, 0),
            ([1, 2, 7, 8, 1, 3], false, 0),
        ];
        let source = MetalBuffer::new_private(&device, 28).unwrap();
        let destination = MetalBuffer::new_private(&device, 64).unwrap();
        let initial = MetalBuffer::new(&device, 64).unwrap();
        initial.write(0, &[0xa5; 64]).unwrap();
        initial
            .encode_copy(&mut scheduler, &destination, 0, 0, 64)
            .unwrap();
        let tick = scheduler.current_tick();
        for (i, (words, compare_to_zero, _)) in cases.iter().enumerate() {
            let upload = MetalBuffer::new(&device, 24).unwrap();
            let bytes: Vec<_> = words.iter().flat_map(|v| v.to_ne_bytes()).collect();
            upload.write(0, &bytes).unwrap();
            upload
                .encode_copy(&mut scheduler, &source, 0, 4, 24)
                .unwrap();
            pass.resolve(
                &mut scheduler,
                &destination,
                4 + i * 4,
                &source,
                4,
                *compare_to_zero,
            )
            .unwrap();
            // Dropping upload here checks command-buffer ownership as well.
        }
        // The preceding dispatch replaces 0xa5a5a5a5 at offset 36 with zero.
        // Consume that write in the same compute encoder, with a nonzero high
        // word at offset 40: reading the pre-dispatch bytes would return 1.
        pass.resolve(&mut scheduler, &destination, 48, &destination, 36, true)
            .unwrap();
        assert_eq!(
            scheduler.current_tick(),
            tick,
            "resolve must not submit or wait"
        );
        let download = MetalBuffer::new(&device, 64).unwrap();
        destination
            .encode_copy(&mut scheduler, &download, 0, 0, 64)
            .unwrap();
        scheduler.finish_all().unwrap();
        let mut bytes = [0u8; 64];
        download.read(0, &mut bytes).unwrap();
        for (i, (_, _, expected)) in cases.iter().enumerate() {
            let offset = 4 + i * 4;
            assert_eq!(
                u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap()),
                *expected,
                "case {i}"
            );
        }
        assert_eq!(&bytes[48..52], &0u32.to_ne_bytes());
        assert_eq!(&bytes[..4], &[0xa5; 4]);
        assert_eq!(&bytes[40..48], &[0xa5; 8]);
        assert_eq!(&bytes[52..], &[0xa5; 12]);
    }

    #[test]
    fn conditional_resolve_rejects_invalid_ranges_before_recording() {
        let device = MetalDevice::new().unwrap();
        let pass = ConditionalRenderingResolvePass::new(&device).unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let source = MetalBuffer::new_private(&device, 24).unwrap();
        let destination = MetalBuffer::new_private(&device, 4).unwrap();
        for (src, dst, zero) in [
            (1, 0, true),
            (20, 0, true),
            (4, 0, false),
            (usize::MAX - 3, 0, false),
            (0, 1, false),
            (0, 4, true),
        ] {
            assert!(matches!(
                pass.resolve(&mut scheduler, &destination, dst, &source, src, zero),
                Err(MetalComputePassError::ConditionalRange)
            ));
        }
        assert_eq!(scheduler.current_tick(), 1);
    }

    #[test]
    fn uint8_conversion_follows_gpu_writers_without_flushing_or_reusing_live_outputs() {
        let device = MetalDevice::new().unwrap();
        let pass = Uint8Pass::new(&device).unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut pool = MetalStagingBufferPool::new(&device).unwrap();
        let upload = MetalBuffer::new(&device, 256).unwrap();
        let values: Vec<_> = (0..=255u8).collect();
        upload.write(0, &values).unwrap();
        let source = MetalBuffer::new_private(&device, 257).unwrap();
        upload
            .encode_copy(&mut scheduler, &source, 0, 1, 256)
            .unwrap();
        let tick = scheduler.current_tick();
        let first = pass
            .assemble(&mut scheduler, &mut pool, 256, &source, 1)
            .unwrap();
        let second = pass
            .assemble(&mut scheduler, &mut pool, 256, &source, 1)
            .unwrap();
        assert_eq!(
            scheduler.current_tick(),
            tick,
            "conversion must not submit the source producer"
        );
        assert!(
            !Arc::ptr_eq(&first.buffer, &second.buffer),
            "live staging output was reused"
        );
        let download = MetalBuffer::new(&device, 1024).unwrap();
        first
            .buffer
            .encode_copy(&mut scheduler, &download, first.offset, 0, 512)
            .unwrap();
        second
            .buffer
            .encode_copy(&mut scheduler, &download, second.offset, 512, 512)
            .unwrap();
        scheduler.finish_all().unwrap();
        let mut bytes = [0u8; 1024];
        download.read(0, &mut bytes).unwrap();
        for (i, value) in bytes.chunks_exact(2).enumerate() {
            assert_eq!(
                u16::from_ne_bytes(value.try_into().unwrap()),
                if i % 256 == 255 {
                    65535
                } else {
                    (i % 256) as u16
                }
            );
        }
    }

    #[test]
    fn quad_conversion_matches_upstream_swizzles_widths_offsets_and_wrapping_base() {
        let device = MetalDevice::new().unwrap();
        let pass = QuadIndexedPass::new(&device).unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut pool = MetalStagingBufferPool::new(&device).unwrap();
        for format in [
            IndexFormat::UnsignedByte,
            IndexFormat::UnsignedShort,
            IndexFormat::UnsignedInt,
        ] {
            let width = format.size_bytes() as usize;
            let mask = match width {
                1 => 0xff,
                2 => 0xffff,
                _ => u32::MAX,
            };
            let values = [
                0x01020304u32,
                0xffffffff,
                0,
                0x12345678,
                0x80808080,
                0x100,
                0x7fff,
                1,
            ]
            .map(|value| value & mask);
            let bytes: Vec<_> = values
                .iter()
                .flat_map(|value| value.to_le_bytes()[..width].to_vec())
                .collect();
            let upload = MetalBuffer::new(&device, bytes.len()).unwrap();
            upload.write(0, &bytes).unwrap();
            let source = MetalBuffer::new_private(&device, bytes.len() + 3).unwrap();
            upload
                .encode_copy(&mut scheduler, &source, 0, 3, bytes.len())
                .unwrap();
            for (strip, order) in [
                (false, vec![0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7]),
                (
                    true,
                    vec![0, 3, 1, 0, 2, 3, 2, 5, 3, 2, 4, 5, 4, 7, 5, 4, 6, 7],
                ),
            ] {
                let tick = scheduler.current_tick();
                let output = pass
                    .assemble(
                        &mut scheduler,
                        &mut pool,
                        format,
                        8,
                        0xfffffffe,
                        &source,
                        3,
                        strip,
                    )
                    .unwrap();
                assert_eq!(scheduler.current_tick(), tick);
                let download = MetalBuffer::new(&device, order.len() * 4).unwrap();
                output
                    .buffer
                    .encode_copy(&mut scheduler, &download, output.offset, 0, order.len() * 4)
                    .unwrap();
                scheduler.finish_all().unwrap();
                let mut actual = vec![0u8; order.len() * 4];
                download.read(0, &mut actual).unwrap();
                for (bytes, input) in actual.chunks_exact(4).zip(order) {
                    assert_eq!(
                        u32::from_ne_bytes(bytes.try_into().unwrap()),
                        values[input].wrapping_add(0xfffffffe),
                        "format={format:?}, strip={strip}"
                    );
                }
            }
        }
    }

    #[test]
    fn index_conversions_cross_threadgroup_boundaries() {
        let device = MetalDevice::new().unwrap();
        let uint8 = Uint8Pass::new(&device).unwrap();
        let quad = QuadIndexedPass::new(&device).unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut pool = MetalStagingBufferPool::new(&device).unwrap();
        // 1025 complete quads plus an unused trailing index; neither dispatch
        // size is a multiple of the upstream 1024-thread group size.
        let count = 4101u32;
        let values: Vec<u8> = (0..count).map(|i| i as u8).collect();
        let source = MetalBuffer::new(&device, values.len()).unwrap();
        source.write(0, &values).unwrap();
        let expanded = uint8
            .assemble(&mut scheduler, &mut pool, count, &source, 0)
            .unwrap();
        let triangles = quad
            .assemble(
                &mut scheduler,
                &mut pool,
                IndexFormat::UnsignedByte,
                count,
                17,
                &source,
                0,
                false,
            )
            .unwrap();
        let expanded_size = count as usize * 2;
        let triangles_size = (count / 4 * 6) as usize * 4;
        let download = MetalBuffer::new(&device, expanded_size + triangles_size).unwrap();
        expanded
            .buffer
            .encode_copy(&mut scheduler, &download, expanded.offset, 0, expanded_size)
            .unwrap();
        triangles
            .buffer
            .encode_copy(
                &mut scheduler,
                &download,
                triangles.offset,
                expanded_size,
                triangles_size,
            )
            .unwrap();
        scheduler.finish_all().unwrap();
        let mut bytes = vec![0; download.length()];
        download.read(0, &mut bytes).unwrap();
        for (actual, expected) in bytes[..expanded_size].chunks_exact(2).zip(&values) {
            assert_eq!(
                u16::from_ne_bytes(actual.try_into().unwrap()),
                if *expected == 255 {
                    65535
                } else {
                    u16::from(*expected)
                },
            );
        }
        for (primitive, actual) in bytes[expanded_size..].chunks_exact(24).enumerate() {
            for (value, vertex) in actual.chunks_exact(4).zip([0, 1, 2, 0, 2, 3]) {
                assert_eq!(
                    u32::from_ne_bytes(value.try_into().unwrap()),
                    u32::from(values[primitive * 4 + vertex]) + 17,
                );
            }
        }
    }

    #[test]
    fn index_passes_reject_invalid_ranges_and_preserve_empty_draws() {
        let device = MetalDevice::new().unwrap();
        let uint8 = Uint8Pass::new(&device).unwrap();
        let quad = QuadIndexedPass::new(&device).unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut pool = MetalStagingBufferPool::new(&device).unwrap();
        let source = MetalBuffer::new(&device, 4).unwrap();
        let tick = scheduler.current_tick();
        assert!(matches!(
            uint8.assemble(&mut scheduler, &mut pool, 4, &source, 1),
            Err(MetalComputePassError::SourceRange)
        ));
        assert!(matches!(
            quad.assemble(
                &mut scheduler,
                &mut pool,
                IndexFormat::UnsignedInt,
                4,
                0,
                &source,
                0,
                false
            ),
            Err(MetalComputePassError::SourceRange)
        ));
        assert!(matches!(
            uint8.assemble(&mut scheduler, &mut pool, 0, &source, usize::MAX),
            Err(MetalComputePassError::SourceRange)
        ));
        uint8
            .assemble(&mut scheduler, &mut pool, 0, &source, 4)
            .unwrap();
        for count in 0..=3 {
            quad.assemble(
                &mut scheduler,
                &mut pool,
                IndexFormat::UnsignedByte,
                count,
                0,
                &source,
                0,
                false,
            )
            .unwrap();
            quad.assemble(
                &mut scheduler,
                &mut pool,
                IndexFormat::UnsignedByte,
                count,
                0,
                &source,
                0,
                true,
            )
            .unwrap();
        }
        assert_eq!(scheduler.current_tick(), tick);
        assert_eq!(scheduler.known_gpu_tick().unwrap(), 0);
    }
}
