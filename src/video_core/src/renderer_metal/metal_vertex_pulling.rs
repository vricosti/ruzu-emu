// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Explicit vertex fetch for the object/mesh path, which has no vertex descriptor.
//! Eden delegates this conversion to Vulkan vertex fetch; this native Metal
//! adaptation consumes the same formats as metal_pipeline_cache's vertex layout.

use super::metal_pipeline_cache::MetalVertexInputState;
use objc2_metal::{MTLVertexFormat, MTLVertexStepFunction};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VertexPullingLayoutError {
    #[error(transparent)]
    Format(#[from] UnsupportedVertexPullingFormat),
    #[error(transparent)]
    Step(#[from] UnsupportedVertexPullingStep),
    #[error("vertex attribute refers to missing Metal buffer layout {0}")]
    MissingBuffer(u8),
}

#[derive(Debug)]
pub struct VertexPullingBuffer {
    pub index: u8,
}

impl VertexPullingBuffer {
    pub fn name(&self) -> String {
        format!("vertex_buffer{}", self.index)
    }

    pub fn parameter(&self) -> String {
        format!("const device uchar* {}", self.name())
    }
}

/// Uses the compact native buffer indices already selected by the pipeline
/// cache. The MslVertexIn declaration is owned by the IR's MSL emitter.
#[derive(Debug)]
pub struct VertexPullingLayout {
    pub buffers: Vec<VertexPullingBuffer>,
    pub source: String,
}

impl VertexPullingLayout {
    pub fn new(state: &MetalVertexInputState) -> Result<Option<Self>, VertexPullingLayoutError> {
        if state
            .attributes
            .iter()
            .all(|attribute| attribute.format == MTLVertexFormat::Invalid)
        {
            return Ok(None);
        }
        let mut buffers = Vec::new();
        let mut source = VERTEX_PULLING_HELPERS.to_owned();
        let mut assignments = String::new();
        for (index, attribute) in state.attributes.iter().enumerate() {
            if attribute.format == MTLVertexFormat::Invalid {
                continue;
            }
            let layout = state
                .layouts
                .iter()
                .find(|layout| layout.enabled && layout.buffer_index == attribute.buffer_index)
                .ok_or(VertexPullingLayoutError::MissingBuffer(
                    attribute.buffer_index,
                ))?;
            if !buffers
                .iter()
                .any(|buffer: &VertexPullingBuffer| buffer.index == attribute.buffer_index)
            {
                buffers.push(VertexPullingBuffer {
                    index: attribute.buffer_index,
                });
                source.push_str(&vertex_index_source(
                    layout.step_function,
                    layout.step_rate,
                    &format!("vertex_index{}", attribute.buffer_index),
                )?);
            }
            let format = VertexPullingFormat::new(attribute.format)?;
            source.push_str(&format.function_source(&format!("vertex_attribute{index}")));
            assignments.push_str(&format!(
                "    ulong offset{index} = ulong(vertex_index{buffer}(vertex_id, instance_id, base_instance)) * {stride}ul + {offset}ul;\n    if (offset{index} <= size{buffer} && {size}ul <= size{buffer} - offset{index}) {{\n        input.in_attr{index} = vertex_attribute{index}(vertex_buffer{buffer} + offset{index});\n    }}\n",
                buffer = attribute.buffer_index, stride = layout.stride, offset = attribute.offset,
                size = format.byte_size()));
        }
        let parameters = buffers
            .iter()
            .map(|buffer| format!("{}, ulong size{}", buffer.parameter(), buffer.index))
            .collect::<Vec<_>>()
            .join(", ");
        source.push_str(&format!(
            "inline MslVertexIn ruzu_pull_vertex(uint vertex_id, uint instance_id, uint base_instance, {parameters}) {{\n    MslVertexIn input = {{}};\n{assignments}    return input;\n}}\n"));
        Ok(Some(Self { buffers, source }))
    }
}

#[derive(Debug, Error)]
#[error("explicit Metal vertex fetch does not support format {0:?}")]
pub struct UnsupportedVertexPullingFormat(pub MTLVertexFormat);

#[derive(Debug, Error)]
#[error("explicit Metal vertex fetch does not support step {step:?} at rate {rate}")]
pub struct UnsupportedVertexPullingStep {
    step: MTLVertexStepFunction,
    rate: u32,
}

/// The IDs include the draw's base values, like Metal's vertex/instance builtins.
/// Instance divisors apply before adding baseInstance, not to the absolute ID.
pub fn vertex_index_source(
    step: MTLVertexStepFunction,
    rate: u32,
    name: &str,
) -> Result<String, UnsupportedVertexPullingStep> {
    let expression = match step {
        MTLVertexStepFunction::Constant => "0u".to_owned(),
        MTLVertexStepFunction::PerVertex if rate != 0 => format!("vertex_id / {rate}u"),
        MTLVertexStepFunction::PerInstance if rate != 0 => {
            format!("base_instance + (instance_id - base_instance) / {rate}u")
        }
        _ => return Err(UnsupportedVertexPullingStep { step, rate }),
    };
    Ok(format!("inline uint {name}(uint vertex_id, uint instance_id, uint base_instance) {{ return {expression}; }}\n"))
}

#[derive(Debug, Clone, Copy)]
enum Scalar {
    Unsigned,
    Signed,
    UNorm,
    SNorm,
    Float,
}

/// One native format's byte representation and shader input type. Integer
/// inputs remain integers; scaled-to-float conversion belongs to the shader IR.
#[derive(Debug, Clone, Copy)]
pub struct VertexPullingFormat {
    scalar: Scalar,
    bits: u32,
    components: u32,
    packed: bool,
}

const FORMATS: &[([MTLVertexFormat; 4], Scalar, u32)] = &[
    (
        [
            MTLVertexFormat::UChar,
            MTLVertexFormat::UChar2,
            MTLVertexFormat::UChar3,
            MTLVertexFormat::UChar4,
        ],
        Scalar::Unsigned,
        8,
    ),
    (
        [
            MTLVertexFormat::Char,
            MTLVertexFormat::Char2,
            MTLVertexFormat::Char3,
            MTLVertexFormat::Char4,
        ],
        Scalar::Signed,
        8,
    ),
    (
        [
            MTLVertexFormat::UCharNormalized,
            MTLVertexFormat::UChar2Normalized,
            MTLVertexFormat::UChar3Normalized,
            MTLVertexFormat::UChar4Normalized,
        ],
        Scalar::UNorm,
        8,
    ),
    (
        [
            MTLVertexFormat::CharNormalized,
            MTLVertexFormat::Char2Normalized,
            MTLVertexFormat::Char3Normalized,
            MTLVertexFormat::Char4Normalized,
        ],
        Scalar::SNorm,
        8,
    ),
    (
        [
            MTLVertexFormat::UShort,
            MTLVertexFormat::UShort2,
            MTLVertexFormat::UShort3,
            MTLVertexFormat::UShort4,
        ],
        Scalar::Unsigned,
        16,
    ),
    (
        [
            MTLVertexFormat::Short,
            MTLVertexFormat::Short2,
            MTLVertexFormat::Short3,
            MTLVertexFormat::Short4,
        ],
        Scalar::Signed,
        16,
    ),
    (
        [
            MTLVertexFormat::UShortNormalized,
            MTLVertexFormat::UShort2Normalized,
            MTLVertexFormat::UShort3Normalized,
            MTLVertexFormat::UShort4Normalized,
        ],
        Scalar::UNorm,
        16,
    ),
    (
        [
            MTLVertexFormat::ShortNormalized,
            MTLVertexFormat::Short2Normalized,
            MTLVertexFormat::Short3Normalized,
            MTLVertexFormat::Short4Normalized,
        ],
        Scalar::SNorm,
        16,
    ),
    (
        [
            MTLVertexFormat::UInt,
            MTLVertexFormat::UInt2,
            MTLVertexFormat::UInt3,
            MTLVertexFormat::UInt4,
        ],
        Scalar::Unsigned,
        32,
    ),
    (
        [
            MTLVertexFormat::Int,
            MTLVertexFormat::Int2,
            MTLVertexFormat::Int3,
            MTLVertexFormat::Int4,
        ],
        Scalar::Signed,
        32,
    ),
    (
        [
            MTLVertexFormat::Half,
            MTLVertexFormat::Half2,
            MTLVertexFormat::Half3,
            MTLVertexFormat::Half4,
        ],
        Scalar::Float,
        16,
    ),
    (
        [
            MTLVertexFormat::Float,
            MTLVertexFormat::Float2,
            MTLVertexFormat::Float3,
            MTLVertexFormat::Float4,
        ],
        Scalar::Float,
        32,
    ),
];

/// Emit once per shader library. Byte loads avoid alignment/alias assumptions
/// when guest attribute offsets do not satisfy host scalar-pointer alignment.
pub const VERTEX_PULLING_HELPERS: &str = r#"
inline uint ruzu_vertex_read(const device uchar* p, uint offset, uint bytes) {
    uint value = 0u;
    for (uint i = 0u; i < bytes; ++i) value |= uint(p[offset + i]) << (8u * i);
    return value;
}
inline float ruzu_vertex_ufloat(uint value, uint mantissa_bits) {
    uint exponent = value >> mantissa_bits;
    uint mantissa = value & ((1u << mantissa_bits) - 1u);
    if (exponent == 0u) return ldexp(float(mantissa), -14 - int(mantissa_bits));
    uint float_exponent = exponent == 31u ? 255u : exponent + 112u;
    return as_type<float>((float_exponent << 23u) | (mantissa << (23u - mantissa_bits)));
}
"#;

impl VertexPullingFormat {
    pub fn new(format: MTLVertexFormat) -> Result<Self, UnsupportedVertexPullingFormat> {
        for &(formats, scalar, bits) in FORMATS {
            if let Some(index) = formats.iter().position(|&value| value == format) {
                return Ok(Self {
                    scalar,
                    bits,
                    components: index as u32 + 1,
                    packed: false,
                });
            }
        }
        let (scalar, components) = match format {
            MTLVertexFormat::UInt1010102Normalized => (Scalar::UNorm, 4),
            MTLVertexFormat::Int1010102Normalized => (Scalar::SNorm, 4),
            MTLVertexFormat::FloatRG11B10 => (Scalar::Float, 3),
            _ => return Err(UnsupportedVertexPullingFormat(format)),
        };
        Ok(Self {
            scalar,
            bits: 32,
            components,
            packed: true,
        })
    }

    pub fn input_type(self) -> &'static str {
        match self.scalar {
            Scalar::Unsigned => "uint4",
            Scalar::Signed => "int4",
            _ => "float4",
        }
    }

    pub fn byte_size(self) -> u32 {
        if self.packed {
            4
        } else {
            self.bits / 8 * self.components
        }
    }

    pub fn function_source(self, name: &str) -> String {
        let values: Vec<_> = (0..4)
            .map(|component| {
                if component >= self.components {
                    return if component == 3 { "1" } else { "0" }.to_owned();
                }
                let (raw, bits) = if self.packed {
                    let (offset, bits) = if matches!(self.scalar, Scalar::Float) {
                        (component * 11, if component == 2 { 10 } else { 11 })
                    } else {
                        (component * 10, if component == 3 { 2 } else { 10 })
                    };
                    (
                        format!(
                            "((ruzu_vertex_read(p, 0u, 4u) >> {offset}u) & {}u)",
                            (1u32 << bits) - 1
                        ),
                        bits,
                    )
                } else {
                    (
                        format!(
                            "ruzu_vertex_read(p, {}u, {}u)",
                            component * self.bits / 8,
                            self.bits / 8
                        ),
                        self.bits,
                    )
                };
                let signed = format!(
                    "(as_type<int>(({raw}) << {}u) >> {}u)",
                    32 - bits,
                    32 - bits
                );
                match self.scalar {
                    Scalar::Unsigned => raw,
                    Scalar::Signed => signed,
                    Scalar::UNorm => format!("(float({raw}) / {}.0f)", (1u32 << bits) - 1),
                    Scalar::SNorm => format!(
                        "max(-1.0f, float({signed}) / {}.0f)",
                        (1u32 << (bits - 1)) - 1
                    ),
                    Scalar::Float if self.packed => {
                        format!("ruzu_vertex_ufloat({raw}, {}u)", bits - 5)
                    }
                    Scalar::Float if bits == 16 => format!("float(as_type<half>(ushort({raw})))"),
                    Scalar::Float => format!("as_type<float>({raw})"),
                }
            })
            .collect();
        format!(
            "inline {} {name}(const device uchar* p) {{ return {}({}); }}\n",
            self.input_type(),
            self.input_type(),
            values.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer_metal::metal_pipeline_cache::{
        MetalVertexAttributeState, MetalVertexBufferLayoutState,
    };
    use crate::renderer_metal::{metal_buffer::MetalBuffer, metal_device::MetalDevice};
    use objc2_foundation::NSString;
    use objc2_metal::{
        MTLCommandBuffer as _, MTLCommandBufferStatus, MTLCommandEncoder as _,
        MTLCommandQueue as _, MTLCompileOptions, MTLComputeCommandEncoder as _, MTLDevice as _,
        MTLLanguageVersion, MTLLibrary as _, MTLMathMode, MTLPrimitiveType,
        MTLRenderCommandEncoder as _, MTLRenderPassDescriptor, MTLRenderPipelineDescriptor,
        MTLSize, MTLVertexDescriptor,
    };

    #[test]
    fn packed_formats_and_default_components() {
        let packed = VertexPullingFormat::new(MTLVertexFormat::UInt1010102Normalized).unwrap();
        assert_eq!(packed.byte_size(), 4);
        let source = packed.function_source("fetch");
        assert!(source.contains(">> 30u) & 3u"));
        assert!(source.contains("/ 3.0f"));
        let signed = VertexPullingFormat::new(MTLVertexFormat::Char).unwrap();
        assert_eq!(signed.byte_size(), 1);
        assert_eq!(signed.input_type(), "int4");
        assert!(signed.function_source("fetch").contains(", 0, 0, 1)"));
        assert!(VertexPullingFormat::new(MTLVertexFormat::Invalid).is_err());
    }

    #[test]
    fn pulling_layout_preserves_compacted_bindings_and_attribute_offsets() {
        let mut state = MetalVertexInputState::default();
        assert!(VertexPullingLayout::new(&state).unwrap().is_none());
        state.attributes[7] = MetalVertexAttributeState {
            format: MTLVertexFormat::Float2,
            buffer_index: 5,
            offset: 4,
        };
        assert!(matches!(
            VertexPullingLayout::new(&state),
            Err(VertexPullingLayoutError::MissingBuffer(5))
        ));
        state.attributes[11] = MetalVertexAttributeState {
            format: MTLVertexFormat::UShort2,
            buffer_index: 5,
            offset: 12,
        };
        state.layouts[19] = MetalVertexBufferLayoutState {
            stride: 24,
            step_function: MTLVertexStepFunction::PerInstance,
            step_rate: 3,
            buffer_index: 5,
            enabled: true,
        };
        let layout = VertexPullingLayout::new(&state).unwrap().unwrap();
        assert_eq!(layout.buffers.len(), 1);
        assert_eq!(layout.buffers[0].index, 5);
        assert_eq!(
            layout.buffers[0].parameter(),
            "const device uchar* vertex_buffer5"
        );
        assert_eq!(
            layout.source.matches("inline uint vertex_index5(").count(),
            1
        );
        assert!(layout.source.contains("ulong offset7 = ulong(vertex_index5(vertex_id, instance_id, base_instance)) * 24ul + 4ul"));
        assert!(layout.source.contains("ulong offset11 = ulong(vertex_index5(vertex_id, instance_id, base_instance)) * 24ul + 12ul"));
        assert!(layout
            .source
            .contains("offset7 <= size5 && 8ul <= size5 - offset7"));
        assert!(layout
            .source
            .contains("base_instance + (instance_id - base_instance) / 3u"));
        state.layouts[19].step_rate = 0;
        assert!(matches!(
            VertexPullingLayout::new(&state),
            Err(VertexPullingLayoutError::Step(_))
        ));
    }

    #[test]
    fn explicit_fetch_respects_bound_range_before_loading() {
        let device = MetalDevice::new().unwrap();
        let mut state = MetalVertexInputState::default();
        state.attributes[0] = MetalVertexAttributeState {
            format: MTLVertexFormat::Float4,
            offset: 0,
            buffer_index: 0,
        };
        state.layouts[0] = MetalVertexBufferLayoutState {
            stride: 16,
            step_function: MTLVertexStepFunction::PerVertex,
            step_rate: 1,
            buffer_index: 0,
            enabled: true,
        };
        let pulling = VertexPullingLayout::new(&state).unwrap().unwrap();
        let source = format!(
            r#"
#include <metal_stdlib>
using namespace metal;
struct MslVertexIn {{ float4 in_attr0; }};
{}
kernel void bounded_fetch(const device uchar* data [[buffer(0)]], device float4* result [[buffer(1)]], uint i [[thread_position_in_grid]]) {{
    uint input_id = i%3u == 0u ? 0u : (i%3u == 1u ? 1u : 0xffffffffu);
    ulong size = i/3u == 0u ? 16ul : (i/3u == 1u ? 15ul : 0ul);
    result[i] = ruzu_pull_vertex(input_id, 0u, 0u, data, size).in_attr0;
}}
"#,
            pulling.source
        );
        let library = super::super::metal_shader::compile_msl_library(
            device.device(),
            &source,
            shader_recompiler::backend::msl::MslVersion::V3_0,
        )
        .unwrap();
        let function = library
            .newFunctionWithName(&NSString::from_str("bounded_fetch"))
            .unwrap();
        let pipeline = device
            .device()
            .newComputePipelineStateWithFunction_error(&function)
            .unwrap();
        let input = MetalBuffer::new(&device, 16).unwrap();
        input
            .write(0, bytemuck::cast_slice(&[1f32, 2., 3., 4.]))
            .unwrap();
        let output = MetalBuffer::new(&device, 9 * 16).unwrap();
        output.write(0, &[0xcd; 9 * 16]).unwrap();
        let command = device.command_queue().commandBuffer().unwrap();
        let encoder = command.computeCommandEncoder().unwrap();
        encoder.setComputePipelineState(&pipeline);
        // SAFETY: one output float4 per invocation; the shader checks input ranges.
        unsafe {
            encoder.setBuffer_offset_atIndex(Some(input.handle()), 0, 0);
            encoder.setBuffer_offset_atIndex(Some(output.handle()), 0, 1);
            encoder.dispatchThreads_threadsPerThreadgroup(
                MTLSize {
                    width: 9,
                    height: 1,
                    depth: 1,
                },
                MTLSize {
                    width: 9,
                    height: 1,
                    depth: 1,
                },
            );
        }
        encoder.endEncoding();
        command.commit();
        command.waitUntilCompleted();
        assert_eq!(command.status(), MTLCommandBufferStatus::Completed);
        let mut bytes = [0u8; 9 * 16];
        output.read(0, &mut bytes).unwrap();
        assert_eq!(&bytes[..16], bytemuck::cast_slice::<f32, u8>(&[1f32, 2., 3., 4.]));
        assert!(bytes[16..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn explicit_fetch_matches_native_vertex_descriptor_on_gpu() {
        let device = MetalDevice::new().expect("Metal device");
        // Include zeros, negative integer extrema, half subnormals and infinities,
        // packed fields and arbitrary bit patterns. Both paths read identical bytes.
        let words = [
            0,
            0,
            0,
            0,
            u32::MAX,
            u32::MAX,
            u32::MAX,
            u32::MAX,
            0x80808080,
            0x7f7f7f7f,
            0x80008000,
            0x7fff7fff,
            0x3f800000,
            0xbf000000,
            0x477fe000,
            0x358637bd,
            0x00013c00,
            0x80017bff,
            0xfc007c00,
            0x3555bc00,
            0x7f800000,
            0xff800000,
            0x7fc01234,
            0x80000000,
            0xa53c9e17,
            0x11223344,
            0x99887766,
            0xffbbccdd,
            0x7fefffff,
            0x3c17c800,
            0x000007ff,
            0xaaaaaaaa,
        ];
        let bytes: Vec<_> = words
            .iter()
            .flat_map(|word: &u32| word.to_le_bytes())
            .collect();
        let input = MetalBuffer::new(&device, bytes.len()).unwrap();
        input.write(0, &bytes).unwrap();
        let native_result = MetalBuffer::new(&device, bytes.len()).unwrap();
        let pulled_result = MetalBuffer::new(&device, bytes.len()).unwrap();
        let formats = FORMATS.iter().flat_map(|&(formats, _, _)| formats).chain([
            MTLVertexFormat::UInt1010102Normalized,
            MTLVertexFormat::Int1010102Normalized,
            MTLVertexFormat::FloatRG11B10,
        ]);
        let cases = formats
            .map(|format| (format, MTLVertexStepFunction::PerVertex, 1, 0, 8, 1, 0))
            .chain([
                (
                    MTLVertexFormat::Float4,
                    MTLVertexStepFunction::PerVertex,
                    1,
                    2,
                    4,
                    2,
                    3,
                ),
                (
                    MTLVertexFormat::UInt4,
                    MTLVertexStepFunction::PerInstance,
                    3,
                    2,
                    2,
                    4,
                    1,
                ),
                (
                    MTLVertexFormat::Short4Normalized,
                    MTLVertexStepFunction::Constant,
                    0,
                    1,
                    2,
                    4,
                    3,
                ),
            ]);
        for (format, step, rate, first_vertex, vertex_count, instance_count, base_instance) in cases
        {
            let pulling = VertexPullingFormat::new(format).unwrap();
            let mut state = MetalVertexInputState::default();
            state.attributes[0] = MetalVertexAttributeState {
                format,
                offset: 0,
                buffer_index: 0,
            };
            state.layouts[0] = MetalVertexBufferLayoutState {
                stride: 16,
                step_function: step,
                step_rate: rate,
                buffer_index: 0,
                enabled: true,
            };
            let pulling_layout = VertexPullingLayout::new(&state).unwrap().unwrap();
            let input_type = pulling.input_type();
            let source = format!(
                r#"
#include <metal_stdlib>
using namespace metal;
struct MslVertexIn {{ {input_type} in_attr0 [[attribute(0)]]; }};
{pulling_source}
vertex void native_fetch(MslVertexIn input [[stage_in]], device uint4* result [[buffer(1)]], uint vertex_id [[vertex_id]], uint instance_id [[instance_id]], uint base_instance [[base_instance]]) {{
    uint id = (instance_id - base_instance) * {vertex_count}u + vertex_id - {first_vertex}u;
    result[id] = as_type<uint4>(input.in_attr0);
}}
kernel void explicit_fetch(const device uchar* data [[buffer(0)]], device uint4* result [[buffer(1)]], uint id [[thread_position_in_grid]]) {{
    uint vertex_id = id % {vertex_count}u + {first_vertex}u;
    uint instance_id = id / {vertex_count}u + {base_instance}u;
    MslVertexIn input = ruzu_pull_vertex(vertex_id, instance_id, {base_instance}u, data, 128ul);
    result[id] = as_type<uint4>(input.in_attr0);
}}
"#,
                pulling_source = pulling_layout.source
            );
            let options = MTLCompileOptions::new();
            options.setLanguageVersion(MTLLanguageVersion::Version3_0);
            if objc2::available!(macos = 15.0, ..) {
                options.setMathMode(MTLMathMode::Safe);
            } else {
                #[allow(deprecated)]
                options.setFastMathEnabled(false);
            }
            let library = device
                .device()
                .newLibraryWithSource_options_error(&NSString::from_str(&source), Some(&options))
                .unwrap_or_else(|error| panic!("{format:?}: {error}\n{source}"));
            let vertex = library
                .newFunctionWithName(&NSString::from_str("native_fetch"))
                .unwrap();
            let kernel = library
                .newFunctionWithName(&NSString::from_str("explicit_fetch"))
                .unwrap();
            let vertex_descriptor = MTLVertexDescriptor::vertexDescriptor();
            // SAFETY: local descriptors, valid attribute and buffer slots.
            unsafe {
                let attribute = vertex_descriptor.attributes().objectAtIndexedSubscript(0);
                attribute.setFormat(format);
                attribute.setBufferIndex(0);
                let layout = vertex_descriptor.layouts().objectAtIndexedSubscript(0);
                layout.setStride(16);
                layout.setStepFunction(step);
                layout.setStepRate(rate as usize);
            }
            let pipeline_descriptor = MTLRenderPipelineDescriptor::new();
            pipeline_descriptor.setVertexFunction(Some(&vertex));
            pipeline_descriptor.setVertexDescriptor(Some(&vertex_descriptor));
            pipeline_descriptor.setRasterizationEnabled(false);
            let native_pipeline = device
                .device()
                .newRenderPipelineStateWithDescriptor_error(&pipeline_descriptor)
                .unwrap_or_else(|error| panic!("native {format:?}: {error}"));
            let compute_pipeline = device
                .device()
                .newComputePipelineStateWithFunction_error(&kernel)
                .unwrap();
            native_result.write(0, &[0xcd; 128]).unwrap();
            pulled_result.write(0, &[0xab; 128]).unwrap();
            let command = device.command_queue().commandBuffer().unwrap();
            let pass = MTLRenderPassDescriptor::renderPassDescriptor();
            pass.setRenderTargetWidth(1);
            pass.setRenderTargetHeight(1);
            pass.setDefaultRasterSampleCount(1);
            let render = command.renderCommandEncoderWithDescriptor(&pass).unwrap();
            render.setRenderPipelineState(&native_pipeline);
            // SAFETY: retained shared buffers cover all eight input/output records.
            unsafe {
                render.setVertexBuffer_offset_atIndex(Some(input.handle()), 0, 0);
                render.setVertexBuffer_offset_atIndex(Some(native_result.handle()), 0, 1);
                render.drawPrimitives_vertexStart_vertexCount_instanceCount_baseInstance(
                    MTLPrimitiveType::Point,
                    first_vertex,
                    vertex_count,
                    instance_count,
                    base_instance,
                );
            }
            render.endEncoding();
            let compute = command.computeCommandEncoder().unwrap();
            compute.setComputePipelineState(&compute_pipeline);
            // SAFETY: buffers and thread dimensions cover exactly eight records.
            unsafe {
                compute.setBuffer_offset_atIndex(Some(input.handle()), 0, 0);
                compute.setBuffer_offset_atIndex(Some(pulled_result.handle()), 0, 1);
                compute.dispatchThreads_threadsPerThreadgroup(
                    MTLSize {
                        width: 8,
                        height: 1,
                        depth: 1,
                    },
                    MTLSize {
                        width: 8,
                        height: 1,
                        depth: 1,
                    },
                );
            }
            compute.endEncoding();
            command.commit();
            // Test-only CPU oracle readback; not a production synchronization path.
            command.waitUntilCompleted();
            assert_eq!(
                command.status(),
                MTLCommandBufferStatus::Completed,
                "{:?}",
                command.error()
            );
            let mut expected = [0; 128];
            let mut actual = [0; 128];
            native_result.read(0, &mut expected).unwrap();
            pulled_result.read(0, &mut actual).unwrap();
            for (index, (expected, actual)) in expected
                .chunks_exact(4)
                .zip(actual.chunks_exact(4))
                .enumerate()
            {
                let expected = u32::from_le_bytes(expected.try_into().unwrap());
                let actual = u32::from_le_bytes(actual.try_into().unwrap());
                if matches!(pulling.scalar, Scalar::Unsigned | Scalar::Signed) {
                    assert_eq!(actual, expected, "{format:?} component {index}");
                } else {
                    let left = f32::from_bits(actual);
                    let right = f32::from_bits(expected);
                    // NaN payloads need not survive format conversion. Normalized
                    // hardware conversion may differ by one final rounding bit.
                    let equal = actual == expected
                        || (left.is_nan() && right.is_nan())
                        || (matches!(pulling.scalar, Scalar::UNorm | Scalar::SNorm)
                            && left.is_finite()
                            && right.is_finite()
                            && actual.abs_diff(expected) <= 1);
                    assert!(equal, "{format:?} component {index}: explicit={left} ({actual:08x}), native={right} ({expected:08x})");
                }
            }
        }
    }
}
