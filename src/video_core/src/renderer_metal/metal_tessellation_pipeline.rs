// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native tessellation stage execution. Eden's vk_graphics_pipeline owns the
//! equivalent fixed-function stage setup; Metal executes the shared callable
//! TCS in compute and retains its outputs for a post-tessellation vertex stage.

use std::ptr::NonNull;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBarrierScope, MTLComputeCommandEncoder, MTLComputePipelineState, MTLDevice, MTLFunction,
    MTLLibrary, MTLRenderCommandEncoder, MTLRenderPipelineDescriptor, MTLRenderPipelineState,
    MTLSize, MTLTessellationFactorFormat, MTLTessellationFactorStepFunction,
    MTLTessellationPartitionMode, MTLWinding,
};
use shader_recompiler::backend::msl::{
    emit_msl_tessellation::TessellationControlLayout,
    msl_function::{MslFunctionKind, MslParameterAttribute},
    MslShaderArtifact, MslVersion,
};
use shader_recompiler::runtime_info::{RuntimeInfo, TessPrimitive, TessSpacing};
use thiserror::Error;

use super::metal_buffer::{MetalBuffer, MetalBufferError};
use super::metal_compute_pass::{
    ConditionalArgumentLayout, ConditionalRenderingArgumentsPass, MetalComputePassError,
};
use super::metal_device::MetalDevice;
use super::metal_geometry_pipeline::{
    MetalGeometryPipelineError, MetalGeometryVertexPipeline, MetalGeometryVertices,
};
use super::metal_pipeline_cache::MetalRenderPipelineKey;
use super::metal_primitive_assembler::MetalPatchAssembly;
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};
use super::metal_shader::{compile_msl_library, validate_native_binding_layout, MetalShaderError};
use super::metal_staging_buffer_pool::MetalStagingBufferPool;

pub struct MetalTessellationControlPipeline {
    device: MetalDevice,
    pipeline: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    layout: TessellationControlLayout,
    buffer_base: usize,
}

/// Instance regions use the allocation capacity, not the GPU's compacted patch
/// count. Consumers must preserve that stride even when restart removed patches.
pub struct MetalTessellationPatches {
    pub control_points: Arc<MetalBuffer>,
    pub patch_data: Arc<MetalBuffer>,
    pub capacity_per_instance: u32,
    pub instances: u32,
    pub output_vertices: u32,
    pub control_stride: usize,
    pub patch_stride: usize,
}

#[derive(Debug, Error)]
pub enum MetalTessellationPipelineError {
    #[error("incompatible native tessellation interface or vertex stream")]
    Layout,
    #[error("native tessellation transport exceeds Metal's buffer slots")]
    BufferSlots,
    #[error("native tessellation output size overflow")]
    OutputSize,
    #[error("native tessellation domain or generation level is unsupported")]
    Tessellator,
    #[error("Metal tessellation pipeline: {0}")]
    Pipeline(String),
    #[error(transparent)]
    Vertex(#[from] MetalGeometryPipelineError),
    #[error(transparent)]
    Conditional(#[from] MetalComputePassError),
    #[error(transparent)]
    Shader(#[from] MetalShaderError),
    #[error(transparent)]
    Buffer(#[from] MetalBufferError),
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
}

/// Converts the TCS producer's f32 factor fields into Metal's native per-patch
/// half layout. Patch data and instance spacing stay in producer allocation order.
pub struct MetalTessellationFactorPipeline {
    device: MetalDevice,
    pipeline: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    patch_stride: usize,
    factor_stride: usize,
}

pub struct MetalTessellationFactors {
    pub buffer: Arc<MetalBuffer>,
    pub instance_stride: usize,
}

pub struct MetalTessellationShaderStages {
    pub vertex: MslShaderArtifact,
    pub control: MslShaderArtifact,
    pub evaluation: MslShaderArtifact,
    pub layout: TessellationControlLayout,
    pub control_runtime: RuntimeInfo,
    pub evaluation_runtime: RuntimeInfo,
}

/// Retains every stage of one native tessellation pipeline. Guest descriptors
/// remain prepared by the graphics cache, not reread by these command encoders.
pub struct MetalTessellationPipeline {
    vertex: MetalGeometryVertexPipeline,
    control: MetalTessellationControlPipeline,
    factors: MetalTessellationFactorPipeline,
    evaluation: MetalTessellationEvaluationPipeline,
    device: MetalDevice,
}

pub struct MetalTessellationDraw {
    pub assembly: MetalPatchAssembly,
    pub patches: MetalTessellationPatches,
    pub factors: MetalTessellationFactors,
}

impl MetalTessellationPipeline {
    pub fn new(
        device: &MetalDevice,
        key: &MetalRenderPipelineKey,
        stages: &MetalTessellationShaderStages,
        fragment: Option<&ProtocolObject<dyn MTLFunction>>,
        max_level: u32,
    ) -> Result<Self, MetalTessellationPipelineError> {
        if !device.profile().supports_indirect_tessellation() {
            return Err(MetalTessellationPipelineError::Tessellator);
        }
        Ok(Self {
            vertex: MetalGeometryVertexPipeline::new(
                device,
                &stages.vertex,
                &key.vertex_input,
                &stages.control_runtime,
            )?,
            control: MetalTessellationControlPipeline::new(
                device,
                &stages.control,
                &stages.layout,
            )?,
            factors: MetalTessellationFactorPipeline::new(
                device,
                &stages.layout,
                stages.evaluation_runtime.tess_primitive,
                stages.evaluation_runtime.tess_spacing,
                max_level,
            )?,
            evaluation: MetalTessellationEvaluationPipeline::new(
                device,
                key,
                &stages.evaluation,
                &stages.layout,
                &stages.evaluation_runtime,
                max_level,
                fragment,
            )?,
            device: device.clone(),
        })
    }

    pub fn retained_state(&self) -> Retained<ProtocolObject<dyn MTLRenderPipelineState>> {
        self.evaluation.state.clone()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_inputs(
        &self,
        scheduler: &mut MetalScheduler,
        staging_pool: &mut MetalStagingBufferPool,
        arguments_pass: &ConditionalRenderingArgumentsPass,
        predicate: Option<(&MetalBuffer, usize, bool)>,
        assembly: &MetalPatchAssembly,
        vertex_sizes: &[u64; 31],
        bind_vertex: impl FnOnce(&ProtocolObject<dyn MTLComputeCommandEncoder>),
        bind_control: impl FnOnce(&ProtocolObject<dyn MTLComputeCommandEncoder>),
    ) -> Result<MetalTessellationDraw, MetalTessellationPipelineError> {
        let mut assembly = assembly.clone();
        // Resolve every consumer before running guest shaders: a shader store
        // must not change the predicate seen by another stage of the same draw.
        if let Some((predicate, offset, inverted)) = predicate {
            for (target, layout) in [
                (
                    &mut assembly.dispatch_arguments,
                    ConditionalArgumentLayout::Dispatch,
                ),
                (
                    &mut assembly.draw_arguments,
                    ConditionalArgumentLayout::Draw,
                ),
            ] {
                let size = layout.byte_size();
                let resolved = arguments_pass.resolve(
                    scheduler,
                    staging_pool,
                    predicate,
                    offset,
                    inverted,
                    target,
                    0,
                    size as u32,
                    1,
                    layout,
                )?;
                let retained = Arc::new(MetalBuffer::new_private(&self.device, size)?);
                resolved
                    .buffer
                    .encode_copy(scheduler, &retained, resolved.offset, 0, size)?;
                *target = retained;
            }
        }
        let vertices = if let Some((predicate, offset, inverted)) = predicate {
            self.vertex.record_conditional_stream(
                scheduler,
                staging_pool,
                arguments_pass,
                predicate,
                offset,
                inverted,
                &assembly.stream,
                assembly.base_instance,
                vertex_sizes,
                bind_vertex,
            )?
        } else {
            self.vertex.record_stream(
                scheduler,
                &assembly.stream,
                assembly.base_instance,
                vertex_sizes,
                bind_vertex,
            )?
        };
        let patches = self
            .control
            .record(scheduler, &assembly, &vertices, bind_control)?;
        let factors = self.factors.record(scheduler, &assembly, &patches)?;
        Ok(MetalTessellationDraw {
            assembly,
            patches,
            factors,
        })
    }

    pub fn record_draw(
        &self,
        encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
        draw: &MetalTessellationDraw,
    ) -> Result<(), MetalTessellationPipelineError> {
        self.evaluation
            .bind(encoder, &draw.patches, &draw.factors)?;
        unsafe {
            encoder.drawPatches_patchIndexBuffer_patchIndexBufferOffset_indirectBuffer_indirectBufferOffset(
                draw.patches.output_vertices as usize, None, 0,
                draw.assembly.draw_arguments.handle(), 0,
            );
        }
        Ok(())
    }
}

/// Post-tessellation vertex entry and fixed-function raster state. Vertex fetch
/// has already executed in compute; only retained control/patch data is bound.
pub struct MetalTessellationEvaluationPipeline {
    state: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    layout: TessellationControlLayout,
    buffer_base: usize,
    factor_stride: usize,
}

impl MetalTessellationEvaluationPipeline {
    pub fn new(
        device: &MetalDevice,
        key: &MetalRenderPipelineKey,
        artifact: &MslShaderArtifact,
        layout: &TessellationControlLayout,
        runtime: &RuntimeInfo,
        max_level: u32,
        fragment: Option<&ProtocolObject<dyn MTLFunction>>,
    ) -> Result<Self, MetalTessellationPipelineError> {
        validate_native_binding_layout(device.profile(), &artifact.bindings)?;
        let interface = artifact
            .interface
            .as_ref()
            .filter(|i| i.kind == MslFunctionKind::TessellationEvaluationFunction)
            .ok_or(MetalTessellationPipelineError::Layout)?;
        if !(1..=32).contains(&layout.output_vertices) {
            return Err(MetalTessellationPipelineError::Layout);
        }
        if max_level < 16
            || (runtime.tess_spacing != TessSpacing::Equal && max_level % 2 != 0)
            || device
                .profile()
                .max_tessellation_factor()
                .is_none_or(|max| max_level > max)
            || !device.profile().supports_sample_count(key.sample_count)
        {
            return Err(MetalTessellationPipelineError::Tessellator);
        }
        let (domain, coordinate_type, coordinate, factor_stride) = match runtime.tess_primitive {
            TessPrimitive::Triangles => ("triangle", "float3", "coordinate", 8),
            TessPrimitive::Quads => ("quad", "float2", "float3(coordinate, 0.0f)", 12),
            TessPrimitive::Isolines => return Err(MetalTessellationPipelineError::Tessellator),
        };
        let buffer_base = artifact.bindings.buffer_count as usize;
        if buffer_base > 28 {
            return Err(MetalTessellationPipelineError::BufferSlots);
        }
        let mut parameters = Vec::new();
        let mut arguments = Vec::new();
        for parameter in &interface.parameters {
            let argument = match parameter.attribute {
                MslParameterAttribute::None => match parameter.name.as_str() {
                    "patch_input" => format!("points + patch_index * {}u", layout.output_vertices),
                    "patch" => "patch_data[patch_index]".into(),
                    "patch_vertices" => format!("{}u", layout.output_vertices),
                    "patch_id" => "id".into(),
                    "tess_coord" => coordinate.into(),
                    _ => return Err(MetalTessellationPipelineError::Layout),
                },
                MslParameterAttribute::Buffer(_)
                | MslParameterAttribute::Texture(_)
                | MslParameterAttribute::Sampler(_) => {
                    parameters.push(parameter.declaration(MslFunctionKind::StageEntryPoint));
                    parameter.name.clone()
                }
                _ => return Err(MetalTessellationPipelineError::Layout),
            };
            arguments.push(argument);
        }
        for (index, declaration) in [
            "const device MslControlOutput* points",
            "const device MslControlPatch* patch_data",
            "constant uint& capacity",
        ]
        .into_iter()
        .enumerate()
        {
            parameters.push(format!("{declaration} [[buffer({})]]", buffer_base + index));
        }
        parameters.extend([
            "uint id [[patch_id]]".into(),
            "uint instance [[instance_id]]".into(),
            format!("{coordinate_type} coordinate [[position_in_patch]]"),
        ]);
        let result_type = if key.rasterization_enabled {
            "MslVertexOut"
        } else {
            "void"
        };
        let return_keyword = if key.rasterization_enabled {
            "return "
        } else {
            ""
        };
        let source = format!(
            r#"
{source}
{declarations}
static_assert(sizeof(MslControlOutput) == {control_stride}, "TES producer stride");
static_assert(sizeof(MslControlPatch) == {patch_stride}, "TES patch stride");
[[patch({domain}, {vertices})]] vertex {result_type} tessellation_evaluate({parameters}) {{
    ulong patch_index = ulong(instance) * capacity + id;
    {return_keyword}{entry}({arguments});
}}
"#,
            source = artifact.source.source,
            declarations = layout.declarations(),
            control_stride = layout.output_stride(),
            patch_stride = layout.patch_stride(),
            vertices = layout.output_vertices,
            parameters = parameters.join(", "),
            entry = artifact.entry_point,
            arguments = arguments.join(", "),
        );
        let library = compile_msl_library(device.device(), &source, artifact.language_version)?;
        let function = library
            .newFunctionWithName(&NSString::from_str("tessellation_evaluate"))
            .ok_or_else(|| MetalShaderError::MissingEntryPoint("tessellation_evaluate".into()))?;
        let descriptor = MTLRenderPipelineDescriptor::new();
        descriptor.setVertexFunction(Some(&function));
        descriptor.setFragmentFunction(fragment.filter(|_| key.rasterization_enabled));
        descriptor.setRasterSampleCount(key.sample_count as usize);
        descriptor.setAlphaToCoverageEnabled(key.alpha_to_coverage);
        descriptor.setAlphaToOneEnabled(key.alpha_to_one);
        descriptor.setRasterizationEnabled(key.rasterization_enabled);
        descriptor.setDepthAttachmentPixelFormat(key.depth_format);
        descriptor.setStencilAttachmentPixelFormat(key.stencil_format);
        descriptor.setTessellationFactorFormat(MTLTessellationFactorFormat::Half);
        descriptor.setTessellationFactorStepFunction(
            MTLTessellationFactorStepFunction::PerPatchAndPerInstance,
        );
        // Eden uses Vulkan's default upper-left tessellation domain. Its
        // signed (u,v) winding is opposite Metal's tessellator convention,
        // independently of the later viewport/front-face transforms.
        descriptor.setTessellationOutputWindingOrder(if runtime.tess_clockwise {
            MTLWinding::CounterClockwise
        } else {
            MTLWinding::Clockwise
        });
        unsafe {
            descriptor.setMaxTessellationFactor(max_level as usize);
            descriptor.setTessellationPartitionMode(match runtime.tess_spacing {
                TessSpacing::Equal => MTLTessellationPartitionMode::Integer,
                TessSpacing::FractionalEven => MTLTessellationPartitionMode::FractionalEven,
                TessSpacing::FractionalOdd => MTLTessellationPartitionMode::FractionalOdd,
            });
        }
        for (index, state) in key.color_attachments.iter().enumerate() {
            let attachment = unsafe {
                descriptor
                    .colorAttachments()
                    .objectAtIndexedSubscript(index)
            };
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
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .map_err(|error| MetalTessellationPipelineError::Pipeline(error.to_string()))?;
        Ok(Self {
            state,
            layout: layout.clone(),
            buffer_base,
            factor_stride,
        })
    }

    /// Bind instance-relative retained outputs. The caller uses the assembler's
    /// zero-base indirect arguments and binds captured guest TES/FS resources.
    pub fn bind(
        &self,
        encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
        patches: &MetalTessellationPatches,
        factors: &MetalTessellationFactors,
    ) -> Result<(), MetalTessellationPipelineError> {
        let count = (patches.capacity_per_instance as usize)
            .checked_mul(patches.instances as usize)
            .ok_or(MetalTessellationPipelineError::OutputSize)?;
        let controls = count
            .checked_mul(self.layout.output_vertices as usize)
            .and_then(|count| count.checked_mul(self.layout.output_stride()))
            .ok_or(MetalTessellationPipelineError::OutputSize)?;
        let patch_size = count
            .checked_mul(self.layout.patch_stride())
            .ok_or(MetalTessellationPipelineError::OutputSize)?;
        let factor_size = count
            .checked_mul(self.factor_stride)
            .ok_or(MetalTessellationPipelineError::OutputSize)?;
        if patches.control_stride != self.layout.output_stride()
            || patches.patch_stride != self.layout.patch_stride()
            || patches.output_vertices != self.layout.output_vertices
            || patches.control_points.length() < controls
            || patches.patch_data.length() < patch_size
            || factors.buffer.length() < factor_size
            || factors.instance_stride
                != patches.capacity_per_instance as usize * self.factor_stride
        {
            return Err(MetalTessellationPipelineError::Layout);
        }
        encoder.setRenderPipelineState(&self.state);
        unsafe {
            encoder.setVertexBuffer_offset_atIndex(
                Some(patches.control_points.handle()),
                0,
                self.buffer_base,
            );
            encoder.setVertexBuffer_offset_atIndex(
                Some(patches.patch_data.handle()),
                0,
                self.buffer_base + 1,
            );
            encoder.setVertexBytes_length_atIndex(
                NonNull::from(&patches.capacity_per_instance).cast(),
                4,
                self.buffer_base + 2,
            );
            encoder.setTessellationFactorBuffer_offset_instanceStride(
                Some(factors.buffer.handle()),
                0,
                // Metal requires a positive per-instance step even when the
                // indirect draw has zero patches and no record can be fetched.
                factors.instance_stride.max(self.factor_stride),
            );
        }
        Ok(())
    }
}

impl MetalTessellationFactorPipeline {
    pub fn new(
        device: &MetalDevice,
        layout: &TessellationControlLayout,
        primitive: TessPrimitive,
        spacing: TessSpacing,
        max_level: u32,
    ) -> Result<Self, MetalTessellationPipelineError> {
        let (edges, inners) = match primitive {
            TessPrimitive::Triangles => (3, 1),
            TessPrimitive::Quads => (4, 2),
            TessPrimitive::Isolines => return Err(MetalTessellationPipelineError::Tessellator),
        };
        if max_level < 16
            || (spacing != TessSpacing::Equal && max_level % 2 != 0)
            || device
                .profile()
                .max_tessellation_factor()
                .is_none_or(|max| max_level > max)
        {
            return Err(MetalTessellationPipelineError::Tessellator);
        }
        let conversion = match spacing {
            TessSpacing::Equal => format!("return half(ceil(clamp(value, 1.0f, {max_level}.0f)));"),
            TessSpacing::FractionalEven | TessSpacing::FractionalOdd => {
                let even = spacing == TessSpacing::FractionalEven;
                let min = if even { 2 } else { 1 };
                let max = if even { max_level } else { max_level - 1 };
                let bias = if even { 0 } else { 1 };
                format!(
                    r#"
    float clamped = clamp(value, {min}.0f, {max}.0f);
    half result = half(clamped);
    // Preserve the segment-count boundary if nearest-half rounding crossed
    // down onto an even/odd integer. Fractional positions retain native half
    // precision; do not replace the fractional level by its integer count.
    float segments = ceil((clamped - {bias}.0f) * 0.5f);
    if (ceil((float(result) - {bias}.0f) * 0.5f) < segments)
        result = as_type<half>(ushort(as_type<ushort>(result) + 1u));
    return result;
"#
                )
            }
        };
        let source = format!(
            r#"
#include <metal_stdlib>
using namespace metal;
{declarations}
static_assert(sizeof(MslControlPatch) == {patch_stride}, "factor source stride");
inline half convert_factor(float value, bool outer) {{
    // A floating comparison can flush a positive f32 subnormal to zero on
    // Apple GPUs. Classify raw bits before clamping so it cannot discard a patch.
    uint bits = as_type<uint>(value);
    uint magnitude = bits & 0x7fffffffu;
    if (outer && ((bits & 0x80000000u) != 0u || magnitude == 0u || magnitude > 0x7f800000u))
        return half(0.0f);
    {conversion}
}}
kernel void tessellation_factors(const device MslControlPatch* patches [[buffer(0)]],
    device half* factors [[buffer(1)]], constant uint& capacity [[buffer(2)]],
    uint3 group [[threadgroup_position_in_grid]]) {{
    ulong index = ulong(group.y) * capacity + group.x;
    for (uint i = 0; i < {edges}u; ++i)
        factors[index * {components}u + i] = convert_factor(patches[index].outer[i], true);
    for (uint i = 0; i < {inners}u; ++i)
        factors[index * {components}u + {edges}u + i] = convert_factor(patches[index].inner[i], false);
}}
"#,
            declarations = layout.declarations(),
            patch_stride = layout.patch_stride(),
            components = edges + inners
        );
        let library = compile_msl_library(device.device(), &source, MslVersion::V2_3)?;
        let function = library
            .newFunctionWithName(&NSString::from_str("tessellation_factors"))
            .ok_or_else(|| MetalShaderError::MissingEntryPoint("tessellation_factors".into()))?;
        let pipeline = device
            .device()
            .newComputePipelineStateWithFunction_error(&function)
            .map_err(|e| MetalTessellationPipelineError::Pipeline(e.to_string()))?;
        Ok(Self {
            device: device.clone(),
            pipeline,
            patch_stride: layout.patch_stride(),
            factor_stride: (edges + inners) * 2,
        })
    }

    pub fn record(
        &self,
        scheduler: &mut MetalScheduler,
        assembly: &MetalPatchAssembly,
        patches: &MetalTessellationPatches,
    ) -> Result<MetalTessellationFactors, MetalTessellationPipelineError> {
        if assembly.control_points == 0
            || patches.patch_stride != self.patch_stride
            || patches.capacity_per_instance
                != assembly.stream.params().count / assembly.control_points
            || patches.instances != assembly.stream.params().instances
        {
            return Err(MetalTessellationPipelineError::Layout);
        }
        let instance_stride = (patches.capacity_per_instance as usize)
            .checked_mul(self.factor_stride)
            .ok_or(MetalTessellationPipelineError::OutputSize)?;
        let size = instance_stride
            .checked_mul(patches.instances as usize)
            .ok_or(MetalTessellationPipelineError::OutputSize)?;
        let buffer = Arc::new(MetalBuffer::new_private(
            &self.device,
            size.max(self.factor_stride),
        )?);
        if size != 0 {
            scheduler.with_compute_encoder(|encoder| {
                encoder.setComputePipelineState(&self.pipeline);
                // SAFETY: the source follows the checked producer layout; the
                // assembler owns the indirect grid. Metal retains bound buffers.
                unsafe {
                    encoder.setBuffer_offset_atIndex(Some(patches.patch_data.handle()), 0, 0);
                    encoder.setBuffer_offset_atIndex(Some(buffer.handle()), 0, 1);
                    encoder.setBytes_length_atIndex(NonNull::from(&patches.capacity_per_instance).cast(), 4, 2);
                    encoder.dispatchThreadgroupsWithIndirectBuffer_indirectBufferOffset_threadsPerThreadgroup(
                        assembly.dispatch_arguments.handle(), 0, MTLSize { width: 1, height: 1, depth: 1 });
                }
                encoder.memoryBarrierWithScope(MTLBarrierScope::Buffers);
            })?;
        }
        Ok(MetalTessellationFactors {
            buffer,
            instance_stride,
        })
    }
}

impl MetalTessellationControlPipeline {
    pub fn new(
        device: &MetalDevice,
        artifact: &MslShaderArtifact,
        layout: &TessellationControlLayout,
    ) -> Result<Self, MetalTessellationPipelineError> {
        validate_native_binding_layout(device.profile(), &artifact.bindings)?;
        let interface = artifact
            .interface
            .as_ref()
            .filter(|i| i.kind == MslFunctionKind::TessellationControlFunction)
            .ok_or(MetalTessellationPipelineError::Layout)?;
        if !(1..=32).contains(&layout.output_vertices) {
            return Err(MetalTessellationPipelineError::Layout);
        }
        let buffer_base = artifact.bindings.buffer_count as usize;
        if buffer_base > 26 {
            return Err(MetalTessellationPipelineError::BufferSlots);
        }
        let mut parameters = Vec::new();
        let mut arguments = Vec::new();
        for parameter in &interface.parameters {
            let argument = match parameter.attribute {
                MslParameterAttribute::None => match parameter.name.as_str() {
                    "patch_input" => {
                        "input + ulong(group.y)*p.input_count + starts[group.x]".into()
                    }
                    "patch_output" => format!("output + patch_index*{}u", layout.output_vertices),
                    "patch" => "patch_data[patch_index]".into(),
                    "patch_vertices" => "p.input_vertices".into(),
                    "invocation_id" => "invocation".into(),
                    "patch_id" => "group.x".into(),
                    _ => return Err(MetalTessellationPipelineError::Layout),
                },
                MslParameterAttribute::Builtin("thread_index_in_simdgroup")
                | MslParameterAttribute::Buffer(_)
                | MslParameterAttribute::Texture(_)
                | MslParameterAttribute::Sampler(_) => {
                    parameters.push(parameter.declaration(MslFunctionKind::StageEntryPoint));
                    parameter.name.clone()
                }
                _ => return Err(MetalTessellationPipelineError::Layout),
            };
            arguments.push(argument);
        }
        for (index, declaration) in [
            "const device MslControlInput* input",
            "device MslControlOutput* output",
            "device MslControlPatch* patch_data",
            "const device uint* starts",
            "constant ControlDrawParams& p",
        ]
        .into_iter()
        .enumerate()
        {
            parameters.push(format!("{declaration} [[buffer({})]]", buffer_base + index));
        }
        parameters.extend([
            "uint3 group [[threadgroup_position_in_grid]]".into(),
            "uint invocation [[thread_index_in_threadgroup]]".into(),
        ]);
        let source = format!(
            r#"
{source}
static_assert(sizeof(MslControlInput) == {input_stride}, "TCS input stride");
static_assert(sizeof(MslControlOutput) == {output_stride}, "TCS output stride");
static_assert(sizeof(MslControlPatch) == {patch_stride}, "TCS patch stride");
struct ControlDrawParams {{ uint input_count, input_vertices, patch_capacity; }};
kernel void tessellation_control({parameters}) {{
    ulong patch_index = ulong(group.y)*p.patch_capacity + group.x;
    {entry}({arguments});
}}
"#,
            source = artifact.source.source,
            input_stride = layout.input_stride(),
            output_stride = layout.output_stride(),
            patch_stride = layout.patch_stride(),
            parameters = parameters.join(", "),
            entry = artifact.entry_point,
            arguments = arguments.join(", ")
        );
        let library = compile_msl_library(device.device(), &source, artifact.language_version)?;
        let function = library
            .newFunctionWithName(&NSString::from_str("tessellation_control"))
            .ok_or_else(|| MetalShaderError::MissingEntryPoint("tessellation_control".into()))?;
        let pipeline = device
            .device()
            .newComputePipelineStateWithFunction_error(&function)
            .map_err(|e| MetalTessellationPipelineError::Pipeline(e.to_string()))?;
        if pipeline.maxTotalThreadsPerThreadgroup() < layout.output_vertices as usize {
            return Err(MetalTessellationPipelineError::Layout);
        }
        Ok(Self {
            device: device.clone(),
            pipeline,
            layout: layout.clone(),
            buffer_base,
        })
    }

    /// Only complete patches enter the TCS; one workgroup owns every invocation
    /// of one patch. Resource bindings must already contain captured guest data.
    pub fn record(
        &self,
        scheduler: &mut MetalScheduler,
        assembly: &MetalPatchAssembly,
        vertices: &MetalGeometryVertices,
        bind_resources: impl FnOnce(&ProtocolObject<dyn MTLComputeCommandEncoder>),
    ) -> Result<MetalTessellationPatches, MetalTessellationPipelineError> {
        if !(1..=32).contains(&assembly.control_points)
            || !vertices.matches_stream(
                &assembly.stream,
                self.layout.input_stride(),
                self.layout.input_generic_mask(),
            )
        {
            return Err(MetalTessellationPipelineError::Layout);
        }
        let params = assembly.stream.params();
        let capacity_per_instance = params.count / assembly.control_points;
        let count = (capacity_per_instance as usize)
            .checked_mul(params.instances as usize)
            .ok_or(MetalTessellationPipelineError::OutputSize)?;
        let output_size = count
            .checked_mul(self.layout.output_vertices as usize)
            .and_then(|n| n.checked_mul(self.layout.output_stride()))
            .ok_or(MetalTessellationPipelineError::OutputSize)?;
        let patch_size = count
            .checked_mul(self.layout.patch_stride())
            .ok_or(MetalTessellationPipelineError::OutputSize)?;
        // Typed bindings must cover at least one element even for a zero grid.
        let control_points = Arc::new(MetalBuffer::new_private(
            &self.device,
            output_size.max(self.layout.output_stride()),
        )?);
        let patch_data = Arc::new(MetalBuffer::new_private(
            &self.device,
            patch_size.max(self.layout.patch_stride()),
        )?);
        if count != 0 {
            let words = [params.count, assembly.control_points, capacity_per_instance];
            scheduler.with_compute_encoder(|encoder| {
                encoder.setComputePipelineState(&self.pipeline);
                bind_resources(encoder);
                // SAFETY: command buffers retain these native resources; setBytes
                // copies the scalar parameters before this method returns.
                unsafe {
                    for (index, buffer) in [vertices.buffer.as_ref(), control_points.as_ref(),
                        patch_data.as_ref(), assembly.starts.as_ref()].into_iter().enumerate() {
                        encoder.setBuffer_offset_atIndex(Some(buffer.handle()), 0, self.buffer_base + index);
                    }
                    encoder.setBytes_length_atIndex(NonNull::from(&words).cast(),
                        std::mem::size_of_val(&words), self.buffer_base + 4);
                    encoder.dispatchThreadgroupsWithIndirectBuffer_indirectBufferOffset_threadsPerThreadgroup(
                        assembly.dispatch_arguments.handle(), 0,
                        MTLSize { width: self.layout.output_vertices as usize, height: 1, depth: 1 });
                }
                encoder.memoryBarrierWithScope(MTLBarrierScope::Buffers | MTLBarrierScope::Textures);
            })?;
        }
        Ok(MetalTessellationPatches {
            control_points,
            patch_data,
            capacity_per_instance,
            instances: params.instances,
            output_vertices: self.layout.output_vertices,
            control_stride: self.layout.output_stride(),
            patch_stride: self.layout.patch_stride(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_factor_lookup_includes_base_instance() {
        use objc2_metal::{
            MTLBlitCommandEncoder as _, MTLClearColor, MTLLoadAction, MTLOrigin, MTLPixelFormat,
            MTLRenderCommandEncoder as _, MTLRenderPassDescriptor, MTLRenderPipelineDescriptor,
            MTLStoreAction, MTLTessellationFactorStepFunction, MTLTessellationPartitionMode,
            MTLTextureDescriptor, MTLTextureUsage, MTLViewport,
        };

        let device = MetalDevice::new().unwrap();
        let source = r#"
#include <metal_stdlib>
using namespace metal;
struct Out { float4 position [[position]]; float4 color [[user(locn0)]]; };
[[patch(triangle, 3)]] vertex Out evaluate(
    uint patch [[patch_id]], uint instance [[instance_id]], uint base [[base_instance]],
    float3 uvw [[position_in_patch]]) {
    const float2 xy[3] = {float2(-0.4f,-0.8f),float2(0.4f,-0.8f),float2(0,0.8f)};
    Out out;
    out.position = float4(xy[0]*uvw.x + xy[1]*uvw.y + xy[2]*uvw.z
        + float2(patch ? 0.5f : -0.5f, 0), 0.5f, 1);
    out.color = instance - base == 0 ? float4(1,0,0,1) : float4(0,1,0,1);
    return out;
}
fragment float4 shade(Out input [[stage_in]]) { return input.color; }
"#;
        let library = compile_msl_library(device.device(), source, MslVersion::V2_3).unwrap();
        let descriptor = MTLRenderPipelineDescriptor::new();
        descriptor.setVertexFunction(Some(
            &library
                .newFunctionWithName(&NSString::from_str("evaluate"))
                .unwrap(),
        ));
        descriptor.setFragmentFunction(Some(
            &library
                .newFunctionWithName(&NSString::from_str("shade"))
                .unwrap(),
        ));
        unsafe {
            descriptor
                .colorAttachments()
                .objectAtIndexedSubscript(0)
                .setPixelFormat(MTLPixelFormat::RGBA8Unorm);
            descriptor.setMaxTessellationFactor(16);
            descriptor.setTessellationPartitionMode(MTLTessellationPartitionMode::Integer);
        }
        descriptor.setTessellationFactorStepFunction(
            MTLTessellationFactorStepFunction::PerPatchAndPerInstance,
        );
        let pipeline = device
            .device()
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .unwrap();
        let texture_descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::RGBA8Unorm,
                64,
                64,
                false,
            )
        };
        texture_descriptor.setUsage(MTLTextureUsage::RenderTarget);
        let texture = device
            .device()
            .newTextureWithDescriptor(&texture_descriptor)
            .unwrap();
        let pass = MTLRenderPassDescriptor::renderPassDescriptor();
        let attachment = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        attachment.setTexture(Some(&texture));
        attachment.setLoadAction(MTLLoadAction::Clear);
        attachment.setStoreAction(MTLStoreAction::Store);
        attachment.setClearColor(MTLClearColor {
            red: 0.,
            green: 0.,
            blue: 0.,
            alpha: 0.,
        });
        let download = MetalBuffer::new(&device, 256 * 64).unwrap();
        let assembler = MetalPrimitiveAssembler::new(&device).unwrap();
        for (base, native_indirect) in [
            (0usize, false),
            (11, false),
            (11, true),
            (u32::MAX as usize, true),
        ] {
            // Padded allocation is strictly a hardware-contract test. Production
            // must not allocate guest-baseInstance-sized prefixes.
            let stride = 32;
            let factor_base = if native_indirect { 0 } else { base };
            let mut bytes = vec![0u8; (factor_base + 2) * stride];
            for instance in 0..2 {
                let offset = (factor_base + instance) * stride + instance * 8;
                for component in 0..4 {
                    bytes[offset + component * 2..offset + component * 2 + 2]
                        .copy_from_slice(&0x4400u16.to_ne_bytes()); // half(4)
                }
            }
            let factors = MetalBuffer::new(&device, bytes.len()).unwrap();
            factors.write(0, &bytes).unwrap();
            let mut scheduler = MetalScheduler::new(&device);
            let assembly = if native_indirect {
                let assembly = assembler
                    .record_patches(
                        &mut scheduler,
                        MetalPrimitiveAssemblyParams {
                            topology: PrimitiveTopology::Patches,
                            count: 6,
                            instances: 2,
                            base_vertex: 0,
                            index_bytes: 0,
                            restart_index: None,
                        },
                        3,
                        None,
                        base as u32,
                    )
                    .unwrap();
                assert_eq!(assembly.base_instance, base as u32);
                Some(assembly)
            } else {
                None
            };
            scheduler.begin_render_pass(&pass).unwrap();
            scheduler.with_render_encoder(|encoder| unsafe {
                encoder.setRenderPipelineState(&pipeline);
                encoder.setViewport(MTLViewport {
                    originX: 0., originY: 0., width: 64., height: 64., znear: 0., zfar: 1.,
                });
                encoder.setTessellationFactorBuffer_offset_instanceStride(
                    Some(factors.handle()), 0, stride,
                );
                if let Some(assembly) = &assembly {
                    encoder.drawPatches_patchIndexBuffer_patchIndexBufferOffset_indirectBuffer_indirectBufferOffset(
                        3, None, 0, assembly.draw_arguments.handle(), 0,
                    );
                } else {
                    encoder.drawPatches_patchStart_patchCount_patchIndexBuffer_patchIndexBufferOffset_instanceCount_baseInstance(
                        3, 0, 2, None, 0, 2, base,
                    );
                }
            }).unwrap();
            scheduler.with_blit_encoder(|encoder| unsafe {
                encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                    &texture, 0, 0, MTLOrigin { x: 0, y: 0, z: 0 },
                    MTLSize { width: 64, height: 64, depth: 1 },
                    download.handle(), 0, 256, 256 * 64,
                );
            }).unwrap();
            scheduler.finish_all().unwrap();
            let mut pixels = vec![0; download.length()];
            download.read(0, &mut pixels).unwrap();
            for (x, expected) in [(16, [255, 0, 0, 255]), (48, [0, 255, 0, 255])] {
                assert_eq!(
                    &pixels[(32 * 64 + x) * 4..(32 * 64 + x) * 4 + 4],
                    &expected,
                    "baseInstance={base} indirect={native_indirect} x={x}",
                );
            }
        }
    }
    use crate::engines::maxwell_3d::PrimitiveTopology;
    use crate::renderer_metal::{
        metal_geometry_pipeline::MetalGeometryVertexPipeline,
        metal_pipeline_cache::MetalVertexInputState,
        metal_primitive_assembler::{MetalPrimitiveAssembler, MetalPrimitiveAssemblyParams},
    };
    use shader_recompiler::{
        backend::{
            bindings::Bindings,
            msl::{
                emit_msl::{emit_msl_tessellation_control_function, emit_msl_vertex_function},
                MslOptions,
            },
        },
        ir::{emitter::Emitter, Attribute, Opcode, Patch, Program, SyntaxNode, Value},
        ir_opt::collect_shader_info_pass::collect_shader_info_pass,
        profile::Profile,
        runtime_info::RuntimeInfo,
        shader_info::StorageBufferDescriptor,
        stage::Stage,
    };

    #[test]
    fn complete_chain_preserves_conditional_stage_writes_and_pixels() {
        use objc2_metal::{
            MTLBlitCommandEncoder as _, MTLClearColor, MTLLoadAction, MTLOrigin, MTLPixelFormat,
            MTLRenderPassDescriptor, MTLStoreAction, MTLTextureDescriptor, MTLTextureUsage,
            MTLViewport,
        };
        use shader_recompiler::backend::msl::emit_msl::emit_msl_tessellation_evaluation_function;
        let device = MetalDevice::new().unwrap();
        let profile = Profile {
            support_vertex_instance_id: true,
            ..Default::default()
        };
        let mut vertex = Program::new(Stage::VertexB);
        vertex.add_block();
        vertex.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        let mut ir = Emitter::new(&mut vertex, 0);
        let id = ir.get_attribute_u32(Attribute::VERTEX_ID, Value::ImmU32(0));
        let first = ir.i_equal(id, Value::ImmU32(0));
        let second = ir.i_equal(id, Value::ImmU32(1));
        let third = ir.i_equal(id, Value::ImmU32(2));
        let x = ir.select_f32(second, Value::ImmF32(0.8), Value::ImmF32(0.0));
        let x = ir.select_f32(first, Value::ImmF32(-0.8), x);
        let y = ir.select_f32(third, Value::ImmF32(0.8), Value::ImmF32(-0.8));
        ir.set_attribute(Attribute::POSITION_X, x, Value::ImmU32(0));
        ir.set_attribute(Attribute::POSITION_Y, y, Value::ImmU32(0));
        ir.set_attribute(Attribute::POSITION_Z, Value::ImmF32(0.5), Value::ImmU32(0));
        ir.set_attribute(Attribute::POSITION_W, Value::ImmF32(1.0), Value::ImmU32(0));
        // All predicate resolutions must precede this write by the VS.
        vertex.blocks[0].append_new_inst(
            Opcode::StorageAtomicExchange32,
            vec![Value::ImmU32(0), Value::ImmU32(12), Value::ImmU32(0)],
        );
        vertex.blocks[0].append_new_inst(
            Opcode::StorageAtomicIAdd32,
            vec![Value::ImmU32(0), Value::ImmU32(0), Value::ImmU32(1)],
        );
        collect_shader_info_pass(&mut vertex);
        vertex
            .info
            .storage_buffers_descriptors
            .push(StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                is_written: true,
            });
        let control_runtime = RuntimeInfo {
            previous_stage_stores: vertex.info.stores.clone(),
            ..Default::default()
        };
        let mut control = Program::new(Stage::TessellationControl);
        control.invocations = 3;
        for _ in 0..3 {
            control.add_block();
        }
        let mut ir = Emitter::new(&mut control, 0);
        let id = ir.invocation_id();
        for attribute in [
            Attribute::POSITION_X,
            Attribute::POSITION_Y,
            Attribute::POSITION_Z,
            Attribute::POSITION_W,
        ] {
            let value = ir.get_attribute(attribute, id);
            ir.set_attribute(attribute, value, Value::ImmU32(0));
        }
        let first = ir.i_equal(id, Value::ImmU32(0));
        let condition = ir.condition_ref(first);
        let mut ir = Emitter::new(&mut control, 1);
        for index in 0..6 {
            ir.set_patch(Patch(index), Value::ImmF32(2.0));
        }
        ir.set_patch(Patch::generic(0, 0), Value::ImmF32(1.0));
        control.blocks[0].append_new_inst(
            Opcode::StorageAtomicIAdd32,
            vec![Value::ImmU32(0), Value::ImmU32(4), Value::ImmU32(1)],
        );
        control.syntax_list = vec![
            SyntaxNode::Block(0),
            SyntaxNode::If {
                cond: condition,
                body: 1,
                merge: 2,
            },
            SyntaxNode::Block(1),
            SyntaxNode::EndIf { merge: 2 },
            SyntaxNode::Block(2),
            SyntaxNode::Return,
        ];
        collect_shader_info_pass(&mut control);
        control
            .info
            .storage_buffers_descriptors
            .push(StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                is_written: true,
            });
        let layout = TessellationControlLayout::new(&control, &control_runtime).unwrap();
        let evaluation_runtime = RuntimeInfo {
            previous_stage_stores: control.info.stores.clone(),
            tess_primitive: TessPrimitive::Triangles,
            tess_spacing: TessSpacing::Equal,
            ..Default::default()
        };
        let mut evaluation = Program::new(Stage::TessellationEval);
        evaluation.add_block();
        evaluation.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        let mut ir = Emitter::new(&mut evaluation, 0);
        let u = ir.get_attribute(Attribute::TESSELLATION_EVALUATION_POINT_U, Value::ImmU32(0));
        let v = ir.get_attribute(Attribute::TESSELLATION_EVALUATION_POINT_V, Value::ImmU32(0));
        let uv = ir.fp_add_32(u, v);
        let w = ir.fp_sub_32(Value::ImmF32(1.0), uv);
        for attribute in [
            Attribute::POSITION_X,
            Attribute::POSITION_Y,
            Attribute::POSITION_Z,
            Attribute::POSITION_W,
        ] {
            let a = ir.get_attribute(attribute, Value::ImmU32(0));
            let b = ir.get_attribute(attribute, Value::ImmU32(1));
            let c = ir.get_attribute(attribute, Value::ImmU32(2));
            let a = ir.fp_mul_32(a, u);
            let b = ir.fp_mul_32(b, v);
            let c = ir.fp_mul_32(c, w);
            let ab = ir.fp_add_32(a, b);
            let position = ir.fp_add_32(ab, c);
            ir.set_attribute(attribute, position, Value::ImmU32(0));
        }
        let red = ir.get_patch(Patch::generic(0, 0));
        for (component, value) in [
            red,
            Value::ImmF32(0.0),
            Value::ImmF32(0.0),
            Value::ImmF32(1.0),
        ]
        .into_iter()
        .enumerate()
        {
            ir.set_attribute(
                Attribute::generic(0, component as u32),
                value,
                Value::ImmU32(0),
            );
        }
        evaluation.blocks[0].append_new_inst(
            Opcode::StorageAtomicIAdd32,
            vec![Value::ImmU32(0), Value::ImmU32(8), Value::ImmU32(1)],
        );
        collect_shader_info_pass(&mut evaluation);
        evaluation
            .info
            .storage_buffers_descriptors
            .push(StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                is_written: true,
            });
        let stages = MetalTessellationShaderStages {
            vertex: emit_msl_vertex_function(
                &vertex,
                &profile,
                &RuntimeInfo::default(),
                &MslOptions::default(),
                &mut Bindings::default(),
            )
            .unwrap(),
            control: emit_msl_tessellation_control_function(
                &control,
                &profile,
                &control_runtime,
                &MslOptions::default(),
                &mut Bindings::default(),
            )
            .unwrap(),
            evaluation: emit_msl_tessellation_evaluation_function(
                &evaluation,
                &profile,
                &evaluation_runtime,
                &MslOptions::default(),
                &mut Bindings::default(),
            )
            .unwrap(),
            layout,
            control_runtime,
            evaluation_runtime,
        };
        let library = compile_msl_library(
            device.device(),
            r#"
#include <metal_stdlib>
using namespace metal;
struct Input { float4 color [[user(locn0)]]; };
fragment float4 shade(Input input [[stage_in]]) { return input.color; }
"#,
            MslVersion::V2_3,
        )
        .unwrap();
        let fragment = library
            .newFunctionWithName(&NSString::from_str("shade"))
            .unwrap();
        let mut key = MetalRenderPipelineKey::new(0, 0);
        key.color_attachments[0].format = MTLPixelFormat::RGBA8Unorm;
        let pipeline =
            MetalTessellationPipeline::new(&device, &key, &stages, Some(&fragment), 16).unwrap();
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::RGBA8Unorm,
                64,
                64,
                false,
            )
        };
        descriptor.setUsage(MTLTextureUsage::RenderTarget);
        let target = device
            .device()
            .newTextureWithDescriptor(&descriptor)
            .unwrap();
        let pass = MTLRenderPassDescriptor::renderPassDescriptor();
        let attachment = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        attachment.setTexture(Some(&target));
        attachment.setLoadAction(MTLLoadAction::Clear);
        attachment.setStoreAction(MTLStoreAction::Store);
        attachment.setClearColor(MTLClearColor {
            red: 0.,
            green: 0.,
            blue: 0.,
            alpha: 0.,
        });
        let assembler = MetalPrimitiveAssembler::new(&device).unwrap();
        let conditional = ConditionalRenderingArgumentsPass::new(&device).unwrap();
        for condition in [
            None,
            Some((0u32, false)),
            Some((1, false)),
            Some((0, true)),
            Some((1, true)),
        ] {
            let enabled = condition.is_none_or(|(value, inverted)| (value != 0) != inverted);
            let mut scheduler = MetalScheduler::new(&device);
            let mut pool = MetalStagingBufferPool::new(&device).unwrap();
            let counters = MetalBuffer::new_private(&device, 16).unwrap();
            let upload = MetalBuffer::new(&device, 16).unwrap();
            upload
                .write(
                    0,
                    bytemuck::cast_slice(&[0u32, 0, 0, condition.map_or(1, |c| c.0)]),
                )
                .unwrap();
            upload
                .encode_copy(&mut scheduler, &counters, 0, 0, 16)
                .unwrap();
            let assembly = assembler
                .record_patches(
                    &mut scheduler,
                    MetalPrimitiveAssemblyParams {
                        topology: PrimitiveTopology::Patches,
                        count: 3,
                        instances: 2,
                        base_vertex: 0,
                        index_bytes: 0,
                        restart_index: None,
                    },
                    3,
                    None,
                    19,
                )
                .unwrap();
            let original = assembly.draw_arguments.clone();
            let bind = |encoder: &ProtocolObject<dyn MTLComputeCommandEncoder>| unsafe {
                encoder.setBuffer_offset_atIndex(Some(counters.handle()), 0, 0);
            };
            let draw = pipeline
                .record_inputs(
                    &mut scheduler,
                    &mut pool,
                    &conditional,
                    condition.map(|(_, inverted)| (&counters, 12, inverted)),
                    &assembly,
                    &[0; 31],
                    bind,
                    bind,
                )
                .unwrap();
            scheduler.begin_render_pass(&pass).unwrap();
            scheduler
                .with_render_encoder(|encoder| unsafe {
                    encoder.setVertexBuffer_offset_atIndex(Some(counters.handle()), 0, 0);
                    encoder.setViewport(MTLViewport {
                        originX: 0.,
                        originY: 0.,
                        width: 64.,
                        height: 64.,
                        znear: 0.,
                        zfar: 1.,
                    });
                    pipeline.record_draw(encoder, &draw).unwrap();
                })
                .unwrap();
            let pixels = MetalBuffer::new(&device, 256 * 64).unwrap();
            scheduler.with_blit_encoder(|encoder| unsafe {
                encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                    &target, 0, 0, MTLOrigin { x: 0, y: 0, z: 0 },
                    MTLSize { width: 64, height: 64, depth: 1 }, pixels.handle(), 0, 256, 256 * 64);
            }).unwrap();
            let downloaded = MetalBuffer::new(&device, 32).unwrap();
            counters
                .encode_copy(&mut scheduler, &downloaded, 0, 0, 16)
                .unwrap();
            original
                .encode_copy(&mut scheduler, &downloaded, 0, 16, 16)
                .unwrap();
            drop((draw, assembly, counters, upload, original));
            scheduler.finish_all().unwrap();
            let mut bytes = [0; 32];
            downloaded.read(0, &mut bytes).unwrap();
            let words: Vec<u32> = bytes
                .chunks_exact(4)
                .map(|b| u32::from_ne_bytes(b.try_into().unwrap()))
                .collect();
            assert_eq!(words[0], if enabled { 6 } else { 0 }, "VS {condition:?}");
            assert_eq!(words[1], if enabled { 6 } else { 0 }, "TCS {condition:?}");
            assert_eq!(words[2] > 0, enabled, "TES {condition:?}");
            assert_eq!(
                &words[4..],
                &[1, 2, 0, 0],
                "conditional path mutated original arguments"
            );
            let mut color = [0; 4];
            pixels.read((32 * 64 + 32) * 4, &mut color).unwrap();
            assert_eq!(
                color,
                if enabled { [255, 0, 0, 255] } else { [0; 4] },
                "{condition:?}"
            );
        }
    }

    /// Vulkan's default upper-left domain defines CW as positive signed area
    /// in (u,v). Compare the production TES against explicit triangles with
    /// that area, using identical native viewport/front-face/culling state.
    #[test]
    fn evaluation_winding_matches_upper_left_domain_triangles() {
        use objc2_metal::{
            MTLBlitCommandEncoder as _, MTLCullMode, MTLLoadAction, MTLOrigin, MTLPixelFormat,
            MTLPrimitiveType, MTLRenderPassDescriptor, MTLStoreAction, MTLTextureDescriptor,
            MTLTextureUsage, MTLViewport,
        };
        use shader_recompiler::backend::msl::emit_msl::emit_msl_tessellation_evaluation_function;

        let device = MetalDevice::new().unwrap();
        let mut evaluation = Program::new(Stage::TessellationEval);
        evaluation.add_block();
        evaluation.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        let mut ir = Emitter::new(&mut evaluation, 0);
        for (coordinate, output) in [
            (
                Attribute::TESSELLATION_EVALUATION_POINT_U,
                Attribute::POSITION_X,
            ),
            (
                Attribute::TESSELLATION_EVALUATION_POINT_V,
                Attribute::POSITION_Y,
            ),
        ] {
            let value = ir.get_attribute(coordinate, Value::ImmU32(0));
            let scaled = ir.fp_mul_32(value, Value::ImmF32(1.6));
            let position = ir.fp_sub_32(scaled, Value::ImmF32(0.8));
            ir.set_attribute(output, position, Value::ImmU32(0));
        }
        ir.set_attribute(Attribute::POSITION_Z, Value::ImmF32(0.5), Value::ImmU32(0));
        ir.set_attribute(Attribute::POSITION_W, Value::ImmF32(1.0), Value::ImmU32(0));
        collect_shader_info_pass(&mut evaluation);
        let artifact = emit_msl_tessellation_evaluation_function(
            &evaluation,
            &Profile::default(),
            &RuntimeInfo::default(),
            &MslOptions::default(),
            &mut Bindings::default(),
        )
        .unwrap();
        let mut control = Program::new(Stage::TessellationControl);
        control.invocations = 3;
        let layout = TessellationControlLayout::new(&control, &RuntimeInfo::default()).unwrap();
        let library = compile_msl_library(
            device.device(),
            r#"
#include <metal_stdlib>
using namespace metal;
vertex float4 reference(uint id [[vertex_id]], constant uint& clockwise [[buffer(0)]]) {
    const float2 uv[6] = {float2(0,0),float2(1,0),float2(0,1),
                          float2(0,1),float2(1,0),float2(1,1)};
    uint corner = id % 3;
    if (!clockwise && corner) id = (id / 3) * 3 + 3 - corner;
    return float4(uv[id] * 1.6f - 0.8f, 0.5f, 1.0f);
}
fragment float4 shade(bool front [[front_facing]]) {
    return front ? float4(1,0,0,1) : float4(0,1,0,1);
}
"#,
            MslVersion::V2_3,
        )
        .unwrap();
        let fragment = library
            .newFunctionWithName(&NSString::from_str("shade"))
            .unwrap();
        let reference_desc = MTLRenderPipelineDescriptor::new();
        reference_desc.setVertexFunction(Some(
            &library
                .newFunctionWithName(&NSString::from_str("reference"))
                .unwrap(),
        ));
        reference_desc.setFragmentFunction(Some(&fragment));
        unsafe {
            reference_desc
                .colorAttachments()
                .objectAtIndexedSubscript(0)
                .setPixelFormat(MTLPixelFormat::RGBA8Unorm);
        }
        let reference = device
            .device()
            .newRenderPipelineStateWithDescriptor_error(&reference_desc)
            .unwrap();
        let mut key = MetalRenderPipelineKey::new(0, 0);
        key.color_attachments[0].format = MTLPixelFormat::RGBA8Unorm;
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::RGBA8Unorm,
                32,
                32,
                false,
            )
        };
        descriptor.setUsage(MTLTextureUsage::RenderTarget);
        let target = device
            .device()
            .newTextureWithDescriptor(&descriptor)
            .unwrap();
        let pass = MTLRenderPassDescriptor::renderPassDescriptor();
        let attachment = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        attachment.setTexture(Some(&target));
        attachment.setLoadAction(MTLLoadAction::Clear);
        attachment.setStoreAction(MTLStoreAction::Store);
        let patches = MetalTessellationPatches {
            control_points: Arc::new(
                MetalBuffer::new(&device, layout.output_stride() * 3).unwrap(),
            ),
            patch_data: Arc::new(MetalBuffer::new(&device, layout.patch_stride()).unwrap()),
            capacity_per_instance: 1,
            instances: 1,
            output_vertices: 3,
            control_stride: layout.output_stride(),
            patch_stride: layout.patch_stride(),
        };
        patches
            .control_points
            .write(0, &vec![0; layout.output_stride() * 3])
            .unwrap();
        patches
            .patch_data
            .write(0, bytemuck::cast_slice(&[1.0f32; 6]))
            .unwrap();
        let assembler = MetalPrimitiveAssembler::new(&device).unwrap();
        for domain in [TessPrimitive::Triangles, TessPrimitive::Quads] {
            for spacing in [
                TessSpacing::Equal,
                TessSpacing::FractionalEven,
                TessSpacing::FractionalOdd,
            ] {
                let factors =
                    MetalTessellationFactorPipeline::new(&device, &layout, domain, spacing, 16)
                        .unwrap();
                for clockwise in [false, true] {
                    let runtime = RuntimeInfo {
                        tess_primitive: domain,
                        tess_clockwise: clockwise,
                        tess_spacing: spacing,
                        ..Default::default()
                    };
                    let native = MetalTessellationEvaluationPipeline::new(
                        &device,
                        &key,
                        &artifact,
                        &layout,
                        &runtime,
                        16,
                        Some(&fragment),
                    )
                    .unwrap();
                    for level in [1.0f32, 2.25, 3.5] {
                        patches
                            .patch_data
                            .write(0, bytemuck::cast_slice(&[level; 6]))
                            .unwrap();
                        for front in [MTLWinding::Clockwise, MTLWinding::CounterClockwise] {
                            for cull in [MTLCullMode::None, MTLCullMode::Back, MTLCullMode::Front] {
                                let mut results = Vec::new();
                                for tessellated in [false, true] {
                                    let mut scheduler = MetalScheduler::new(&device);
                                    let assembly = assembler
                                        .record_patches(
                                            &mut scheduler,
                                            MetalPrimitiveAssemblyParams {
                                                topology: PrimitiveTopology::Patches,
                                                count: 3,
                                                instances: 1,
                                                base_vertex: 0,
                                                index_bytes: 0,
                                                restart_index: None,
                                            },
                                            3,
                                            None,
                                            0,
                                        )
                                        .unwrap();
                                    let factors = factors
                                        .record(&mut scheduler, &assembly, &patches)
                                        .unwrap();
                                    scheduler.begin_render_pass(&pass).unwrap();
                                    scheduler.with_render_encoder(|encoder| unsafe {
                                encoder.setViewport(MTLViewport { originX: 0., originY: 0., width: 32.,
                                    height: 32., znear: 0., zfar: 1. });
                                encoder.setFrontFacingWinding(front);
                                encoder.setCullMode(cull);
                                if tessellated {
                                    native.bind(encoder, &patches, &factors).unwrap();
                                    encoder.drawPatches_patchIndexBuffer_patchIndexBufferOffset_indirectBuffer_indirectBufferOffset(
                                        3, None, 0, assembly.draw_arguments.handle(), 0);
                                } else {
                                    encoder.setRenderPipelineState(&reference);
                                    let clockwise = clockwise as u32;
                                    encoder.setVertexBytes_length_atIndex(NonNull::from(&clockwise).cast(), 4, 0);
                                    encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0,
                                        if domain == TessPrimitive::Triangles { 3 } else { 6 });
                                }
                            }).unwrap();
                                    let download = MetalBuffer::new(&device, 256 * 32).unwrap();
                                    scheduler.with_blit_encoder(|encoder| unsafe {
                                encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                                    &target, 0, 0, MTLOrigin { x: 0, y: 0, z: 0 },
                                    MTLSize { width: 32, height: 32, depth: 1 }, download.handle(), 0, 256, 256 * 32);
                            }).unwrap();
                                    scheduler.finish_all().unwrap();
                                    let mut probes = Vec::new();
                                    for (x, y) in [(8, 24), (20, 24), (8, 12), (20, 12)] {
                                        let mut pixel = [0; 4];
                                        download.read(y * 256 + x * 4, &mut pixel).unwrap();
                                        probes.push(pixel);
                                    }
                                    results.push(probes);
                                }
                                assert_eq!(results[1], results[0], "domain={domain:?} spacing={spacing:?} level={level} clockwise={clockwise} front={front:?} cull={cull:?}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn evaluation_pipeline_compiles_native_domains_spacing_and_binding_limits() {
        use shader_recompiler::backend::msl::emit_msl::emit_msl_tessellation_evaluation_function;
        let device = MetalDevice::new().unwrap();
        let mut program = Program::new(Stage::TessellationEval);
        program.add_block();
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        let mut ir = Emitter::new(&mut program, 0);
        let u = ir.get_attribute(Attribute::TESSELLATION_EVALUATION_POINT_U, Value::ImmU32(0));
        ir.set_attribute(Attribute::POSITION_X, u, Value::ImmU32(0));
        ir.set_attribute(Attribute::POSITION_W, Value::ImmF32(1.0), Value::ImmU32(0));
        collect_shader_info_pass(&mut program);
        let artifact = emit_msl_tessellation_evaluation_function(
            &program,
            &Profile::default(),
            &RuntimeInfo::default(),
            &MslOptions::default(),
            &mut Bindings::default(),
        )
        .unwrap();
        let mut control = Program::new(Stage::TessellationControl);
        control.invocations = 3;
        let layout = TessellationControlLayout::new(&control, &RuntimeInfo::default()).unwrap();
        let library = compile_msl_library(device.device(),
            "#include <metal_stdlib>\nusing namespace metal;\nfragment float4 shade() { return float4(1); }",
            MslVersion::V2_3).unwrap();
        let fragment = library
            .newFunctionWithName(&NSString::from_str("shade"))
            .unwrap();
        let mut key = MetalRenderPipelineKey::new(0, 0);
        key.color_attachments[0].format = objc2_metal::MTLPixelFormat::RGBA8Unorm;
        for primitive in [TessPrimitive::Triangles, TessPrimitive::Quads] {
            for spacing in [
                TessSpacing::Equal,
                TessSpacing::FractionalEven,
                TessSpacing::FractionalOdd,
            ] {
                for clockwise in [false, true] {
                    let runtime = RuntimeInfo {
                        tess_primitive: primitive,
                        tess_spacing: spacing,
                        tess_clockwise: clockwise,
                        ..Default::default()
                    };
                    MetalTessellationEvaluationPipeline::new(
                        &device,
                        &key,
                        &artifact,
                        &layout,
                        &runtime,
                        16,
                        Some(&fragment),
                    )
                    .unwrap();
                }
            }
        }
        let runtime = RuntimeInfo {
            tess_primitive: TessPrimitive::Triangles,
            ..Default::default()
        };
        let mut boundary = artifact.clone();
        boundary.bindings.buffer_count = 28;
        MetalTessellationEvaluationPipeline::new(
            &device,
            &key,
            &boundary,
            &layout,
            &runtime,
            16,
            Some(&fragment),
        )
        .unwrap();
        boundary.bindings.buffer_count = 29;
        assert!(matches!(
            MetalTessellationEvaluationPipeline::new(
                &device,
                &key,
                &boundary,
                &layout,
                &runtime,
                16,
                Some(&fragment)
            ),
            Err(MetalTessellationPipelineError::BufferSlots)
        ));
        assert!(matches!(
            MetalTessellationEvaluationPipeline::new(
                &device,
                &key,
                &artifact,
                &layout,
                &RuntimeInfo {
                    tess_primitive: TessPrimitive::Isolines,
                    ..runtime
                },
                16,
                Some(&fragment)
            ),
            Err(MetalTessellationPipelineError::Tessellator)
        ));
    }

    #[test]
    fn factor_conversion_preserves_boundaries_discard_and_sparse_instances() {
        let device = MetalDevice::new().unwrap();
        let assembler = MetalPrimitiveAssembler::new(&device).unwrap();
        let values = [
            0.0f32,
            -0.0,
            -1.0,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
            f32::from_bits(1),
            0.5,
            1.0,
            f32::from_bits(1.0f32.to_bits() + 1),
            2.0,
            f32::from_bits(2.0f32.to_bits() + 1),
            3.0,
            f32::from_bits(3.0f32.to_bits() + 1),
            4.0,
            f32::from_bits(4.0f32.to_bits() - 1),
            f32::from_bits(4.0f32.to_bits() + 1),
            15.9999,
            31.9999,
            32.00001,
            63.0,
            63.00001,
            64.0,
            65.0,
        ];
        // Each complete patch is followed by restart: allocation capacity is
        // larger than the compacted count used by both indirect dispatches.
        let indices: Vec<u32> = (0..values.len() as u32)
            .flat_map(|id| [id, id, id, u32::MAX])
            .collect();
        let upload_indices = MetalBuffer::new(&device, indices.len() * 4).unwrap();
        upload_indices
            .write(0, bytemuck::cast_slice(&indices))
            .unwrap();
        for generics in [false, true] {
            let mut control = Program::new(Stage::TessellationControl);
            control.invocations = 3;
            if generics {
                control.info.uses_patches[29] = true;
            }
            let layout = TessellationControlLayout::new(&control, &RuntimeInfo::default()).unwrap();
            for primitive in [TessPrimitive::Triangles, TessPrimitive::Quads] {
                let edges = if primitive == TessPrimitive::Triangles {
                    3
                } else {
                    4
                };
                let inners = if primitive == TessPrimitive::Triangles {
                    1
                } else {
                    2
                };
                for spacing in [
                    TessSpacing::Equal,
                    TessSpacing::FractionalEven,
                    TessSpacing::FractionalOdd,
                ] {
                    let pipeline = MetalTessellationFactorPipeline::new(
                        &device, &layout, primitive, spacing, 64,
                    )
                    .unwrap();
                    let mut scheduler = MetalScheduler::new(&device);
                    let assembly = assembler
                        .record_patches(
                            &mut scheduler,
                            MetalPrimitiveAssemblyParams {
                                topology: PrimitiveTopology::Patches,
                                count: indices.len() as u32,
                                base_vertex: 0,
                                instances: 2,
                                index_bytes: 4,
                                restart_index: Some(u32::MAX),
                            },
                            3,
                            Some((&upload_indices, 0)),
                            11,
                        )
                        .unwrap();
                    let capacity = indices.len() / 3;
                    let mut bytes = vec![0xcd; capacity * 2 * layout.patch_stride()];
                    for instance in 0..2 {
                        for patch in 0..values.len() {
                            let offset = (instance * capacity + patch) * layout.patch_stride();
                            for component in 0..6 {
                                let value = if component < 4 && instance == 1 {
                                    1.25 + component as f32 * 4.0
                                } else {
                                    values[(patch + component) % values.len()]
                                };
                                bytes[offset + component * 4..offset + component * 4 + 4]
                                    .copy_from_slice(&value.to_ne_bytes());
                            }
                        }
                    }
                    let upload = MetalBuffer::new(&device, bytes.len()).unwrap();
                    upload.write(0, &bytes).unwrap();
                    let patch_data =
                        Arc::new(MetalBuffer::new_private(&device, bytes.len()).unwrap());
                    upload
                        .encode_copy(&mut scheduler, &patch_data, 0, 0, bytes.len())
                        .unwrap();
                    let patches = MetalTessellationPatches {
                        control_points: Arc::new(MetalBuffer::new_private(&device, 4).unwrap()),
                        patch_data,
                        capacity_per_instance: capacity as u32,
                        instances: 2,
                        output_vertices: 3,
                        control_stride: layout.output_stride(),
                        patch_stride: layout.patch_stride(),
                    };
                    let factors = pipeline
                        .record(&mut scheduler, &assembly, &patches)
                        .unwrap();
                    assert_eq!(factors.instance_stride, capacity * (edges + inners) * 2);
                    let download = MetalBuffer::new(&device, factors.buffer.length()).unwrap();
                    factors
                        .buffer
                        .encode_copy(&mut scheduler, &download, 0, 0, download.length())
                        .unwrap();
                    drop((factors, patches, assembly, upload));
                    scheduler.finish_all().unwrap();
                    let mut bytes = vec![0; download.length()];
                    download.read(0, &mut bytes).unwrap();
                    for instance in 0..2 {
                        for patch in 0..values.len() {
                            for component in 0..edges + inners {
                                let outer = component < edges;
                                let source_component = if outer {
                                    component
                                } else {
                                    component - edges + 4
                                };
                                let value = if outer && instance == 1 {
                                    1.25 + component as f32 * 4.0
                                } else {
                                    values[(patch + source_component) % values.len()]
                                };
                                let offset = ((instance * capacity + patch) * (edges + inners)
                                    + component)
                                    * 2;
                                let bits = u16::from_ne_bytes(
                                    bytes[offset..offset + 2].try_into().unwrap(),
                                );
                                if outer && !(value > 0.0) {
                                    assert_eq!(bits, 0);
                                    continue;
                                }
                                if value.is_nan() {
                                    continue;
                                } // Inner NaN is unspecified by the guest API.
                                  // All defined nonzero factors are normal binary16 in [1,64].
                                let exponent = ((bits >> 10) & 31) as u32;
                                assert!((1..31).contains(&exponent), "primitive={primitive:?} spacing={spacing:?} instance={instance} patch={patch} component={component} value={value:?} half=0x{bits:04x}");
                                let actual = f32::from_bits(
                                    ((exponent + 112) << 23) | (u32::from(bits & 1023) << 13),
                                );
                                let (min, max, bias) = match spacing {
                                    TessSpacing::Equal => (1.0, 64.0, 0.0),
                                    TessSpacing::FractionalEven => (2.0, 64.0, 0.0),
                                    TessSpacing::FractionalOdd => (1.0, 63.0, 1.0),
                                };
                                let clamped = value.clamp(min, max);
                                if spacing == TessSpacing::Equal {
                                    assert_eq!(actual, clamped.ceil(), "value={value}");
                                } else {
                                    assert_eq!(
                                        ((actual - bias) / 2.0).ceil(),
                                        ((clamped - bias) / 2.0).ceil(),
                                        "value={value}"
                                    );
                                    assert!(
                                        (actual - clamped).abs() <= clamped / 1024.0,
                                        "fractional value lost: {value} -> {actual}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn factor_pipeline_rejects_unsupported_domain_and_limit() {
        let device = MetalDevice::new().unwrap();
        let mut control = Program::new(Stage::TessellationControl);
        control.invocations = 3;
        let layout = TessellationControlLayout::new(&control, &RuntimeInfo::default()).unwrap();
        for (primitive, spacing, max) in [
            (TessPrimitive::Isolines, TessSpacing::Equal, 64),
            (TessPrimitive::Triangles, TessSpacing::Equal, 0),
            (TessPrimitive::Triangles, TessSpacing::Equal, 128),
            (TessPrimitive::Quads, TessSpacing::FractionalOdd, 63),
        ] {
            assert!(matches!(
                MetalTessellationFactorPipeline::new(&device, &layout, primitive, spacing, max),
                Err(MetalTessellationPipelineError::Tessellator)
            ));
        }
        assert!(MetalTessellationFactorPipeline::new(
            &device,
            &layout,
            TessPrimitive::Triangles,
            TessSpacing::Equal,
            63
        )
        .is_ok());
    }

    #[test]
    fn control_runtime_preserves_sparse_instance_regions_resources_and_patch_counts() {
        let device = MetalDevice::new().unwrap();
        let mut vertex = Program::new(Stage::VertexB);
        vertex.add_block();
        vertex.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
        let mut ir = Emitter::new(&mut vertex, 0);
        for (component, attribute) in [Attribute::VERTEX_ID, Attribute::INSTANCE_ID]
            .into_iter()
            .enumerate()
        {
            let value = ir.get_attribute_u32(attribute, Value::ImmU32(0));
            let value = ir.bit_cast_f32_u32(value);
            ir.set_attribute(
                Attribute::generic(7, component as u32),
                value,
                Value::ImmU32(0),
            );
        }
        collect_shader_info_pass(&mut vertex);
        let artifact = emit_msl_vertex_function(
            &vertex,
            &Profile {
                support_vertex_instance_id: true,
                ..Default::default()
            },
            &RuntimeInfo::default(),
            &MslOptions::default(),
            &mut Bindings::default(),
        )
        .unwrap();
        let runtime = RuntimeInfo {
            previous_stage_stores: vertex.info.stores,
            ..Default::default()
        };
        let vertex_pipeline = MetalGeometryVertexPipeline::new(
            &device,
            &artifact,
            &MetalVertexInputState::default(),
            &runtime,
        )
        .unwrap();
        let assembler = MetalPrimitiveAssembler::new(&device).unwrap();

        for invocations in [1, 3, 32] {
            let mut program = Program::new(Stage::TessellationControl);
            program.invocations = invocations;
            for _ in 0..3 {
                program.add_block();
            }
            let mut ir = Emitter::new(&mut program, 0);
            let id = ir.invocation_id();
            let input = ir.get_attribute(Attribute::generic(7, 0), Value::ImmU32(0));
            let instance = ir.get_attribute(Attribute::generic(7, 1), Value::ImmU32(0));
            let constant = ir.get_cbuf_f32(Value::ImmU32(0), Value::ImmU32(0));
            ir.set_attribute(Attribute::generic(3, 0), input, Value::ImmU32(31));
            ir.set_attribute(Attribute::generic(3, 1), instance, Value::ImmU32(31));
            ir.set_attribute(Attribute::generic(3, 2), constant, Value::ImmU32(31));
            ir.set_attribute(
                Attribute(Attribute::CLIP_DISTANCE_0.0 + 2),
                constant,
                Value::ImmU32(31),
            );
            let bits = ir.bit_cast_f32_u32(id);
            ir.set_attribute(Attribute::generic(3, 3), bits, Value::ImmU32(31));
            let info = ir.invocation_info();
            let info = ir.bit_cast_f32_u32(info);
            ir.set_attribute(Attribute::POSITION_X, info, Value::ImmU32(0));
            let patch_id = ir.get_attribute(Attribute::PRIMITIVE_ID, Value::ImmU32(0));
            ir.set_attribute(Attribute::POSITION_Y, patch_id, Value::ImmU32(0));
            ir.barrier();
            let first = ir.i_equal(id, Value::ImmU32(0));
            let condition = ir.condition_ref(first);
            let mut ir = Emitter::new(&mut program, 1);
            ir.set_patch(Patch::TESS_LOD_LEFT, constant);
            ir.set_patch(Patch::generic(2, 0), input);
            ir.set_patch(Patch::generic(2, 1), instance);
            program.blocks[0].append_new_inst(
                Opcode::StorageAtomicIAdd32,
                vec![Value::ImmU32(0), Value::ImmU32(0), Value::ImmU32(1)],
            );
            program.syntax_list = vec![
                SyntaxNode::Block(0),
                SyntaxNode::If {
                    cond: condition,
                    body: 1,
                    merge: 2,
                },
                SyntaxNode::Block(1),
                SyntaxNode::EndIf { merge: 2 },
                SyntaxNode::Block(2),
                SyntaxNode::Return,
            ];
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
            let artifact = emit_msl_tessellation_control_function(
                &program,
                &Profile {
                    max_user_clip_distances: 8,
                    ..Default::default()
                },
                &runtime,
                &MslOptions::default(),
                &mut Bindings::default(),
            )
            .unwrap();
            assert_eq!(artifact.bindings.buffer_count, 2);
            let layout = TessellationControlLayout::new(&program, &runtime).unwrap();
            let pipeline =
                MetalTessellationControlPipeline::new(&device, &artifact, &layout).unwrap();

            for input_vertices in [3, 32] {
                for (indices, instances) in [
                    (vec![], 2),
                    (vec![u32::MAX; 35], 2),
                    ((0..96).collect(), 0),
                    (
                        (0..96)
                            .map(|id| {
                                if matches!(id, 5 | 19 | 53 | 95) {
                                    u32::MAX
                                } else {
                                    id
                                }
                            })
                            .collect(),
                        2,
                    ),
                ] {
                    let mut scheduler = MetalScheduler::new(&device);
                    let upload = MetalBuffer::new(&device, indices.len() * 4).unwrap();
                    upload.write(0, bytemuck::cast_slice(&indices)).unwrap();
                    let patches = assembler
                        .record_patches(
                            &mut scheduler,
                            MetalPrimitiveAssemblyParams {
                                topology: PrimitiveTopology::Patches,
                                count: indices.len() as u32,
                                base_vertex: -2,
                                instances,
                                index_bytes: 4,
                                restart_index: Some(u32::MAX),
                            },
                            input_vertices,
                            Some((&upload, 0)),
                            11,
                        )
                        .unwrap();
                    let vertices = vertex_pipeline
                        .record_stream(
                            &mut scheduler,
                            &patches.stream,
                            patches.base_instance,
                            &[0; 31],
                            |_| {},
                        )
                        .unwrap();
                    let cbuf = MetalBuffer::new(&device, 16).unwrap();
                    cbuf.write(0, bytemuck::cast_slice(&[2.5f32, 0.0, 0.0, 0.0]))
                        .unwrap();
                    let counter = MetalBuffer::new(&device, 4).unwrap();
                    counter.write(0, &0u32.to_ne_bytes()).unwrap();
                    let output = pipeline
                        .record(&mut scheduler, &patches, &vertices, |encoder| unsafe {
                            encoder.setBuffer_offset_atIndex(Some(cbuf.handle()), 0, 0);
                            encoder.setBuffer_offset_atIndex(Some(counter.handle()), 0, 1);
                        })
                        .unwrap();
                    assert_eq!(output.control_stride, 80);
                    assert_eq!(output.patch_stride, 48);
                    let points = MetalBuffer::new(&device, output.control_points.length()).unwrap();
                    let data = MetalBuffer::new(&device, output.patch_data.length()).unwrap();
                    output
                        .control_points
                        .encode_copy(&mut scheduler, &points, 0, 0, points.length())
                        .unwrap();
                    output
                        .patch_data
                        .encode_copy(&mut scheduler, &data, 0, 0, data.length())
                        .unwrap();
                    let capacity = output.capacity_per_instance as usize;
                    drop((output, vertices, patches, cbuf, upload));
                    scheduler.finish_all().unwrap();
                    let mut counter_bytes = [0; 4];
                    counter.read(0, &mut counter_bytes).unwrap();
                    let expected: Vec<_> = indices
                        .split(|v| *v == u32::MAX)
                        .flat_map(|segment| segment.chunks_exact(input_vertices as usize))
                        .map(|patch| patch[0].wrapping_sub(2))
                        .collect();
                    assert_eq!(
                        u32::from_ne_bytes(counter_bytes),
                        expected.len() as u32 * invocations * instances
                    );
                    let mut point_bytes = vec![0; points.length()];
                    let mut patch_bytes = vec![0; data.length()];
                    points.read(0, &mut point_bytes).unwrap();
                    data.read(0, &mut patch_bytes).unwrap();
                    let word = |bytes: &[u8], offset| {
                        u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap())
                    };
                    for instance in 0..instances as usize {
                        for (patch, expected_id) in expected.iter().enumerate() {
                            let index = instance * capacity + patch;
                            assert_eq!(word(&patch_bytes, index * 48), 2.5f32.to_bits());
                            assert_eq!(word(&patch_bytes, index * 48 + 32), *expected_id);
                            assert_eq!(word(&patch_bytes, index * 48 + 36), 11 + instance as u32);
                            for invocation in 0..invocations as usize {
                                let offset = (index * invocations as usize + invocation) * 80;
                                assert_eq!(word(&point_bytes, offset), input_vertices << 16);
                                assert_eq!(word(&point_bytes, offset + 4), patch as u32);
                                for clip in 0..8 {
                                    assert_eq!(
                                        word(&point_bytes, offset + 20 + clip * 4),
                                        if clip == 2 { 2.5f32.to_bits() } else { 0 }
                                    );
                                }
                                assert_eq!(word(&point_bytes, offset + 64), *expected_id);
                                assert_eq!(word(&point_bytes, offset + 68), 11 + instance as u32);
                                assert_eq!(word(&point_bytes, offset + 72), 2.5f32.to_bits());
                                assert_eq!(word(&point_bytes, offset + 76), invocation as u32);
                            }
                        }
                    }
                }
            }
        }
    }
}
