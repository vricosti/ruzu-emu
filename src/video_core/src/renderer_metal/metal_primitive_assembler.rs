// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GPU input assembly for native raster and object/mesh geometry emulation. Eden delegates
//! this operation to Vulkan input assembly (vk_pipeline_cache.cpp topology).
//! Primitive records contain stream ordinals, not fetched vertex IDs, so strip
//! vertices can share one preceding-stage result. No guest-memory readback occurs.
//! Vulkan Geometry Shader Input Primitives permits cyclic triangle input order
//! variations that preserve winding. Input order is not the rasterization
//! provoking-vertex selection performed later on the emitted mesh primitives.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBarrierScope, MTLCompileOptions, MTLComputeCommandEncoder, MTLComputePipelineState,
    MTLDevice, MTLLanguageVersion, MTLLibrary, MTLSize,
};
use shader_recompiler::runtime_info::InputTopology;
use thiserror::Error;

use super::metal_buffer::{MetalBuffer, MetalBufferError};
use super::metal_device::MetalDevice;
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};
use crate::engines::maxwell_3d::PrimitiveTopology;

const SCAN_WIDTH: u32 = 256;
pub const PRIMITIVE_RECORD_WORDS: usize = 6;

#[derive(Debug, Error)]
pub enum MetalPrimitiveAssemblyError {
    #[error("unsupported unconverted geometry input topology {0:?}")]
    Topology(PrimitiveTopology),
    #[error("geometry index width must be 0, 1, 2 or 4 bytes, got {0}")]
    IndexWidth(u32),
    #[error("geometry index buffer range is missing or out of bounds")]
    IndexRange,
    #[error("geometry input count exceeds addressable native buffer range")]
    Size,
    #[error("Metal primitive assembly compilation failed: {0}")]
    Compile(String),
    #[error(transparent)]
    Buffer(#[from] MetalBufferError),
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
}

#[derive(Clone, Copy, Debug)]
pub struct MetalPrimitiveAssemblyParams {
    pub topology: PrimitiveTopology,
    pub count: u32,
    pub base_vertex: i32,
    pub instances: u32,
    /// Zero means an array draw; other values describe the supplied native buffer.
    pub index_bytes: u32,
    /// Compared with the raw index, before adding base_vertex. Ignored for arrays.
    pub restart_index: Option<u32>,
}

impl MetalPrimitiveAssemblyParams {
    pub fn input_topology(self) -> Result<InputTopology, MetalPrimitiveAssemblyError> {
        Ok(match self.topology {
            PrimitiveTopology::Points => InputTopology::Points,
            PrimitiveTopology::Lines
            | PrimitiveTopology::LineStrip
            | PrimitiveTopology::LineLoop => InputTopology::Lines,
            PrimitiveTopology::Triangles
            | PrimitiveTopology::TriangleStrip
            | PrimitiveTopology::TriangleFan => InputTopology::Triangles,
            PrimitiveTopology::LinesAdjacency | PrimitiveTopology::LineStripAdjacency => {
                InputTopology::LinesAdjacency
            }
            PrimitiveTopology::TrianglesAdjacency | PrimitiveTopology::TriangleStripAdjacency => {
                InputTopology::TrianglesAdjacency
            }
            topology => return Err(MetalPrimitiveAssemblyError::Topology(topology)),
        })
    }

    fn words(self) -> [u32; 8] {
        [
            self.count,
            self.index_bytes,
            self.base_vertex as u32,
            u32::from(self.index_bytes != 0 && self.restart_index.is_some()),
            self.restart_index.unwrap_or(0),
            self.topology as u32,
            self.instances,
            0,
        ]
    }
}

#[derive(Clone)]
pub struct MetalPrimitiveAssembly {
    params: MetalPrimitiveAssemblyParams,
    /// One raw-bit-preserving vertex index per input stream entry.
    pub vertex_ids: Arc<MetalBuffer>,
    /// uint2 records: X is the segment start; X == ordinal + 1 marks restart.
    pub segments: Arc<MetalBuffer>,
    /// Six u32 ordinals per primitive, unused fields zero. Array index is PrimitiveId.
    pub primitives: Arc<MetalBuffer>,
    /// MTLDispatchThreadgroupsIndirectArguments: primitive count, instances, one.
    pub dispatch_arguments: Arc<MetalBuffer>,
    pub input_topology: InputTopology,
}

/// Packed triangle-list indices and MTLDrawIndexedPrimitivesIndirectArguments.
/// The GPU determines the count after primitive restart; the host never reads it.
pub struct MetalTriangleFanDraw {
    pub indices: Arc<MetalBuffer>,
    pub arguments: Arc<MetalBuffer>,
}

impl MetalPrimitiveAssembly {
    pub fn params(&self) -> MetalPrimitiveAssemblyParams {
        self.params
    }
}

pub struct MetalPrimitiveAssembler {
    device: MetalDevice,
    initialize: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    scan: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    add: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    classify: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    emit: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    fan_indices: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    null_index: MetalBuffer,
}

impl MetalPrimitiveAssembler {
    pub fn new(device: &MetalDevice) -> Result<Self, MetalPrimitiveAssemblyError> {
        let source = format!("#define SCAN_WIDTH {SCAN_WIDTH}u\n{ASSEMBLY_SOURCE}");
        let options = MTLCompileOptions::new();
        options.setLanguageVersion(MTLLanguageVersion::Version2_3);
        let library = device
            .device()
            .newLibraryWithSource_options_error(&NSString::from_str(&source), Some(&options))
            .map_err(|e| MetalPrimitiveAssemblyError::Compile(e.to_string()))?;
        let pipeline = |name: &str| {
            let function = library
                .newFunctionWithName(&NSString::from_str(name))
                .ok_or_else(|| MetalPrimitiveAssemblyError::Compile(format!("missing {name}")))?;
            device
                .device()
                .newComputePipelineStateWithFunction_error(&function)
                .map_err(|e| MetalPrimitiveAssemblyError::Compile(e.to_string()))
        };
        Ok(Self {
            device: device.clone(),
            initialize: pipeline("assembly_initialize")?,
            scan: pipeline("assembly_scan")?,
            add: pipeline("assembly_add")?,
            classify: pipeline("assembly_classify")?,
            emit: pipeline("assembly_emit")?,
            fan_indices: pipeline("assembly_fan_indices")?,
            null_index: MetalBuffer::new(device, 4)?,
        })
    }

    pub fn record_triangle_fan_draw(
        &self,
        scheduler: &mut MetalScheduler,
        params: MetalPrimitiveAssemblyParams,
        index: Option<(&MetalBuffer, usize)>,
        base_instance: u32,
        provoking_vertex_last: bool,
    ) -> Result<MetalTriangleFanDraw, MetalPrimitiveAssemblyError> {
        if params.topology != PrimitiveTopology::TriangleFan {
            return Err(MetalPrimitiveAssemblyError::Topology(params.topology));
        }
        let max_indices = params
            .count
            .saturating_sub(2)
            .checked_mul(3)
            .ok_or(MetalPrimitiveAssemblyError::Size)?;
        let size = (max_indices.max(1) as usize)
            .checked_mul(4)
            .ok_or(MetalPrimitiveAssemblyError::Size)?;
        let indices = Arc::new(MetalBuffer::new_private(&self.device, size)?);
        let arguments = Arc::new(MetalBuffer::new_private(&self.device, 5 * 4)?);
        let assembly = self.record(scheduler, params, index)?;
        let words = [
            params.base_vertex as u32,
            base_instance,
            u32::from(provoking_vertex_last),
        ];
        scheduler.with_compute_encoder(|encoder| {
            encoder.setComputePipelineState(&self.fan_indices);
            bind(encoder, &assembly.vertex_ids, 0, 0);
            bind(encoder, &assembly.primitives, 0, 1);
            bind(encoder, &assembly.dispatch_arguments, 0, 2);
            bind(encoder, &indices, 0, 3);
            bind(encoder, &arguments, 0, 4);
            bind_words(encoder, &words, 5);
            dispatch(encoder, params.count.saturating_sub(2).max(1));
        })?;
        Ok(MetalTriangleFanDraw { indices, arguments })
    }

    pub fn record(
        &self,
        scheduler: &mut MetalScheduler,
        params: MetalPrimitiveAssemblyParams,
        index: Option<(&MetalBuffer, usize)>,
    ) -> Result<MetalPrimitiveAssembly, MetalPrimitiveAssemblyError> {
        let input_topology = params.input_topology()?;
        if !matches!(params.index_bytes, 0 | 1 | 2 | 4) {
            return Err(MetalPrimitiveAssemblyError::IndexWidth(params.index_bytes));
        }
        let (index_buffer, index_offset) = if params.index_bytes == 0 {
            (&self.null_index, 0)
        } else {
            let (buffer, offset) = index.ok_or(MetalPrimitiveAssemblyError::IndexRange)?;
            let bytes = (params.count as usize)
                .checked_mul(params.index_bytes as usize)
                .and_then(|size| offset.checked_add(size))
                .ok_or(MetalPrimitiveAssemblyError::IndexRange)?;
            if bytes > buffer.length() {
                return Err(MetalPrimitiveAssemblyError::IndexRange);
            }
            (buffer, offset)
        };
        let buffer = |stride: usize| -> Result<Arc<MetalBuffer>, MetalPrimitiveAssemblyError> {
            // Metal validates a typed pointer's minimum extent even when the
            // empty-draw path never dereferences it.
            let size = (params.count.max(1) as usize)
                .checked_mul(stride)
                .ok_or(MetalPrimitiveAssemblyError::Size)?;
            Ok(Arc::new(MetalBuffer::new_private(&self.device, size)?))
        };
        let vertex_ids = buffer(4)?;
        let segments = buffer(8)?;
        let counts = buffer(8)?;
        let primitives = buffer(PRIMITIVE_RECORD_WORDS * 4)?;
        let dispatch_arguments = Arc::new(MetalBuffer::new_private(&self.device, 12)?);
        let words = params.words();
        // An empty range may start at buffer.length(); Metal cannot bind that
        // offset, and there are no input vertices to initialize or classify.
        if params.count != 0 {
            scheduler.with_compute_encoder(|encoder| {
                encoder.setComputePipelineState(&self.initialize);
                bind(encoder, index_buffer, index_offset, 0);
                bind(encoder, &vertex_ids, 0, 1);
                bind(encoder, &segments, 0, 2);
                bind_words(encoder, &words, 3);
                dispatch(encoder, params.count);
            })?;
            self.scan_prefix(scheduler, &segments, params.count)?;
            scheduler.with_compute_encoder(|encoder| {
                encoder.setComputePipelineState(&self.classify);
                bind(encoder, &segments, 0, 0);
                bind(encoder, &counts, 0, 1);
                bind_words(encoder, &words, 2);
                dispatch(encoder, params.count);
            })?;
            self.scan_prefix(scheduler, &counts, params.count)?;
        }
        scheduler.with_compute_encoder(|encoder| {
            encoder.setComputePipelineState(&self.emit);
            bind(encoder, &segments, 0, 0);
            bind(encoder, &counts, 0, 1);
            bind(encoder, &primitives, 0, 2);
            bind(encoder, &dispatch_arguments, 0, 3);
            bind_words(encoder, &words, 4);
            // Even an empty draw must write a zero X dimension to the indirect buffer.
            dispatch(encoder, params.count.max(1));
        })?;
        Ok(MetalPrimitiveAssembly {
            params,
            vertex_ids,
            segments,
            primitives,
            dispatch_arguments,
            input_topology,
        })
    }

    fn scan_prefix(
        &self,
        scheduler: &mut MetalScheduler,
        values: &MetalBuffer,
        count: u32,
    ) -> Result<(), MetalPrimitiveAssemblyError> {
        if count == 0 {
            return Ok(());
        }
        let groups = count.div_ceil(SCAN_WIDTH);
        let totals = MetalBuffer::new_private(&self.device, groups as usize * 8)?;
        scheduler.with_compute_encoder(|encoder| {
            encoder.setComputePipelineState(&self.scan);
            bind(encoder, values, 0, 0);
            bind(encoder, &totals, 0, 1);
            bind_words(encoder, &[count], 2);
            dispatch(encoder, count);
        })?;
        if groups > 1 {
            self.scan_prefix(scheduler, &totals, groups)?;
            scheduler.with_compute_encoder(|encoder| {
                encoder.setComputePipelineState(&self.add);
                bind(encoder, values, 0, 0);
                bind(encoder, &totals, 0, 1);
                bind_words(encoder, &[count], 2);
                dispatch(encoder, count);
            })?;
        }
        // Command buffers retain directly bound buffers until GPU completion.
        Ok(())
    }
}

fn bind(
    encoder: &ProtocolObject<dyn MTLComputeCommandEncoder>,
    buffer: &MetalBuffer,
    offset: usize,
    index: usize,
) {
    // SAFETY: record validates the external range; internal buffers are allocated above.
    unsafe {
        encoder.setBuffer_offset_atIndex(Some(buffer.handle()), offset, index);
    }
}

fn bind_words(encoder: &ProtocolObject<dyn MTLComputeCommandEncoder>, words: &[u32], index: usize) {
    // SAFETY: Metal copies all parameter bytes before setBytes returns. No padding is serialized.
    unsafe {
        encoder.setBytes_length_atIndex(
            NonNull::new(words.as_ptr() as *mut c_void).unwrap(),
            std::mem::size_of_val(words),
            index,
        );
    }
}

fn dispatch(encoder: &ProtocolObject<dyn MTLComputeCommandEncoder>, count: u32) {
    if count == 0 {
        return;
    }
    encoder.dispatchThreadgroups_threadsPerThreadgroup(
        MTLSize {
            width: count.div_ceil(SCAN_WIDTH) as usize,
            height: 1,
            depth: 1,
        },
        MTLSize {
            width: SCAN_WIDTH as usize,
            height: 1,
            depth: 1,
        },
    );
    encoder.memoryBarrierWithScope(MTLBarrierScope::Buffers);
}

const ASSEMBLY_SOURCE: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct AssemblyParams {
    uint count, index_bytes, base_vertex, restart_enabled;
    uint restart_index, topology, instances, reserved;
};
inline uint2 combine(uint2 a, uint2 b) { return uint2(max(a.x, b.x), a.y + b.y); }

kernel void assembly_initialize(const device uchar* indices [[buffer(0)]],
    device uint* vertex_ids [[buffer(1)]], device uint2* segments [[buffer(2)]],
    constant AssemblyParams& p [[buffer(3)]], uint i [[thread_position_in_grid]]) {
    if (i >= p.count) return;
    uint value = i;
    if (p.index_bytes != 0u) {
        value = 0u;
        for (uint b = 0u; b < p.index_bytes; ++b) value |= uint(indices[ulong(i)*p.index_bytes+b]) << (8u*b);
    }
    bool restart = p.restart_enabled != 0u && value == p.restart_index;
    vertex_ids[i] = value + p.base_vertex;
    segments[i] = uint2(restart ? i+1u : 0u, 0u);
}

kernel void assembly_scan(device uint2* values [[buffer(0)]], device uint2* totals [[buffer(1)]],
    constant uint& count [[buffer(2)]], uint i [[thread_position_in_grid]],
    uint lane [[thread_position_in_threadgroup]], uint group [[threadgroup_position_in_grid]]) {
    threadgroup uint2 scratch[SCAN_WIDTH];
    scratch[lane] = i < count ? values[i] : uint2(0u);
    threadgroup_barrier(mem_flags::mem_threadgroup);
    for (uint step = 1u; step < SCAN_WIDTH; step *= 2u) {
        uint2 previous = lane >= step ? scratch[lane-step] : uint2(0u);
        threadgroup_barrier(mem_flags::mem_threadgroup);
        scratch[lane] = combine(scratch[lane], previous);
        threadgroup_barrier(mem_flags::mem_threadgroup);
    }
    if (i < count) values[i] = scratch[lane];
    if (lane == SCAN_WIDTH-1u) totals[group] = scratch[lane];
}

kernel void assembly_add(device uint2* values [[buffer(0)]], const device uint2* totals [[buffer(1)]],
    constant uint& count [[buffer(2)]], uint i [[thread_position_in_grid]]) {
    if (i < count && i >= SCAN_WIDTH) values[i] = combine(totals[i/SCAN_WIDTH-1u], values[i]);
}

inline uint primitive_ends(uint i, const device uint2* segments, constant AssemblyParams& p) {
    uint start = segments[i].x;
    if (start > i) return 0u;
    uint local = i-start;
    switch (p.topology) {
    case 0u: return 1u;
    case 1u: return uint(local % 2u == 1u);
    case 2u: return local == 0u ? 0u : 1u + uint(i+1u == p.count || segments[i+1u].x > start);
    case 3u: return uint(local >= 1u);
    case 4u: return uint(local % 3u == 2u);
    case 5u: case 6u: return uint(local >= 2u);
    case 10u: return uint(local % 4u == 3u);
    case 11u: return uint(local >= 3u);
    case 12u: return uint(local % 6u == 5u);
    case 13u: return uint(local >= 5u && (local & 1u) == 1u);
    }
    return 0u; // Host rejects unsupported topology before encoding.
}

kernel void assembly_classify(const device uint2* segments [[buffer(0)]], device uint2* counts [[buffer(1)]],
    constant AssemblyParams& p [[buffer(2)]], uint i [[thread_position_in_grid]]) {
    if (i < p.count) counts[i] = uint2(0u, primitive_ends(i, segments, p));
}

kernel void assembly_emit(const device uint2* segments [[buffer(0)]], const device uint2* counts [[buffer(1)]],
    device uint* primitives [[buffer(2)]], device uint* arguments [[buffer(3)]],
    constant AssemblyParams& p [[buffer(4)]], uint i [[thread_position_in_grid]]) {
    if (i == 0u) {
        arguments[0] = p.count == 0u ? 0u : counts[p.count-1u].y;
        arguments[1] = p.instances;
        arguments[2] = 1u;
    }
    if (i >= p.count) return;
    uint emitted = primitive_ends(i, segments, p);
    if (emitted == 0u) return;
    uint start = segments[i].x;
    uint local = i-start;
    uint v[6] = {};
    switch (p.topology) {
    case 0u: v[0] = i; break;
    case 1u: case 2u: case 3u: v[0] = i-1u; v[1] = i; break;
    case 4u: v[0] = i-2u; v[1] = i-1u; v[2] = i; break;
    case 5u: v[0] = i-2u+(local & 1u); v[1] = i-1u-(local & 1u); v[2] = i; break;
    case 6u: v[0] = start; v[1] = i-1u; v[2] = i; break;
    case 10u: case 11u: for (uint j=0u; j<4u; ++j) v[j] = i-3u+j; break;
    case 12u: for (uint j=0u; j<6u; ++j) v[j] = i-5u+j; break;
    case 13u: {
        uint b = i-5u;
        bool odd = ((local-5u)/2u & 1u) != 0u;
        bool last = i+2u >= p.count || segments[i+2u].x != start;
        v[0] = b;
        v[1] = b + (odd ? 3u : (b == start ? 1u : uint(-2)));
        v[2] = b + (odd ? 4u : 2u);
        v[3] = b + (last ? 5u : 6u);
        v[4] = b + (odd ? 2u : 4u);
        v[5] = b + (odd ? uint(-2) : 3u);
        break;
    }
    }
    uint primitive = counts[i].y-emitted;
    for (uint j=0u; j<6u; ++j) primitives[ulong(primitive)*6u+j] = v[j];
    if (emitted == 2u) {
        // Closing edge of a line loop follows its last ordinary edge.
        v[0] = i; v[1] = start;
        for (uint j=0u; j<6u; ++j) primitives[ulong(primitive+1u)*6u+j] = v[j];
    }
}

kernel void assembly_fan_indices(const device uint* vertex_ids [[buffer(0)]],
    const device uint* primitives [[buffer(1)]], const device uint* counts [[buffer(2)]],
    device uint* indices [[buffer(3)]], device uint* arguments [[buffer(4)]],
    constant uint* p [[buffer(5)]], uint i [[thread_position_in_grid]]) {
    if (i == 0u) {
        arguments[0] = counts[0] * 3u;
        arguments[1] = counts[1];
        arguments[2] = 0u; // indexStart
        arguments[3] = p[0]; // signed baseVertex, unchanged bits
        arguments[4] = p[1]; // baseInstance
    }
    if (i >= counts[0]) return;
    // Vulkan fan first/last provoking vertices are the rim's previous/current
    // vertex, not its center. Cyclic rotation preserves front-face winding.
    uint first = p[2] != 0u ? 2u : 1u;
    for (uint j = 0u; j < 3u; ++j) {
        uint ordinal = primitives[ulong(i)*6u + (first+j)%3u];
        // Keep baseVertex in the native draw arguments (and its shader builtin),
        // rather than folding it into the indices a second time.
        indices[ulong(i)*3u+j] = vertex_ids[ordinal] - p[0];
    }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn params(topology: PrimitiveTopology, count: usize) -> MetalPrimitiveAssemblyParams {
        MetalPrimitiveAssemblyParams {
            topology,
            count: count as u32,
            base_vertex: -7,
            instances: 3,
            index_bytes: 4,
            restart_index: Some(255),
        }
    }

    fn readback(
        device: &MetalDevice,
        scheduler: &mut MetalScheduler,
        source: &MetalBuffer,
    ) -> MetalBuffer {
        let destination = MetalBuffer::new(device, source.length()).unwrap();
        source
            .encode_copy(scheduler, &destination, 0, 0, source.length())
            .unwrap();
        destination
    }

    fn words(buffer: &MetalBuffer) -> Vec<u32> {
        let mut bytes = vec![0; buffer.length()];
        buffer.read(0, &mut bytes).unwrap();
        bytes
            .chunks_exact(4)
            .map(|word| u32::from_ne_bytes(word.try_into().unwrap()))
            .collect()
    }

    fn run_case(
        assembler: &MetalPrimitiveAssembler,
        p: MetalPrimitiveAssemblyParams,
        indices: &[u32],
        expected: &[Vec<u32>],
    ) {
        let device = &assembler.device;
        let mut scheduler = MetalScheduler::new(device);
        let mut bytes = vec![0xBA; 12];
        for index in indices {
            bytes.extend_from_slice(&index.to_ne_bytes()[..p.index_bytes as usize]);
        }
        let upload = MetalBuffer::new(device, bytes.len()).unwrap();
        upload.write(0, &bytes).unwrap();
        let source = MetalBuffer::new_private(device, bytes.len()).unwrap();
        upload
            .encode_copy(&mut scheduler, &source, 0, 0, bytes.len())
            .unwrap();
        let output = assembler
            .record(&mut scheduler, p, Some((&source, 12)))
            .unwrap();
        let ids = readback(device, &mut scheduler, &output.vertex_ids);
        let segments = readback(device, &mut scheduler, &output.segments);
        let primitives = readback(device, &mut scheduler, &output.primitives);
        let arguments = readback(device, &mut scheduler, &output.dispatch_arguments);
        // Only the oracle waits. Assembly itself never submits or reads back.
        scheduler.finish_all().unwrap();
        let arguments = words(&arguments);
        assert_eq!(arguments, [expected.len() as u32, p.instances, 1], "{p:?}");
        let records = words(&primitives);
        for (index, expected) in expected.iter().enumerate() {
            let mut padded = [0; PRIMITIVE_RECORD_WORDS];
            padded[..expected.len()].copy_from_slice(expected);
            assert_eq!(
                &records[index * 6..index * 6 + 6],
                &padded,
                "primitive={index} {p:?}"
            );
        }
        let actual_ids = words(&ids);
        let actual_segments = words(&segments);
        let mut segment_start = 0;
        for i in 0..p.count as usize {
            let index = if p.index_bytes == 0 {
                i as u32
            } else {
                indices[i]
            };
            assert_eq!(
                actual_ids[i],
                index.wrapping_add(p.base_vertex as u32),
                "vertex={i} {p:?}"
            );
            if p.index_bytes != 0 && p.restart_index == Some(index) {
                segment_start = i as u32 + 1;
            }
            assert_eq!(actual_segments[i * 2], segment_start, "segment={i} {p:?}");
            assert_eq!(actual_segments[i * 2 + 1], 0);
        }
    }

    #[test]
    fn native_triangle_fan_indices_preserve_restart_draw_arguments_and_provoking_vertex() {
        let device = MetalDevice::new().unwrap();
        let assembler = MetalPrimitiveAssembler::new(&device).unwrap();
        // Includes empty/short segments, leading/consecutive/trailing restarts,
        // repeated (degenerate) vertices, nonzero source offsets and baseVertex.
        for (input, expected_first, expected_last) in [
            (vec![], vec![], vec![]),
            (vec![4, 5], vec![], vec![]),
            (
                vec![4, 5, 6, 7],
                vec![5, 6, 4, 6, 7, 4],
                vec![6, 4, 5, 7, 4, 6],
            ),
            (
                vec![255, 4, 5, 6, 7, 255, 255, 2, 255, 8, 8, 9, 255],
                vec![5, 6, 4, 6, 7, 4, 8, 9, 8],
                vec![6, 4, 5, 7, 4, 6, 9, 8, 8],
            ),
        ] {
            for width in [1, 2, 4] {
                for (last, expected) in [(false, &expected_first), (true, &expected_last)] {
                    let mut scheduler = MetalScheduler::new(&device);
                    let mut bytes = vec![0xBA; 12];
                    for value in &input {
                        bytes.extend_from_slice(&(*value as u32).to_ne_bytes()[..width]);
                    }
                    let upload = MetalBuffer::new(&device, bytes.len()).unwrap();
                    upload.write(0, &bytes).unwrap();
                    let source = MetalBuffer::new_private(&device, bytes.len()).unwrap();
                    upload
                        .encode_copy(&mut scheduler, &source, 0, 0, bytes.len())
                        .unwrap();
                    let mut p = params(PrimitiveTopology::TriangleFan, input.len());
                    p.index_bytes = width as u32;
                    let draw = assembler
                        .record_triangle_fan_draw(&mut scheduler, p, Some((&source, 12)), 19, last)
                        .unwrap();
                    let indices = readback(&device, &mut scheduler, &draw.indices);
                    let arguments = readback(&device, &mut scheduler, &draw.arguments);
                    scheduler.finish_all().unwrap();
                    assert_eq!(
                        &words(&indices)[..expected.len()],
                        expected,
                        "input={input:?} width={width} last={last}"
                    );
                    assert_eq!(
                        words(&arguments),
                        [expected.len() as u32, 3, 0, (-7i32) as u32, 19]
                    );
                }
            }
        }
        let mut scheduler = MetalScheduler::new(&device);
        let mut p = params(PrimitiveTopology::TriangleFan, 5);
        p.index_bytes = 0;
        p.base_vertex = 13;
        let draw = assembler
            .record_triangle_fan_draw(&mut scheduler, p, None, 7, false)
            .unwrap();
        let indices = readback(&device, &mut scheduler, &draw.indices);
        let arguments = readback(&device, &mut scheduler, &draw.arguments);
        scheduler.finish_all().unwrap();
        assert_eq!(&words(&indices)[..9], &[1, 2, 0, 2, 3, 0, 3, 4, 0]);
        assert_eq!(words(&arguments), [9, 3, 0, 13, 7]);
    }

    #[test]
    fn native_triangle_fan_draw_rasterizes_flat_attributes_and_instances() {
        use objc2_metal::{
            MTLBlitCommandEncoder, MTLClearColor, MTLCullMode, MTLIndexType, MTLLoadAction,
            MTLOrigin, MTLPixelFormat, MTLPrimitiveType, MTLRenderCommandEncoder,
            MTLRenderPassDescriptor, MTLRenderPipelineDescriptor, MTLStoreAction,
            MTLTextureDescriptor, MTLTextureUsage, MTLWinding,
        };
        let device = MetalDevice::new().unwrap();
        let assembler = MetalPrimitiveAssembler::new(&device).unwrap();
        let library = device
            .device()
            .newLibraryWithSource_options_error(
                &NSString::from_str(
                    r#"
#include <metal_stdlib>
using namespace metal;
struct Out { float4 position [[position]]; float4 color [[user(locn0), flat]]; };
vertex Out vs(uint id [[vertex_id]], uint instance [[instance_id]], uint base [[base_instance]],
    uint vertex_base [[base_vertex]]) {
    const float2 positions[4] = { float2(-1,-1), float2(0,-1), float2(0,1), float2(-1,1) };
    uint local = id - vertex_base;
    Out out;
    out.position = float4(positions[local] + float2(instance - base, 0), 0, 1);
    out.color = float4(local == 1u, local == 2u, local == 3u, 1);
    return out;
}
fragment float4 fs(Out in [[stage_in]]) { return in.color; }
"#,
                ),
                None,
            )
            .unwrap();
        let descriptor = MTLRenderPipelineDescriptor::new();
        descriptor.setVertexFunction(Some(
            &library
                .newFunctionWithName(&NSString::from_str("vs"))
                .unwrap(),
        ));
        descriptor.setFragmentFunction(Some(
            &library
                .newFunctionWithName(&NSString::from_str("fs"))
                .unwrap(),
        ));
        unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) }
            .setPixelFormat(MTLPixelFormat::RGBA8Unorm);
        let pipeline = device
            .device()
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .unwrap();
        let td = MTLTextureDescriptor::new();
        td.setPixelFormat(MTLPixelFormat::RGBA8Unorm);
        td.setUsage(MTLTextureUsage::RenderTarget);
        unsafe {
            td.setWidth(16);
            td.setHeight(16);
        }
        let texture = device.device().newTextureWithDescriptor(&td).unwrap();
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
        for last in [false, true] {
            let mut scheduler = MetalScheduler::new(&device);
            let p = MetalPrimitiveAssemblyParams {
                topology: PrimitiveTopology::TriangleFan,
                count: 4,
                base_vertex: 13,
                instances: 2,
                index_bytes: 0,
                restart_index: None,
            };
            let draw = assembler
                .record_triangle_fan_draw(&mut scheduler, p, None, 7, last)
                .unwrap();
            scheduler.begin_render_pass(&pass).unwrap();
            scheduler.with_render_encoder(|encoder| unsafe {
                encoder.setRenderPipelineState(&pipeline);
                encoder.setCullMode(MTLCullMode::Back);
                encoder.setFrontFacingWinding(MTLWinding::CounterClockwise);
                encoder.drawIndexedPrimitives_indexType_indexBuffer_indexBufferOffset_indirectBuffer_indirectBufferOffset(
                    MTLPrimitiveType::Triangle, MTLIndexType::UInt32,
                    draw.indices.handle(), 0, draw.arguments.handle(), 0,
                );
            }).unwrap();
            let download = MetalBuffer::new(&device, 4096).unwrap();
            scheduler.with_blit_encoder(|encoder| unsafe {
                encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                    &texture, 0, 0, MTLOrigin { x: 0, y: 0, z: 0 },
                    MTLSize { width: 16, height: 16, depth: 1 }, download.handle(), 0, 256, 4096,
                );
            }).unwrap();
            scheduler.finish_all().unwrap();
            let mut pixels = [0; 4096];
            download.read(0, &mut pixels).unwrap();
            // Test strictly inside each half of both instances, off the diagonal.
            for x_offset in [0, 8] {
                for (x, y, first_color, last_color) in [
                    (6, 12, [255, 0, 0, 255], [0, 255, 0, 255]),
                    (1, 3, [0, 255, 0, 255], [0, 0, 255, 255]),
                ] {
                    let offset = y * 256 + (x + x_offset) * 4;
                    assert_eq!(
                        &pixels[offset..offset + 4],
                        if last { &last_color } else { &first_color },
                        "last={last} instance={} pixel=({x},{y})",
                        x_offset / 8
                    );
                }
            }
        }
    }

    #[test]
    fn native_assembly_preserves_topology_order_and_adjacency_boundaries() {
        let device = MetalDevice::new().unwrap();
        if !device.profile().supports_mesh_shaders() {
            return;
        }
        let assembler = MetalPrimitiveAssembler::new(&device).unwrap();
        use PrimitiveTopology::*;
        let cases: Vec<(PrimitiveTopology, usize, Vec<Vec<u32>>)> = vec![
            (Points, 3, vec![vec![0], vec![1], vec![2]]),
            (Lines, 5, vec![vec![0, 1], vec![2, 3]]),
            (LineStrip, 3, vec![vec![0, 1], vec![1, 2]]),
            (LineLoop, 3, vec![vec![0, 1], vec![1, 2], vec![2, 0]]),
            (LineLoop, 1, vec![]),
            (Triangles, 7, vec![vec![0, 1, 2], vec![3, 4, 5]]),
            (
                TriangleStrip,
                5,
                vec![vec![0, 1, 2], vec![2, 1, 3], vec![2, 3, 4]],
            ),
            (
                TriangleFan,
                5,
                vec![vec![0, 1, 2], vec![0, 2, 3], vec![0, 3, 4]],
            ),
            (LinesAdjacency, 9, vec![vec![0, 1, 2, 3], vec![4, 5, 6, 7]]),
            (
                LineStripAdjacency,
                5,
                vec![vec![0, 1, 2, 3], vec![1, 2, 3, 4]],
            ),
            (
                TrianglesAdjacency,
                13,
                vec![vec![0, 1, 2, 3, 4, 5], vec![6, 7, 8, 9, 10, 11]],
            ),
            // Vulkan drawing.adoc: single, first, odd/even interior and last.
            (TriangleStripAdjacency, 6, vec![vec![0, 1, 2, 5, 4, 3]]),
            (
                TriangleStripAdjacency,
                8,
                vec![vec![0, 1, 2, 6, 4, 3], vec![2, 5, 6, 7, 4, 0]],
            ),
            (
                TriangleStripAdjacency,
                10,
                vec![
                    vec![0, 1, 2, 6, 4, 3],
                    vec![2, 5, 6, 8, 4, 0],
                    vec![4, 2, 6, 9, 8, 7],
                ],
            ),
            (
                TriangleStripAdjacency,
                13,
                vec![
                    vec![0, 1, 2, 6, 4, 3],
                    vec![2, 5, 6, 8, 4, 0],
                    vec![4, 2, 6, 10, 8, 7],
                    vec![6, 9, 10, 11, 8, 4],
                ],
            ),
        ];
        for (topology, count, expected) in cases {
            let mut p = params(topology, count);
            let indices = (0..count).map(|i| (i as u32 * 3) % 7).collect::<Vec<_>>();
            for bytes in [0, 1, 2, 4] {
                p.index_bytes = bytes;
                run_case(&assembler, p, &indices, &expected);
            }
        }
    }

    #[test]
    fn native_assembly_restart_preserves_ids_and_strips_across_scan_levels() {
        let device = MetalDevice::new().unwrap();
        if !device.profile().supports_mesh_shaders() {
            return;
        }
        let assembler = MetalPrimitiveAssembler::new(&device).unwrap();
        let mut indices = (0..66_007).map(|i| i % 19).collect::<Vec<u32>>();
        for i in [
            0, 1, 3, 254, 255, 256, 511, 514, 65_535, 65_536, 65_537, 66_006,
        ] {
            indices[i] = 255;
        }
        let mut expected = Vec::new();
        let mut run = Vec::new();
        for (i, &index) in indices.iter().enumerate() {
            if index == 255 {
                run.clear();
                continue;
            }
            run.push(i as u32);
            if run.len() >= 3 {
                let n = run.len();
                let mut triangle = run[n - 3..].to_vec();
                if n % 2 == 0 {
                    triangle.swap(0, 1);
                }
                expected.push(triangle);
            }
        }
        run_case(
            &assembler,
            params(PrimitiveTopology::TriangleStrip, indices.len()),
            &indices,
            &expected,
        );

        let indices = [255, 255, 4, 8, 9, 255, 1, 255, 6, 7, 255];
        run_case(
            &assembler,
            params(PrimitiveTopology::LineLoop, indices.len()),
            &indices,
            &[vec![2, 3], vec![3, 4], vec![4, 2], vec![8, 9], vec![9, 8]],
        );
        let indices = [0, 1, 2, 3, 4, 5, 255, 6, 7, 8, 9, 10, 11, 255];
        run_case(
            &assembler,
            params(PrimitiveTopology::TriangleStripAdjacency, indices.len()),
            &indices,
            &[vec![0, 1, 2, 5, 4, 3], vec![7, 8, 9, 12, 11, 10]],
        );
        // Compare restart before baseVertex, and never drop degenerates.
        let mut p = params(PrimitiveTopology::Triangles, 6);
        p.restart_index = None;
        run_case(
            &assembler,
            p,
            &[255, 255, 255, 0, 0, 0],
            &[vec![0, 1, 2], vec![3, 4, 5]],
        );
        p.count = 0;
        run_case(&assembler, p, &[], &[]);
    }

    #[test]
    fn assembly_parameters_are_explicit_and_invalid_ranges_fail_before_recording() {
        let p = params(PrimitiveTopology::Triangles, 8);
        assert_eq!(p.words(), [8, 4, 0xfffffff9, 1, 255, 4, 3, 0]);
        assert_eq!(std::mem::size_of_val(&p.words()), 32);
        assert_eq!(
            params(PrimitiveTopology::TrianglesAdjacency, 0)
                .input_topology()
                .unwrap()
                .vertices(),
            6
        );
        assert!(params(PrimitiveTopology::Patches, 0)
            .input_topology()
            .is_err());
        assert!(params(PrimitiveTopology::Quads, 0)
            .input_topology()
            .is_err());
        let device = MetalDevice::new().unwrap();
        if !device.profile().supports_mesh_shaders() {
            return;
        }
        let assembler = MetalPrimitiveAssembler::new(&device).unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        assert!(matches!(
            assembler.record(&mut scheduler, p, None),
            Err(MetalPrimitiveAssemblyError::IndexRange)
        ));
        let buffer = MetalBuffer::new(&device, 32).unwrap();
        assert!(matches!(
            assembler.record(&mut scheduler, p, Some((&buffer, 1))),
            Err(MetalPrimitiveAssemblyError::IndexRange)
        ));
        assert!(matches!(
            assembler.record(
                &mut scheduler,
                MetalPrimitiveAssemblyParams {
                    index_bytes: 3,
                    ..p
                },
                None
            ),
            Err(MetalPrimitiveAssemblyError::IndexWidth(3))
        ));
        assert!(scheduler.flush().unwrap().is_none());
    }
}
