// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GPU capture and primitive replay for geometry Layer/ViewportIndex outputs.
//! Eden uses a hardware geometry stage. This native Metal transport executes
//! that guest stage once and preserves its emitted order without CPU readback.

use super::metal_buffer::MetalBuffer;
use super::metal_device::MetalDevice;
use super::metal_geometry_pipeline::{
    bind_vertex_resources, MetalGeometryPipelineError, MetalGeometryVertices,
};
use super::metal_graphics_pipeline::MetalPreparedStage;
use super::metal_primitive_assembler::MetalPrimitiveAssembly;
use super::metal_scheduler::MetalScheduler;
use super::metal_shader::{compile_msl_library, MetalShaderError};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBarrierScope, MTLComputeCommandEncoder, MTLComputePipelineState, MTLDevice, MTLFunction,
    MTLLibrary, MTLPipelineOption, MTLRenderCommandEncoder, MTLSize,
};
use shader_recompiler::backend::msl::{
    emit_msl_geometry::GeometryLayout,
    msl_function::{MslFunctionKind, MslParameterAttribute},
    MslShaderArtifact,
};
use shader_recompiler::ir::types::OutputTopology;
use std::ptr::NonNull;
use std::sync::Arc;

pub struct MetalGeometryCapturedDraw {
    records: Arc<MetalBuffer>,
    arguments: Arc<MetalBuffer>,
}

pub struct MetalGeometryCapture {
    device: MetalDevice,
    initialize: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    capture: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    pub object: Retained<ProtocolObject<dyn MTLFunction>>,
    pub mesh: Retained<ProtocolObject<dyn MTLFunction>>,
    _library: Retained<ProtocolObject<dyn MTLLibrary>>,
    extra_buffer: u32,
    record_stride: usize,
    invocations: u32,
    vertex_stride: usize,
    generic_mask: u32,
    input_vertices: u32,
}

impl MetalGeometryCapture {
    pub fn new(
        device: &MetalDevice,
        artifact: &MslShaderArtifact,
        layout: &GeometryLayout,
        runtime: &shader_recompiler::runtime_info::RuntimeInfo,
        invocations: u32,
    ) -> Result<Self, MetalGeometryPipelineError> {
        super::metal_shader::validate_native_binding_layout(device.profile(), &artifact.bindings)?;
        if !layout.has_primitive_outputs() {
            return Err(MetalGeometryPipelineError::Layout);
        }
        if layout.input_vertices != runtime.input_topology.vertices() {
            return Err(MetalGeometryPipelineError::Layout);
        }
        if !(1..=1024).contains(&invocations) {
            return Err(MetalGeometryPipelineError::Invocations(invocations));
        }
        let interface = artifact
            .interface
            .as_ref()
            .filter(|i| i.kind == MslFunctionKind::GeometryFunction)
            .ok_or(MetalGeometryPipelineError::Layout)?;
        let base = artifact.bindings.buffer_count;
        if base + 5 > 31 {
            return Err(MetalGeometryPipelineError::BufferSlots);
        }
        let mut parameters = Vec::new();
        let mut arguments = Vec::new();
        for p in &interface.parameters {
            match p.attribute {
                MslParameterAttribute::Buffer(_)
                | MslParameterAttribute::Texture(_)
                | MslParameterAttribute::Sampler(_) => {
                    parameters.push(p.declaration(MslFunctionKind::StageEntryPoint));
                    arguments.push(p.name.clone());
                }
                MslParameterAttribute::Builtin("payload") => arguments.push("input".into()),
                MslParameterAttribute::Builtin("threadgroup_position_in_grid") => {
                    arguments.push(format!("uint3(group.x % {invocations}u, 0u, 0u)"));
                }
                MslParameterAttribute::None if p.name == "geometry_mesh" => {
                    arguments.push("output".into())
                }
                _ => return Err(MetalGeometryPipelineError::Layout),
            }
        }
        let resource_parameters = if parameters.is_empty() {
            String::new()
        } else {
            format!("{},", parameters.join(", "))
        };
        let (width, topology) = match layout.topology {
            OutputTopology::PointList => (1, "point"),
            OutputTopology::LineStrip => (2, "line"),
            OutputTopology::TriangleStrip => (3, "triangle"),
        };
        let source = format!(
            r#"{guest}
struct GeometryCaptured {{
    MslVertexOut vertices[{nv}];
    uint indices[{ni}];
    MslGeometryPrimitiveOut primitives[{np}];
    uint count;
    void set_vertex(uint i, MslVertexOut v) thread {{ vertices[i] = v; }}
    void set_index(uint i, uchar v) thread {{ indices[i] = v; }}
    void set_primitive(uint i, MslGeometryPrimitiveOut p) thread {{ primitives[i] = p; }}
    void set_primitive_count(uint n) thread {{ count = n; }}
}};
kernel void geometry_capture_arguments(const device uint* input [[buffer(0)]],
    device uint* output [[buffer(1)]]) {{
    output[0] = input[0] * {invocations}u;
    output[1] = input[1]; output[2] = 1u;
}}
kernel void geometry_capture({resource_parameters}
    const device MslGeometryVertexIn* vertices [[buffer({base})]],
    const device uint* primitives [[buffer({primitives_binding})]],
    const device uint* dispatch [[buffer({dispatch_binding})]],
    constant uint& vertex_count [[buffer({params_binding})]],
    device GeometryCaptured* records [[buffer({records_binding})]],
    uint3 group [[threadgroup_position_in_grid]]) {{
    uint primitive = group.x / {invocations}u;
    MslGeometryPayload input = {{}};
    for (uint i = 0; i < {input_vertices}u; ++i) {{
        uint ordinal = primitives[ulong(primitive) * 6u + i];
        input.vertices[i] = vertices[ulong(group.y) * vertex_count + ordinal];
    }}
    input.primitive_id = primitive;
    GeometryCaptured output = {{}};
    ruzu_geometry({arguments});
    records[ulong(group.y) * dispatch[0] + group.x] = output;
}}
struct GeometryReplayPayload {{ ulong record; }};
[[object, max_total_threads_per_threadgroup(1)]]
void geometry_replay_object(object_data GeometryReplayPayload& payload [[payload]],
    mesh_grid_properties grid, const device GeometryCaptured* records [[buffer(0)]],
    const device uint* dispatch [[buffer(1)]], uint3 group [[threadgroup_position_in_grid]]) {{
    payload.record = ulong(group.y) * dispatch[0] + group.x;
    grid.set_threadgroups_per_grid(uint3(records[payload.record].count, 1u, 1u));
}}
using GeometryReplayMesh = metal::mesh<MslVertexOut, MslGeometryPrimitiveOut, {width}, 1, metal::topology::{topology}>;
[[mesh, max_total_threads_per_threadgroup(1)]]
void geometry_replay_mesh(const object_data GeometryReplayPayload& payload [[payload]],
    const device GeometryCaptured* records [[buffer(0)]],
    uint3 group [[threadgroup_position_in_grid]], GeometryReplayMesh mesh) {{
    const device GeometryCaptured& record = records[payload.record];
    for (uint i = 0; i < {width}u; ++i) {{
        mesh.set_vertex(i, record.vertices[record.indices[group.x * {width}u + i]]);
        mesh.set_index(i, i);
    }}
    mesh.set_primitive(0, record.primitives[group.x]);
    mesh.set_primitive_count(1);
}}
"#,
            guest = artifact.source.source,
            nv = layout.output_vertices,
            np = layout.output_primitives,
            ni = layout.output_primitives * width,
            primitives_binding = base + 1,
            dispatch_binding = base + 2,
            params_binding = base + 3,
            records_binding = base + 4,
            input_vertices = layout.input_vertices,
            arguments = arguments.join(", ")
        );
        let library = compile_msl_library(device.device(), &source, artifact.language_version)?;
        let function = |name: &str| {
            library
                .newFunctionWithName(&NSString::from_str(name))
                .ok_or_else(|| MetalShaderError::MissingEntryPoint(name.into()))
        };
        let initialize_function = function("geometry_capture_arguments")?;
        let capture_function = function("geometry_capture")?;
        let initialize = device
            .device()
            .newComputePipelineStateWithFunction_error(&initialize_function)
            .map_err(|e| MetalGeometryPipelineError::Pipeline(e.to_string()))?;
        let mut reflection = None;
        // The compiler owns the record ABI, including padding of stage outputs.
        // Reflection avoids guessing the stride or launching a CPU layout query.
        let capture = unsafe {
            device
                .device()
                .newComputePipelineStateWithFunction_options_reflection_error(
                    &capture_function,
                    MTLPipelineOption::BindingInfo | MTLPipelineOption::BufferTypeInfo,
                    Some(&mut reflection),
                )
        }
        .map_err(|e| MetalGeometryPipelineError::Pipeline(e.to_string()))?;
        // arguments also supports the backend's macOS 13 deployment target.
        #[allow(deprecated)]
        let (record_size, record_alignment) = reflection
            .ok_or(MetalGeometryPipelineError::Layout)?
            .arguments()
            .iter()
            .find(|a| a.r#type() == objc2_metal::MTLArgumentType::Buffer && a.index() == (base + 4) as usize)
            .map(|a| (a.bufferDataSize(), a.bufferAlignment()))
            .filter(|&(size, alignment)| size != 0 && alignment != 0)
            .ok_or(MetalGeometryPipelineError::Layout)?;
        let record_stride = record_size.checked_add(record_alignment - 1)
            .map(|size| size / record_alignment * record_alignment)
            .ok_or(MetalGeometryPipelineError::OutputSize)?;
        Ok(Self {
            device: device.clone(),
            initialize,
            capture,
            object: function("geometry_replay_object")?,
            mesh: function("geometry_replay_mesh")?,
            _library: library,
            extra_buffer: base,
            record_stride,
            invocations,
            vertex_stride: GeometryLayout::vertex_stride(runtime),
            generic_mask: GeometryLayout::generic_mask(runtime),
            input_vertices: layout.input_vertices,
        })
    }

    pub fn record(
        &self,
        scheduler: &mut MetalScheduler,
        assembly: &MetalPrimitiveAssembly,
        vertices: &MetalGeometryVertices,
        resources: &MetalPreparedStage,
    ) -> Result<MetalGeometryCapturedDraw, MetalGeometryPipelineError> {
        vertices.validate(
            assembly,
            self.vertex_stride,
            self.generic_mask,
            self.input_vertices,
        )?;
        let p = assembly.params();
        let groups = p
            .count
            .checked_mul(self.invocations)
            .ok_or(MetalGeometryPipelineError::OutputSize)?;
        let records_size = (groups as usize)
            .checked_mul(p.instances as usize)
            .and_then(|n| n.max(1).checked_mul(self.record_stride))
            .ok_or(MetalGeometryPipelineError::OutputSize)?;
        let records = Arc::new(MetalBuffer::new_private(&self.device, records_size)?);
        let arguments = Arc::new(MetalBuffer::new_private(&self.device, 12)?);
        let one = MTLSize {
            width: 1,
            height: 1,
            depth: 1,
        };
        scheduler.with_compute_encoder(|encoder| unsafe {
            encoder.setComputePipelineState(&self.initialize);
            encoder.setBuffer_offset_atIndex(Some(assembly.dispatch_arguments.handle()), 0, 0);
            encoder.setBuffer_offset_atIndex(Some(arguments.handle()), 0, 1);
            encoder.dispatchThreads_threadsPerThreadgroup(one, one);
            encoder.memoryBarrierWithScope(MTLBarrierScope::Buffers);
            encoder.setComputePipelineState(&self.capture);
            bind_vertex_resources(encoder, resources);
            let b = self.extra_buffer as usize;
            encoder.setBuffer_offset_atIndex(Some(vertices.buffer.handle()), 0, b);
            encoder.setBuffer_offset_atIndex(Some(assembly.primitives.handle()), 0, b + 1);
            encoder.setBuffer_offset_atIndex(Some(arguments.handle()), 0, b + 2);
            encoder.setBytes_length_atIndex(NonNull::from(&p.count).cast(), 4, b + 3);
            encoder.setBuffer_offset_atIndex(Some(records.handle()), 0, b + 4);
            encoder
                .dispatchThreadgroupsWithIndirectBuffer_indirectBufferOffset_threadsPerThreadgroup(
                    arguments.handle(),
                    0,
                    one,
                );
            encoder.memoryBarrierWithScope(MTLBarrierScope::Buffers);
        })?;
        Ok(MetalGeometryCapturedDraw { records, arguments })
    }

    pub fn record_draw(
        &self,
        encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
        draw: &MetalGeometryCapturedDraw,
    ) {
        let one = MTLSize {
            width: 1,
            height: 1,
            depth: 1,
        };
        // Buffers use tracked hazards and are retained by the command buffer.
        unsafe {
            encoder.setObjectBuffer_offset_atIndex(Some(draw.records.handle()), 0, 0);
            encoder.setObjectBuffer_offset_atIndex(Some(draw.arguments.handle()), 0, 1);
            encoder.setMeshBuffer_offset_atIndex(Some(draw.records.handle()), 0, 0);
            encoder.drawMeshThreadgroupsWithIndirectBuffer_indirectBufferOffset_threadsPerObjectThreadgroup_threadsPerMeshThreadgroup(
                draw.arguments.handle(), 0, one, one);
        }
    }
}
