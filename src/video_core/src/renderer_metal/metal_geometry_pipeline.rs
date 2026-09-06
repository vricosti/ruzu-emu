// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Preceding-stage execution and payload production for native geometry meshes.
//! Eden executes these operations with fixed-function vertex/geometry stages.
//! MSL shares the same guest IR function, executing each input-stream entry once
//! before object shaders reuse its output for their assembled primitive.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBarrierScope, MTLComputeCommandEncoder, MTLComputePipelineState, MTLDevice, MTLFunction,
    MTLLibrary, MTLMeshRenderPipelineDescriptor, MTLPipelineOption, MTLRenderCommandEncoder,
    MTLRenderPipelineState, MTLSize,
};
use shader_recompiler::backend::msl::{
    emit_msl_geometry::GeometryLayout,
    msl_function::{MslFunctionKind, MslParameterAttribute},
    MslShaderArtifact, MslVersion,
};
use shader_recompiler::runtime_info::RuntimeInfo;
use thiserror::Error;

use super::metal_buffer::{MetalBuffer, MetalBufferError};
use super::metal_compute_pass::{
    ConditionalArgumentLayout, ConditionalRenderingArgumentsPass, MetalComputePassError,
};
use super::metal_device::MetalDevice;
use super::metal_graphics_pipeline::MetalPreparedStage;
use super::metal_pipeline_cache::{MetalRenderPipelineKey, MetalVertexInputState};
use super::metal_primitive_assembler::MetalPrimitiveAssembly;
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};
use super::metal_staging_buffer_pool::MetalStagingBufferPool;
use super::metal_shader::{compile_msl_library, MetalShaderError, MetalShaderModule};
use super::metal_geometry_capture::{MetalGeometryCapture, MetalGeometryCapturedDraw};

pub enum MetalGeometryShader {
    Mesh(Arc<MetalShaderModule>),
    Capture(MslShaderArtifact),
}

impl MetalGeometryShader {
    pub fn bindings(&self) -> &shader_recompiler::backend::msl::MslBindingLayout {
        match self { Self::Mesh(shader) => shader.bindings(), Self::Capture(shader) => &shader.bindings }
    }
    pub fn language_version(&self) -> MslVersion {
        match self { Self::Mesh(shader) => shader.language_version(), Self::Capture(shader) => shader.language_version }
    }
}

pub struct MetalGeometryShaderStages {
    pub vertex: MslShaderArtifact,
    pub shader: MetalGeometryShader,
    pub runtime: RuntimeInfo,
    pub layout: GeometryLayout,
    pub invocations: u32,
}

/// Native counterpart of the geometry-bearing graphics PSO. The vertex producer
/// is keyed by the same vertex formats/strides as its associated mesh pipeline.
pub struct MetalGeometryPipeline {
    pub vertex: MetalGeometryVertexPipeline,
    output: MetalGeometryOutput,
    pub state: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
}

enum MetalGeometryOutput {
    Mesh(MetalGeometryObject),
    Capture(MetalGeometryCapture),
}

impl MetalGeometryPipeline {
    pub fn new(
        device: &MetalDevice,
        key: &MetalRenderPipelineKey,
        stages: &MetalGeometryShaderStages,
        fragment: Option<&MetalShaderModule>,
    ) -> Result<Self, MetalGeometryPipelineError> {
        let vertex = MetalGeometryVertexPipeline::new(
            device,
            &stages.vertex,
            &key.vertex_input,
            &stages.runtime,
        )?;
        let output = match &stages.shader {
            MetalGeometryShader::Mesh(shader) => MetalGeometryOutput::Mesh(MetalGeometryObject::new(
                device, &stages.layout, &stages.runtime, stages.invocations, shader.language_version())?),
            MetalGeometryShader::Capture(artifact) => MetalGeometryOutput::Capture(MetalGeometryCapture::new(
                device, artifact, &stages.layout, &stages.runtime, stages.invocations)?),
        };
        let descriptor = MTLMeshRenderPipelineDescriptor::new();
        // SAFETY: functions belong to this device, and the cache validates the
        // framebuffer sample count before constructing this pipeline.
        unsafe {
            match (&output, &stages.shader) {
                (MetalGeometryOutput::Mesh(object), MetalGeometryShader::Mesh(shader)) => {
                    descriptor.setObjectFunction(Some(object.function()));
                    descriptor.setMeshFunction(Some(shader.function()));
                    descriptor.setPayloadMemoryLength(stages.layout.payload_size(&stages.runtime));
                }
                (MetalGeometryOutput::Capture(capture), _) => {
                    descriptor.setObjectFunction(Some(&capture.object));
                    descriptor.setMeshFunction(Some(&capture.mesh));
                    descriptor.setPayloadMemoryLength(16);
                }
                _ => return Err(MetalGeometryPipelineError::Layout),
            }
            descriptor.setFragmentFunction(
                fragment
                    .filter(|_| key.rasterization_enabled)
                    .map(MetalShaderModule::function),
            );
            descriptor.setRasterSampleCount(key.sample_count as usize);
        }
        descriptor.setMaxTotalThreadsPerObjectThreadgroup(1);
        descriptor.setMaxTotalThreadsPerMeshThreadgroup(1);
        descriptor.setAlphaToCoverageEnabled(key.alpha_to_coverage);
        descriptor.setAlphaToOneEnabled(key.alpha_to_one);
        descriptor.setRasterizationEnabled(key.rasterization_enabled);
        descriptor.setDepthAttachmentPixelFormat(key.depth_format);
        descriptor.setStencilAttachmentPixelFormat(key.stencil_format);
        let attachments = descriptor.colorAttachments();
        for (index, state) in key.color_attachments.iter().enumerate() {
            let attachment = unsafe { attachments.objectAtIndexedSubscript(index) };
            attachment.setPixelFormat(state.format);
            attachment.setBlendingEnabled(state.blending_enabled);
            attachment.setSourceRGBBlendFactor(state.source_rgb);
            attachment.setDestinationRGBBlendFactor(state.destination_rgb);
            attachment.setRgbBlendOperation(state.rgb_operation);
            attachment.setSourceAlphaBlendFactor(state.source_alpha);
            attachment.setDestinationAlphaBlendFactor(state.destination_alpha);
            attachment.setAlphaBlendOperation(state.alpha_operation);
            attachment.setWriteMask(state.write_mask);
        }
        let state = device
            .device()
            .newRenderPipelineStateWithMeshDescriptor_options_reflection_error(
                &descriptor,
                MTLPipelineOption::empty(),
                None,
            )
            .map_err(|error| MetalGeometryPipelineError::Pipeline(error.to_string()))?;
        Ok(Self {
            vertex,
            output,
            state,
        })
    }

    /// Metal's replacement for an upstream conditional-render region must gate
    /// both guest stages, including their stores. Keep the original assembly
    /// untouched so a later enabled draw can reuse it.
    #[allow(clippy::too_many_arguments)]
    pub fn record_conditional_inputs(
        &self,
        scheduler: &mut MetalScheduler,
        staging_pool: &mut MetalStagingBufferPool,
        arguments_pass: &ConditionalRenderingArgumentsPass,
        predicate: &MetalBuffer,
        predicate_offset: usize,
        inverted: bool,
        assembly: &MetalPrimitiveAssembly,
        base_instance: u32,
        vertex_sizes: &[u64; 31],
        bind_resources: impl FnOnce(&ProtocolObject<dyn MTLComputeCommandEncoder>),
    ) -> Result<(MetalPrimitiveAssembly, MetalGeometryVertices), MetalGeometryPipelineError> {
        let conditional = arguments_pass.resolve(
            scheduler, staging_pool, predicate, predicate_offset, inverted,
            &assembly.dispatch_arguments, 0, 12, 1, ConditionalArgumentLayout::Dispatch,
        )?;
        // Assembly consumers use an owned argument buffer at offset zero. Do not
        // retain a pool slice beyond its scheduler tick or assume its offset is zero.
        let dispatch_arguments = Arc::new(MetalBuffer::new_private(&self.vertex.device, 12)?);
        conditional.buffer.encode_copy(
            scheduler, &dispatch_arguments, conditional.offset, 0, 12,
        )?;
        let mut conditional_assembly = assembly.clone();
        conditional_assembly.dispatch_arguments = dispatch_arguments;

        let params = assembly.params();
        let groups = [
            (params.count as usize).div_ceil(self.vertex.pipeline.threadExecutionWidth()) as u32,
            params.instances,
            1,
        ];
        let source = MetalBuffer::new(&self.vertex.device, 12)?;
        source.write(0, bytemuck::cast_slice(&groups))?;
        let vertex_arguments = arguments_pass.resolve(
            scheduler, staging_pool, predicate, predicate_offset, inverted,
            &source, 0, 12, 1, ConditionalArgumentLayout::Dispatch,
        )?;
        let vertices = self.vertex.record_indirect(
            scheduler, &conditional_assembly, base_instance, vertex_sizes,
            &vertex_arguments.buffer, vertex_arguments.offset, bind_resources,
        )?;
        Ok((conditional_assembly, vertices))
    }

    pub fn capture_output(&self, scheduler: &mut MetalScheduler, assembly: &MetalPrimitiveAssembly,
        vertices: &MetalGeometryVertices, resources: &MetalPreparedStage)
        -> Result<Option<MetalGeometryCapturedDraw>, MetalGeometryPipelineError> {
        match &self.output {
            MetalGeometryOutput::Mesh(_) => Ok(None),
            MetalGeometryOutput::Capture(capture) => capture.record(scheduler, assembly, vertices, resources).map(Some),
        }
    }

    pub fn record_draw(&self, encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
        assembly: &MetalPrimitiveAssembly, vertices: &MetalGeometryVertices,
        resources: &MetalPreparedStage, captured: Option<&MetalGeometryCapturedDraw>)
        -> Result<(), MetalGeometryPipelineError> {
        match (&self.output, captured) {
            (MetalGeometryOutput::Mesh(object), None) => {
                bind_geometry_resources(encoder, resources);
                object.record_draw(encoder, assembly, vertices)
            }
            (MetalGeometryOutput::Capture(capture), Some(draw)) => {
                capture.record_draw(encoder, draw);
                Ok(())
            }
            _ => Err(MetalGeometryPipelineError::Layout),
        }
    }
}

/// Bind the exact resources prepared for the guest vertex stage to its compute
/// execution. No guest reads or second descriptor acquisition occur here.
pub fn bind_vertex_resources(
    encoder: &ProtocolObject<dyn MTLComputeCommandEncoder>,
    stage: &MetalPreparedStage,
) {
    unsafe {
        for b in &stage.buffers {
            encoder.setBuffer_offset_atIndex(Some(b.buffer.handle()), b.offset, b.index as usize);
        }
        for t in &stage.textures {
            encoder.setTexture_atIndex(t.texture.as_deref(), t.index as usize);
        }
        for s in stage.samplers.iter().filter(|_| !stage.samplers_in_argument_buffer) {
            encoder.setSamplerState_atIndex(Some(&s.sampler), s.index as usize);
        }
        if let Some((index, bytes)) = &stage.push_constants {
            encoder.setBytes_length_atIndex(
                NonNull::new(bytes.as_ptr() as *mut c_void).unwrap(),
                bytes.len(),
                *index as usize,
            );
        }
    }
}

pub fn bind_geometry_resources(
    encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    stage: &MetalPreparedStage,
) {
    unsafe {
        for b in &stage.buffers {
            encoder.setMeshBuffer_offset_atIndex(
                Some(b.buffer.handle()),
                b.offset,
                b.index as usize,
            );
        }
        for t in &stage.textures {
            encoder.setMeshTexture_atIndex(t.texture.as_deref(), t.index as usize);
        }
        for s in stage.samplers.iter().filter(|_| !stage.samplers_in_argument_buffer) {
            encoder.setMeshSamplerState_atIndex(Some(&s.sampler), s.index as usize);
        }
        if let Some((index, bytes)) = &stage.push_constants {
            encoder.setMeshBytes_length_atIndex(
                NonNull::new(bytes.as_ptr() as *mut c_void).unwrap(),
                bytes.len(),
                *index as usize,
            );
        }
    }
}
use super::metal_vertex_pulling::{VertexPullingLayout, VertexPullingLayoutError};

#[derive(Debug, Error)]
pub enum MetalGeometryPipelineError {
    #[error("geometry producer requires a direct callable vertex artifact")]
    VertexInterface,
    #[error("geometry producer cannot forward builtin {0}")]
    Builtin(&'static str),
    #[error("geometry vertex input layout does not match its stage-in interface")]
    VertexInput,
    #[error("geometry producer exceeds the 31 Metal buffer slots")]
    BufferSlots,
    #[error("geometry vertex output size overflows")]
    OutputSize,
    #[error("geometry vertex indirect arguments are misaligned or out of bounds")]
    IndirectRange,
    #[error("geometry producer and consumer layouts do not match")]
    Layout,
    #[error("invalid geometry invocation count {0}")]
    Invocations(u32),
    #[error("geometry compute pipeline creation failed: {0}")]
    Pipeline(String),
    #[error(transparent)]
    Shader(#[from] MetalShaderError),
    #[error(transparent)]
    Buffer(#[from] MetalBufferError),
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
    #[error(transparent)]
    VertexFetch(#[from] VertexPullingLayoutError),
    #[error(transparent)]
    Conditional(#[from] MetalComputePassError),
}

pub struct MetalGeometryVertices {
    pub buffer: Arc<MetalBuffer>,
    stride: usize,
    count: u32,
    instances: u32,
    base_instance: u32,
    generic_mask: u32,
}

impl MetalGeometryVertices {
    pub(super) fn validate(&self, assembly: &MetalPrimitiveAssembly, stride: usize,
        generic_mask: u32, input_vertices: u32) -> Result<(), MetalGeometryPipelineError> {
        let p = assembly.params();
        if self.stride != stride || self.generic_mask != generic_mask || self.count != p.count
            || self.instances != p.instances || assembly.input_topology.vertices() != input_vertices {
            return Err(MetalGeometryPipelineError::Layout);
        }
        Ok(())
    }
}

pub struct MetalGeometryVertexPipeline {
    device: MetalDevice,
    pipeline: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    extra_buffer_base: u32,
    stride: usize,
    generic_mask: u32,
}

impl MetalGeometryVertexPipeline {
    pub fn new(
        device: &MetalDevice,
        vertex: &MslShaderArtifact,
        input: &MetalVertexInputState,
        geometry_runtime: &RuntimeInfo,
    ) -> Result<Self, MetalGeometryPipelineError> {
        super::metal_shader::validate_native_binding_layout(device.profile(), &vertex.bindings)?;
        let interface = vertex
            .interface
            .as_ref()
            .filter(|i| i.kind == MslFunctionKind::VertexFunction)
            .ok_or(MetalGeometryPipelineError::VertexInterface)?;
        let pulling = VertexPullingLayout::new(input)?;
        let has_input = interface
            .parameters
            .iter()
            .any(|p| p.attribute == MslParameterAttribute::Builtin("stage_in"));
        if has_input != pulling.is_some() {
            return Err(MetalGeometryPipelineError::VertexInput);
        }
        let mut parameters = Vec::new();
        let mut arguments = Vec::new();
        for parameter in &interface.parameters {
            let argument = match parameter.attribute {
                MslParameterAttribute::Builtin("stage_in") => "pulled_input".to_owned(),
                MslParameterAttribute::Builtin("vertex_id") => "input_vertex_id".to_owned(),
                MslParameterAttribute::Builtin("instance_id") => "input_instance_id".to_owned(),
                MslParameterAttribute::Builtin("base_vertex") => "p.base_vertex".to_owned(),
                MslParameterAttribute::Builtin("base_instance") => "p.base_instance".to_owned(),
                MslParameterAttribute::Builtin("thread_index_in_simdgroup") => {
                    parameters.push(parameter.declaration(MslFunctionKind::StageEntryPoint));
                    parameter.name.clone()
                }
                MslParameterAttribute::Builtin(name) => {
                    return Err(MetalGeometryPipelineError::Builtin(name))
                }
                _ => {
                    parameters.push(parameter.declaration(MslFunctionKind::StageEntryPoint));
                    parameter.name.clone()
                }
            };
            arguments.push(argument);
        }
        let mut extra_buffer_base = vertex.bindings.buffer_count;
        let mut pulling_source = String::new();
        let mut pull = String::new();
        if let Some(pulling) = pulling {
            let mut buffer_arguments = Vec::new();
            for buffer in pulling.buffers {
                extra_buffer_base = extra_buffer_base.max(u32::from(buffer.index) + 1);
                parameters.push(format!(
                    "{} [[buffer({})]]",
                    buffer.parameter(),
                    buffer.index
                ));
                buffer_arguments.push(buffer.name());
                buffer_arguments.push(format!("p.vertex_sizes[{}]", buffer.index));
            }
            pulling_source = pulling.source;
            pull = format!("MslVertexIn pulled_input = ruzu_pull_vertex(input_vertex_id, input_instance_id, p.base_instance, {});", buffer_arguments.join(", "));
        }
        if extra_buffer_base > 27 {
            return Err(MetalGeometryPipelineError::BufferSlots);
        }
        let slots = extra_buffer_base;
        parameters.extend([
            format!("const device uint* vertex_ids [[buffer({slots})]]"),
            format!("const device uint2* segments [[buffer({})]]", slots + 1),
            format!(
                "device MslGeometryVertexIn* outputs [[buffer({})]]",
                slots + 2
            ),
            format!(
                "constant GeometryVertexDrawParams& p [[buffer({})]]",
                slots + 3
            ),
            "uint3 entry [[thread_position_in_grid]]".into(),
        ]);
        let mut stores = String::from("outputs[output_index].position = result.position;\n");
        for i in 0..32 {
            if geometry_runtime.previous_stage_stores.generic_any(i) {
                stores.push_str(&format!(
                    "outputs[output_index].in_attr{i} = result.out_attr{i};\n"
                ));
            }
        }
        let source = format!(
            r#"
{vertex_source}
{pulling_source}
{vertex_declaration}
{draw_params}
kernel void geometry_vertices({parameters}) {{
    if (entry.x >= p.count || entry.y >= p.instances) return;
    if (segments[entry.x].x == entry.x+1u) return;
    uint input_vertex_id = vertex_ids[entry.x];
    uint input_instance_id = p.base_instance + entry.y;
    {pull}
    MslVertexOut result = {vertex_entry}({arguments});
    ulong output_index = ulong(entry.y)*p.count+entry.x;
    {stores}
}}
"#,
            vertex_source = vertex.source.source,
            vertex_declaration = GeometryLayout::vertex_declaration(geometry_runtime),
            draw_params = VERTEX_DRAW_PARAMS,
            parameters = parameters.join(", "),
            vertex_entry = vertex.entry_point,
            arguments = arguments.join(", ")
        );
        let library = compile_msl_library(device.device(), &source, vertex.language_version)?;
        let function = library
            .newFunctionWithName(&NSString::from_str("geometry_vertices"))
            .ok_or_else(|| MetalShaderError::MissingEntryPoint("geometry_vertices".into()))?;
        let pipeline = device
            .device()
            .newComputePipelineStateWithFunction_error(&function)
            .map_err(|e| MetalGeometryPipelineError::Pipeline(e.to_string()))?;
        Ok(Self {
            device: device.clone(),
            pipeline,
            extra_buffer_base,
            stride: GeometryLayout::vertex_stride(geometry_runtime),
            generic_mask: GeometryLayout::generic_mask(geometry_runtime),
        })
    }

    /// The binding closure forwards prepared native vertex resources. It must not
    /// re-read guest memory; resources were captured by graphics configuration.
    pub fn record(
        &self,
        scheduler: &mut MetalScheduler,
        assembly: &MetalPrimitiveAssembly,
        base_instance: u32,
        vertex_sizes: &[u64; 31],
        bind_resources: impl FnOnce(&ProtocolObject<dyn MTLComputeCommandEncoder>),
    ) -> Result<MetalGeometryVertices, MetalGeometryPipelineError> {
        self.record_impl(scheduler, assembly, base_instance, vertex_sizes, None, bind_resources)
    }

    /// The grid uses threadgroups (not threads); the generated vertex entry point
    /// retains its count/instance bounds checks for the rounded final group.
    #[allow(clippy::too_many_arguments)]
    pub fn record_indirect(
        &self,
        scheduler: &mut MetalScheduler,
        assembly: &MetalPrimitiveAssembly,
        base_instance: u32,
        vertex_sizes: &[u64; 31],
        arguments: &MetalBuffer,
        offset: usize,
        bind_resources: impl FnOnce(&ProtocolObject<dyn MTLComputeCommandEncoder>),
    ) -> Result<MetalGeometryVertices, MetalGeometryPipelineError> {
        if offset % 4 != 0 || offset.checked_add(12).is_none_or(|end| end > arguments.length()) {
            return Err(MetalGeometryPipelineError::IndirectRange);
        }
        self.record_impl(scheduler, assembly, base_instance, vertex_sizes,
            Some((arguments, offset)), bind_resources)
    }

    fn record_impl(
        &self,
        scheduler: &mut MetalScheduler,
        assembly: &MetalPrimitiveAssembly,
        base_instance: u32,
        vertex_sizes: &[u64; 31],
        indirect: Option<(&MetalBuffer, usize)>,
        bind_resources: impl FnOnce(&ProtocolObject<dyn MTLComputeCommandEncoder>),
    ) -> Result<MetalGeometryVertices, MetalGeometryPipelineError> {
        let params = assembly.params();
        let size = (params.count as usize)
            .checked_mul(params.instances as usize)
            .and_then(|n| n.checked_mul(self.stride))
            .ok_or(MetalGeometryPipelineError::OutputSize)?;
        let buffer = Arc::new(MetalBuffer::new_private(&self.device, size)?);
        if params.count != 0 && params.instances != 0 {
            let mut words = [0u32; 66];
            words[..4].copy_from_slice(&[
                params.count,
                params.instances,
                params.base_vertex as u32,
                base_instance,
            ]);
            for (size, pair) in vertex_sizes.iter().zip(words[4..].chunks_exact_mut(2)) {
                pair[0] = *size as u32;
                pair[1] = (*size >> 32) as u32;
            }
            scheduler.with_compute_encoder(|encoder| {
                encoder.setComputePipelineState(&self.pipeline);
                bind_resources(encoder);
                // SAFETY: each buffer is retained through the command buffer; parameters are copied.
                unsafe {
                    let slot = self.extra_buffer_base as usize;
                    encoder.setBuffer_offset_atIndex(Some(assembly.vertex_ids.handle()), 0, slot);
                    encoder.setBuffer_offset_atIndex(Some(assembly.segments.handle()), 0, slot + 1);
                    encoder.setBuffer_offset_atIndex(Some(buffer.handle()), 0, slot + 2);
                    encoder.setBytes_length_atIndex(
                        NonNull::from(&words).cast::<c_void>(),
                        std::mem::size_of_val(&words),
                        slot + 3,
                    );
                }
                let group_size = MTLSize {
                    width: self.pipeline.threadExecutionWidth(), height: 1, depth: 1,
                };
                if let Some((arguments, offset)) = indirect {
                    // SAFETY: record_indirect validates the 12-byte aligned range;
                    // the command buffer retains the GPU-produced argument buffer.
                    unsafe {
                        encoder.dispatchThreadgroupsWithIndirectBuffer_indirectBufferOffset_threadsPerThreadgroup(
                            arguments.handle(), offset, group_size,
                        );
                    }
                } else {
                    encoder.dispatchThreads_threadsPerThreadgroup(
                        MTLSize { width: params.count as usize, height: params.instances as usize, depth: 1 },
                        group_size,
                    );
                }
                encoder
                    .memoryBarrierWithScope(MTLBarrierScope::Buffers | MTLBarrierScope::Textures);
            })?;
        }
        Ok(MetalGeometryVertices {
            buffer,
            stride: self.stride,
            count: params.count,
            instances: params.instances,
            base_instance,
            generic_mask: self.generic_mask,
        })
    }
}

/// Native object function only distributes cached vertex results. It executes
/// no guest vertex instructions, so sharing a strip vertex cannot repeat stores.
pub struct MetalGeometryObject {
    function: Retained<ProtocolObject<dyn MTLFunction>>,
    _library: Retained<ProtocolObject<dyn MTLLibrary>>,
    vertex_stride: usize,
    input_vertices: u32,
    generic_mask: u32,
}

impl MetalGeometryObject {
    pub fn new(
        device: &MetalDevice,
        layout: &GeometryLayout,
        runtime: &RuntimeInfo,
        invocations: u32,
        version: MslVersion,
    ) -> Result<Self, MetalGeometryPipelineError> {
        if !(1..=1024).contains(&invocations) {
            return Err(MetalGeometryPipelineError::Invocations(invocations));
        }
        if layout.input_vertices != runtime.input_topology.vertices() {
            return Err(MetalGeometryPipelineError::Layout);
        }
        let source = format!(
            r#"
#include <metal_stdlib>
using namespace metal;
{payload}
{params}
[[object, max_total_threads_per_threadgroup(1)]]
void geometry_object(object_data MslGeometryPayload& payload [[payload]], mesh_grid_properties grid,
    const device MslGeometryVertexIn* vertices [[buffer(0)]],
    const device uint* primitives [[buffer(1)]], constant GeometryDrawParams& p [[buffer(2)]],
    uint3 primitive_group [[threadgroup_position_in_grid]]) {{
    for (uint i=0u; i<{input_vertices}u; ++i) {{
        uint ordinal = primitives[ulong(primitive_group.x)*6u+i];
        payload.vertices[i] = vertices[ulong(primitive_group.y)*p.count+ordinal];
    }}
    payload.primitive_id = primitive_group.x;
    grid.set_threadgroups_per_grid(uint3({invocations}u, 1u, 1u));
}}
"#,
            payload = layout.payload_declaration(runtime),
            params = DRAW_PARAMS,
            input_vertices = layout.input_vertices
        );
        let library = compile_msl_library(device.device(), &source, version)?;
        let function = library
            .newFunctionWithName(&NSString::from_str("geometry_object"))
            .ok_or_else(|| MetalShaderError::MissingEntryPoint("geometry_object".into()))?;
        Ok(Self {
            function,
            _library: library,
            vertex_stride: GeometryLayout::vertex_stride(runtime),
            input_vertices: layout.input_vertices,
            generic_mask: GeometryLayout::generic_mask(runtime),
        })
    }

    pub fn function(&self) -> &ProtocolObject<dyn MTLFunction> {
        &self.function
    }

    pub fn record_draw(
        &self,
        encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
        assembly: &MetalPrimitiveAssembly,
        vertices: &MetalGeometryVertices,
    ) -> Result<(), MetalGeometryPipelineError> {
        let p = assembly.params();
        vertices.validate(assembly, self.vertex_stride, self.generic_mask, self.input_vertices)?;
        let words = [
            p.count,
            p.instances,
            p.base_vertex as u32,
            vertices.base_instance,
        ];
        // SAFETY: assembler and vertex pipeline wrote buffers earlier in the same scheduler.
        // The render pipeline bound by the caller contains this object function and matching mesh.
        unsafe {
            encoder.setObjectBuffer_offset_atIndex(Some(vertices.buffer.handle()), 0, 0);
            encoder.setObjectBuffer_offset_atIndex(Some(assembly.primitives.handle()), 0, 1);
            encoder.setObjectBytes_length_atIndex(
                NonNull::from(&words).cast(),
                std::mem::size_of_val(&words),
                2,
            );
            let one = MTLSize {
                width: 1,
                height: 1,
                depth: 1,
            };
            encoder.drawMeshThreadgroupsWithIndirectBuffer_indirectBufferOffset_threadsPerObjectThreadgroup_threadsPerMeshThreadgroup(
                assembly.dispatch_arguments.handle(), 0, one, one);
        }
        Ok(())
    }
}

const DRAW_PARAMS: &str =
    "struct GeometryDrawParams { uint count, instances, base_vertex, base_instance; };";
const VERTEX_DRAW_PARAMS: &str =
    "struct GeometryVertexDrawParams { uint count, instances, base_vertex, base_instance; ulong vertex_sizes[31]; };";

#[cfg(test)]
mod tests {
    use super::super::metal_primitive_assembler::{
        MetalPrimitiveAssembler, MetalPrimitiveAssemblyParams,
    };
    use super::*;
    use crate::engines::maxwell_3d::PrimitiveTopology;
    use shader_recompiler::backend::{
        bindings::Bindings,
        msl::{emit_msl::emit_msl_vertex_function, MslOptions},
    };
    use shader_recompiler::ir::{
        emitter::Emitter,
        opcodes::Opcode,
        value::{Attribute, Value},
        Program, SyntaxNode,
    };
    use shader_recompiler::ir_opt::collect_shader_info_pass::collect_shader_info_pass;
    use shader_recompiler::profile::Profile;
    use shader_recompiler::shader_info::StorageBufferDescriptor;
    use shader_recompiler::stage::Stage;

    #[test]
    fn vertex_results_preserve_ids_and_execute_stores_once_per_stream_entry() {
        let device = MetalDevice::new().unwrap();
        if !device.profile().supports_mesh_shaders() {
            return;
        }
        let assembler = MetalPrimitiveAssembler::new(&device).unwrap();
        let mut program = Program::new(Stage::VertexB);
        program.add_block();
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        {
            let mut ir = Emitter::new(&mut program, 0);
            ir.prologue();
            for (component, builtin) in [
                Attribute::VERTEX_ID,
                Attribute::INSTANCE_ID,
                Attribute::BASE_VERTEX,
                Attribute::BASE_INSTANCE,
            ]
            .into_iter()
            .enumerate()
            {
                let id = ir.get_attribute_u32(builtin, Value::ImmU32(0));
                let bits = ir.bit_cast_f32_u32(id);
                ir.set_attribute(
                    Attribute::generic(0, component as u32),
                    bits,
                    Value::ImmU32(0),
                );
            }
            ir.epilogue();
        }
        program.blocks[0].append_new_inst(
            Opcode::StorageAtomicIAdd32,
            vec![Value::ImmU32(0), Value::ImmU32(0), Value::ImmU32(1)],
        );
        collect_shader_info_pass(&mut program);
        program
            .info
            .storage_buffers_descriptors
            .push(StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                is_written: true,
            });
        let runtime = RuntimeInfo {
            previous_stage_stores: program.info.stores.clone(),
            ..Default::default()
        };
        let indices = [u32::MAX, 8, 9, 10, 11, u32::MAX, 5, 5, 5, u32::MAX];
        let index_buffer = MetalBuffer::new(&device, indices.len() * 4).unwrap();
        index_buffer
            .write(
                0,
                &indices
                    .into_iter()
                    .flat_map(u32::to_ne_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let arguments_pass = ConditionalRenderingArgumentsPass::new(&device).unwrap();
        for (native_ids, condition) in [false, true].into_iter().flat_map(|native_ids|
            [None, Some((0u32, false)), Some((1, false)), Some((0, true)), Some((1, true))]
                .into_iter().map(move |condition| (native_ids, condition))) {
            let profile = Profile {
                support_vertex_instance_id: native_ids,
                ..Default::default()
            };
            let artifact = emit_msl_vertex_function(
                &program,
                &profile,
                &RuntimeInfo::default(),
                &MslOptions {
                    language_version: MslVersion::V3_0,
                    ..Default::default()
                },
                &mut Bindings::default(),
            )
            .unwrap();
            let producer = MetalGeometryVertexPipeline::new(
                &device,
                &artifact,
                &MetalVertexInputState::default(),
                &runtime,
            )
            .unwrap();
            let mut scheduler = MetalScheduler::new(&device);
            let assembly = assembler
                .record(
                    &mut scheduler,
                    MetalPrimitiveAssemblyParams {
                        topology: PrimitiveTopology::TriangleStrip,
                        count: indices.len() as u32,
                        base_vertex: -3,
                        instances: 3,
                        index_bytes: 4,
                        restart_index: Some(u32::MAX),
                    },
                    Some((&index_buffer, 0)),
                )
                .unwrap();
            let counter = MetalBuffer::new(&device, 4).unwrap();
            counter.write(0, &0u32.to_ne_bytes()).unwrap();
            let bind = |encoder: &ProtocolObject<dyn MTLComputeCommandEncoder>| unsafe {
                encoder.setBuffer_offset_atIndex(Some(counter.handle()), 0, 0);
            };
            let mut pool = MetalStagingBufferPool::new(&device).unwrap();
            let vertices = if let Some((value, inverted)) = condition {
                let predicate = MetalBuffer::new_private(&device, 8).unwrap();
                let upload = MetalBuffer::new(&device, 4).unwrap();
                upload.write(0, &value.to_ne_bytes()).unwrap();
                upload.encode_copy(&mut scheduler, &predicate, 0, 4, 4).unwrap();
                let source = MetalBuffer::new(&device, 12).unwrap();
                let groups = [indices.len().div_ceil(producer.pipeline.threadExecutionWidth()) as u32, 3, 1];
                source.write(0, bytemuck::cast_slice(&groups)).unwrap();
                let arguments = arguments_pass.resolve(
                    &mut scheduler, &mut pool, &predicate, 4, inverted, &source, 0, 12, 1,
                    ConditionalArgumentLayout::Dispatch,
                ).unwrap();
                for offset in [1, arguments.buffer.length() - 4, usize::MAX] {
                    assert!(matches!(producer.record_indirect(
                        &mut scheduler, &assembly, 11, &[0; 31], &arguments.buffer, offset, |_| {
                            panic!("invalid arguments must be rejected before binding");
                        }), Err(MetalGeometryPipelineError::IndirectRange)));
                }
                producer.record_indirect(
                    &mut scheduler, &assembly, 11, &[0; 31], &arguments.buffer, arguments.offset, bind,
                ).unwrap()
            } else {
                producer.record(&mut scheduler, &assembly, 11, &[0; 31], bind).unwrap()
            };
            assert_eq!(vertices.stride, 32);
            let readback = MetalBuffer::new(&device, vertices.buffer.length()).unwrap();
            vertices
                .buffer
                .encode_copy(&mut scheduler, &readback, 0, 0, readback.length())
                .unwrap();
            scheduler.finish_all().unwrap();
            let mut count = [0u8; 4];
            counter.read(0, &mut count).unwrap();
            if condition.is_some_and(|(value, inverted)| (value != 0) == inverted) {
                assert_eq!(u32::from_ne_bytes(count), 0, "disabled guest vertex stage must not execute atomics");
                continue;
            }
            // Seven non-restart entries in each of three instances, not nine
            // primitive vertices per instance. Repeated indices remain entries.
            assert_eq!(u32::from_ne_bytes(count), 21);
            let mut bytes = vec![0; readback.length()];
            readback.read(0, &mut bytes).unwrap();
            for instance in 0..3 {
                for (ordinal, index) in indices.into_iter().enumerate() {
                    if index == u32::MAX {
                        continue;
                    }
                    let offset = (instance * indices.len() + ordinal) * 32 + 16;
                    let fields = bytes[offset..offset + 16]
                        .chunks_exact(4)
                        .map(|b| u32::from_ne_bytes(b.try_into().unwrap()))
                        .collect::<Vec<_>>();
                    assert_eq!(
                        fields,
                        [
                            index - 3,
                            instance as u32 + if native_ids { 11 } else { 0 },
                            (-3i32) as u32,
                            11
                        ],
                        "instance={instance} ordinal={ordinal} native_ids={native_ids}"
                    );
                }
            }
        }
    }
}
