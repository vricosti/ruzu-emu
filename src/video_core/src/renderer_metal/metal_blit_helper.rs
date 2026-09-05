// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal fixed-function image passes.
//!
//! This owns the Metal counterpart of Eden's `renderer_vulkan/blit_image`.
//! Maxwell DrawTexture and final presentation use the same pixel-space quad
//! convention rather than open-coding different coordinate transforms.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLBlitOption, MTLColorWriteMask, MTLCompareFunction, MTLCompileOptions,
    MTLComputeCommandEncoder, MTLComputePipelineState, MTLCullMode, MTLDepthStencilDescriptor,
    MTLDepthStencilState, MTLDevice as _, MTLFunction, MTLLanguageVersion, MTLLibrary, MTLOrigin,
    MTLPixelFormat, MTLPrimitiveType, MTLRenderCommandEncoder, MTLRenderPassDescriptor,
    MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLSamplerAddressMode,
    MTLSamplerBorderColor, MTLSamplerDescriptor, MTLSamplerMinMagFilter, MTLSamplerMipFilter,
    MTLSamplerState, MTLScissorRect, MTLSize, MTLStencilDescriptor, MTLStencilOperation,
    MTLTexture, MTLViewport,
};
use thiserror::Error;

use super::metal_buffer::{MetalBuffer, MetalBufferError};
use super::metal_device::MetalDevice;
use super::metal_framebuffer::{MetalFramebuffer, MetalFramebufferSignature};
use super::metal_image_view::MetalImageView;
use super::metal_query_cache::{MetalQueryCache, MetalVisibilityQuery};
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};
use crate::engines::fermi_2d::{Filter, Operation};
use crate::surface::{get_format_type, SurfaceType};
use shader_recompiler::shader_info::TextureType;

#[derive(Clone, Copy, Debug)]
pub struct MetalBlitRegion {
    pub start: (i32, i32),
    pub end: (i32, i32),
}

#[repr(C)]
#[derive(Clone, Copy)]
struct BlitParameters {
    dst: [f32; 4],
    src: [f32; 4],
    target_size: [f32; 2],
    _padding: [f32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MetalClearColorType {
    Float,
    Sint,
    Uint,
}

#[derive(Clone, Copy, Debug)]
pub struct MetalClearParameters {
    pub region: MetalBlitRegion,
    pub render_area: (u32, u32),
    pub color: [f32; 4],
    pub signed_color: [i32; 4],
    pub unsigned_color: [u32; 4],
    pub depth: f32,
    pub stencil: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ClearShaderParameters {
    dst: [f32; 4],
    target_size: [f32; 2],
    depth: f32,
    _padding: f32,
    color: [f32; 4],
    signed_color: [i32; 4],
    unsigned_color: [u32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ClearPipelineKey {
    signature: MetalFramebufferSignature,
    color_attachment: Option<u8>,
    color_type: MetalClearColorType,
    color_mask: u8,
    depth: bool,
    stencil: bool,
    stencil_write_mask: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum BlitAspect {
    Color,
    Depth,
    Stencil,
    DepthStencil,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct BlitPipelineKey {
    signature: MetalFramebufferSignature,
    aspect: BlitAspect,
    color_type: MetalClearColorType,
    source_msaa: bool,
    operation: Operation,
}

#[derive(Debug, Error)]
pub enum MetalBlitError {
    #[error("invalid Metal image blit: {0}")]
    InvalidBlit(&'static str),
    #[error("Metal failed to create a fixed blit sampler")]
    NoSampler,
    #[error(transparent)]
    Buffer(#[from] MetalBufferError),
    #[error("invalid depth/stencil buffer copy: {0}")]
    InvalidDepthStencilCopy(&'static str),
    #[error("Metal blit shader compilation failed: {0}")]
    ShaderCompile(String),
    #[error("Metal blit shader entry point {0} is missing")]
    MissingEntryPoint(String),
    #[error("Metal blit pipeline creation failed: {0}")]
    Pipeline(String),
    #[error("Metal failed to create a depth/stencil state for a fixed image pass")]
    NoDepthStencilState,
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
}

/// Byte layout and subresource for a D32S8 buffer transfer. Unlike Metal's
/// individual planes, the guest stores depth bits followed by a stencil word.
#[derive(Clone, Copy)]
pub struct MetalDepthStencilBufferCopy {
    pub buffer_offset: usize,
    pub bytes_per_row: usize,
    pub bytes_per_image: usize,
    pub slice: usize,
    pub level: usize,
    pub origin: MTLOrigin,
    pub size: MTLSize,
}

/// GPU-only equivalent of the D32S8/RG32 reinterpretation passes. Copying a
/// combined Metal depth/stencil texture requires separate aspect blits; integer
/// kernels preserve the depth bit pattern and pack/unpack the stencil byte.
pub struct MetalDepthStencilCopy {
    device: MetalDevice,
    pack: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    unpack: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
}

impl MetalDepthStencilCopy {
    pub fn new(device: &MetalDevice) -> Result<Self, MetalBlitError> {
        let source = NSString::from_str(
            r#"
#include <metal_stdlib>
using namespace metal;
struct Layout {
    uint width, height, depth;
    uint depth_row, depth_image, stencil_row, stencil_image, packed_row, packed_image;
};
kernel void pack_d32s8(device const uint* d [[buffer(0)]],
                       device const uchar* s [[buffer(1)]],
                       device uint* p [[buffer(2)]],
                       constant Layout& l [[buffer(3)]], uint3 t [[thread_position_in_grid]]) {
    if (t.x >= l.width || t.y >= l.height || t.z >= l.depth) return;
    uint i = t.z*l.packed_image + t.y*l.packed_row + 2*t.x;
    p[i] = d[t.z*l.depth_image + t.y*l.depth_row + t.x];
    p[i+1] = uint(s[t.z*l.stencil_image + t.y*l.stencil_row + t.x]);
}
kernel void unpack_d32s8(device uint* d [[buffer(0)]],
                         device uchar* s [[buffer(1)]],
                         device const uint* p [[buffer(2)]],
                         constant Layout& l [[buffer(3)]], uint3 t [[thread_position_in_grid]]) {
    if (t.x >= l.width || t.y >= l.height || t.z >= l.depth) return;
    uint i = t.z*l.packed_image + t.y*l.packed_row + 2*t.x;
    d[t.z*l.depth_image + t.y*l.depth_row + t.x] = p[i];
    s[t.z*l.stencil_image + t.y*l.stencil_row + t.x] = uchar(p[i+1] & 0xffu);
}
"#,
        );
        let options = MTLCompileOptions::new();
        options.setLanguageVersion(MTLLanguageVersion::Version2_3);
        let library = device
            .device()
            .newLibraryWithSource_options_error(&source, Some(&options))
            .map_err(|error| {
                MetalBlitError::ShaderCompile(error.localizedDescription().to_string())
            })?;
        let pipeline = |name: &str| {
            let function = library
                .newFunctionWithName(&NSString::from_str(name))
                .ok_or_else(|| MetalBlitError::MissingEntryPoint(name.into()))?;
            device
                .device()
                .newComputePipelineStateWithFunction_error(&function)
                .map_err(|error| MetalBlitError::Pipeline(error.localizedDescription().to_string()))
        };
        Ok(Self {
            device: device.clone(),
            pack: pipeline("pack_d32s8")?,
            unpack: pipeline("unpack_d32s8")?,
        })
    }

    pub fn copy(
        &self,
        scheduler: &mut MetalScheduler,
        texture: &ProtocolObject<dyn MTLTexture>,
        packed: &MetalBuffer,
        copy: MetalDepthStencilBufferCopy,
        upload: bool,
    ) -> Result<(), MetalBlitError> {
        let invalid = MetalBlitError::InvalidDepthStencilCopy;
        if texture.pixelFormat() != MTLPixelFormat::Depth32Float_Stencil8
            || texture.sampleCount() != 1
        {
            return Err(invalid("requires single-sample Depth32Float_Stencil8"));
        }
        if copy.size.width == 0 || copy.size.height == 0 || copy.size.depth == 0 {
            return Ok(());
        }
        let align = |value: usize| value.checked_add(255).map(|value| value & !255);
        let depth_row = copy
            .size
            .width
            .checked_mul(4)
            .and_then(align)
            .ok_or(invalid("depth row overflow"))?;
        let stencil_row = align(copy.size.width).ok_or(invalid("stencil row overflow"))?;
        let depth_image = depth_row
            .checked_mul(copy.size.height)
            .ok_or(invalid("depth image overflow"))?;
        let stencil_image = stencil_row
            .checked_mul(copy.size.height)
            .ok_or(invalid("stencil image overflow"))?;
        let stencil_offset = depth_image
            .checked_mul(copy.size.depth)
            .ok_or(invalid("depth plane overflow"))?;
        let length = stencil_image
            .checked_mul(copy.size.depth)
            .and_then(|size| size.checked_add(stencil_offset))
            .ok_or(invalid("plane buffer overflow"))?;
        if copy.buffer_offset % 4 != 0
            || copy.bytes_per_row % 4 != 0
            || copy.bytes_per_image % 4 != 0
        {
            return Err(invalid("packed words must be aligned"));
        }
        let width_bytes = copy
            .size
            .width
            .checked_mul(8)
            .ok_or(invalid("packed row overflow"))?;
        let row_span = (copy.size.height - 1)
            .checked_mul(copy.bytes_per_row)
            .and_then(|offset| offset.checked_add(width_bytes))
            .ok_or(invalid("packed image overflow"))?;
        let end = (copy.size.depth - 1)
            .checked_mul(copy.bytes_per_image)
            .and_then(|offset| offset.checked_add(row_span))
            .and_then(|offset| offset.checked_add(copy.buffer_offset))
            .ok_or(invalid("packed buffer overflow"))?;
        if copy.bytes_per_row < width_bytes
            || (copy.size.depth > 1 && copy.bytes_per_image < row_span)
            || end > packed.length()
        {
            return Err(invalid("packed buffer range"));
        }
        // The MSL ABI is nine consecutive uints, with no implicit padding.
        let mut parameters = [0u32; 9];
        for (output, value) in parameters.iter_mut().zip([
            copy.size.width,
            copy.size.height,
            copy.size.depth,
            depth_row / 4,
            depth_image / 4,
            stencil_row,
            stencil_image,
            copy.bytes_per_row / 4,
            copy.bytes_per_image / 4,
        ]) {
            *output = u32::try_from(value).map_err(|_| invalid("shader stride overflow"))?;
        }
        if end / 4 > u32::MAX as usize || length > u32::MAX as usize {
            return Err(invalid("shader index overflow"));
        }
        let planes = MetalBuffer::new_private(&self.device, length)?;
        let blit = |scheduler: &mut MetalScheduler| {
            scheduler.with_blit_encoder(|encoder| {
                for (offset, row, image, option) in [
                    (0, depth_row, depth_image, MTLBlitOption::DepthFromDepthStencil),
                    (stencil_offset, stencil_row, stencil_image, MTLBlitOption::StencilFromDepthStencil),
                ] {
                    unsafe {
                        if upload {
                            encoder.copyFromBuffer_sourceOffset_sourceBytesPerRow_sourceBytesPerImage_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin_options(
                                planes.handle(), offset, row, image, copy.size, texture, copy.slice, copy.level, copy.origin, option);
                        } else {
                            encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage_options(
                                texture, copy.slice, copy.level, copy.origin, copy.size, planes.handle(), offset, row, image, option);
                        }
                    }
                }
            })
        };
        scheduler.request_outside_render_pass_operation_context();
        if !upload {
            blit(scheduler)?;
        }
        scheduler.with_compute_encoder(|encoder| unsafe {
            encoder.setComputePipelineState(if upload { &self.unpack } else { &self.pack });
            encoder.setBuffer_offset_atIndex(Some(planes.handle()), 0, 0);
            encoder.setBuffer_offset_atIndex(Some(planes.handle()), stencil_offset, 1);
            encoder.setBuffer_offset_atIndex(Some(packed.handle()), copy.buffer_offset, 2);
            encoder.setBytes_length_atIndex(
                NonNull::from(&parameters).cast(),
                std::mem::size_of_val(&parameters),
                3,
            );
            encoder.dispatchThreads_threadsPerThreadgroup(
                copy.size,
                MTLSize {
                    width: 8,
                    height: 8,
                    depth: 1,
                },
            );
        })?;
        // The encoder change orders packing and aspect transfers. Metal's
        // retained command buffer owns the buffers until GPU completion.
        if upload {
            blit(scheduler)?;
        }
        Ok(())
    }
}

pub struct MetalBlitHelper {
    device: MetalDevice,
    library: Retained<ProtocolObject<dyn MTLLibrary>>,
    vertex: Retained<ProtocolObject<dyn MTLFunction>>,
    fragment: Retained<ProtocolObject<dyn MTLFunction>>,
    blit_pipelines:
        HashMap<MetalFramebufferSignature, Retained<ProtocolObject<dyn MTLRenderPipelineState>>>,
    image_blit_pipelines:
        HashMap<BlitPipelineKey, Retained<ProtocolObject<dyn MTLRenderPipelineState>>>,
    nearest_sampler: Retained<ProtocolObject<dyn MTLSamplerState>>,
    linear_sampler: Retained<ProtocolObject<dyn MTLSamplerState>>,
    clear_pipelines:
        HashMap<ClearPipelineKey, Retained<ProtocolObject<dyn MTLRenderPipelineState>>>,
    clear_depth_states:
        HashMap<ClearPipelineKey, Retained<ProtocolObject<dyn MTLDepthStencilState>>>,
}

impl MetalBlitHelper {
    pub fn new(device: &MetalDevice) -> Result<Self, MetalBlitError> {
        let mut source = String::from(
            r#"
#include <metal_stdlib>
using namespace metal;

struct BlitParameters {
    float4 dst;
    float4 src;
    float2 target_size;
    float2 padding;
};

struct BlitVertexOut {
    float4 position [[position]];
    float2 uv;
};

vertex BlitVertexOut blit_vertex(uint id [[vertex_id]],
                                 constant BlitParameters& params [[buffer(0)]]) {
    const uint2 corners[4] = { uint2(0, 0), uint2(1, 0), uint2(0, 1), uint2(1, 1) };
    uint2 corner = corners[id];
    float2 pixel = mix(params.dst.xy, params.dst.zw, float2(corner));
    float2 uv = mix(params.src.xy, params.src.zw, float2(corner));
    BlitVertexOut output;
    output.position = float4(pixel.x / params.target_size.x * 2.0 - 1.0,
                             1.0 - pixel.y / params.target_size.y * 2.0,
                             0.0, 1.0);
    output.uv = uv;
    return output;
}

fragment float4 blit_fragment(BlitVertexOut input [[stage_in]],
                              texture2d<float> source [[texture(0)]],
                              sampler source_sampler [[sampler(0)]]) {
    return source.sample(source_sampler, input.uv, level(0.0));
}

fragment float4 blit_color_msaa(BlitVertexOut input [[stage_in]],
                               texture2d_ms<float> source [[texture(0)]],
                               uint sample_index [[sample_id]]) {
    uint2 coord = uint2(input.uv * float2(source.get_width(), source.get_height()));
    return source.read(coord, sample_index);
}
fragment float4 blit_color_resolve(BlitVertexOut input [[stage_in]],
                                  texture2d_ms<float> source [[texture(0)]]) {
    uint2 coord = uint2(input.uv * float2(source.get_width(), source.get_height()));
    float4 result = float4(0.0);
    for (uint i = 0; i < source.get_num_samples(); ++i) result += source.read(coord, i);
    return result / float(source.get_num_samples());
}

struct ClearParameters {
    float4 dst;
    float2 target_size;
    float depth;
    float padding;
    float4 color;
    int4 signed_color;
    uint4 unsigned_color;
};

struct ClearVertexOut { float4 position [[position]]; };
vertex ClearVertexOut clear_vertex(uint id [[vertex_id]],
                                   constant ClearParameters& params [[buffer(0)]]) {
    const uint2 corners[4] = { uint2(0, 0), uint2(1, 0), uint2(0, 1), uint2(1, 1) };
    float2 pixel = mix(params.dst.xy, params.dst.zw, float2(corners[id]));
    ClearVertexOut output;
    output.position = float4(pixel.x / params.target_size.x * 2.0 - 1.0,
                             1.0 - pixel.y / params.target_size.y * 2.0,
                             0.0, 1.0);
    return output;
}

struct ClearDepthOut { float depth [[depth(any)]]; };
fragment ClearDepthOut clear_depth_fragment(constant ClearParameters& params [[buffer(0)]]) {
    return { params.depth };
}
fragment void clear_stencil_fragment() {}
"#,
        );
        for (suffix, data_type) in [("sint", "int"), ("uint", "uint")] {
            for (variant, msaa, resolve) in [
                ("", false, false),
                ("_msaa", true, false),
                ("_resolve", true, true),
            ] {
                let texture = if msaa { "texture2d_ms" } else { "texture2d" };
                let sample_arg = if msaa && !resolve {
                    ", uint sample_index [[sample_id]]"
                } else {
                    ""
                };
                let sample = if msaa && !resolve {
                    "sample_index"
                } else {
                    "0"
                };
                source.push_str(&format!("\nfragment {data_type}4 blit_color_{suffix}{variant}(BlitVertexOut input [[stage_in]], {texture}<{data_type}> source [[texture(0)]]{sample_arg}) {{\nint2 coord = int2(floor(input.uv * float2(source.get_width(), source.get_height())));\nif (any(coord < int2(0)) || any(coord >= int2(source.get_width(), source.get_height()))) return {data_type}4(1);\nreturn source.read(uint2(coord), {sample});\n}}\n"));
            }
        }
        // These are the native counterparts of blit_depth[_stencil]_msaa.frag
        // and vulkan_blit_depth_stencil.frag. Resolve uses sample zero, not an
        // average or minimum; same-sample-count blits preserve each sample.
        for (name, depth, stencil) in [
            ("depth", true, false),
            ("stencil", false, true),
            ("depth_stencil", true, true),
        ] {
            for (suffix, msaa, resolve) in [
                ("", false, false),
                ("_msaa", true, false),
                ("_resolve", true, true),
            ] {
                let function = format!("blit_{name}{suffix}");
                source.push_str(&format!("\nstruct {function}_out {{\n"));
                if depth {
                    source.push_str("float d [[depth(any)]];\n");
                }
                if stencil {
                    source.push_str("uint s [[stencil]];\n");
                }
                source.push_str(&format!(
                    "}};\nfragment {function}_out {function}(BlitVertexOut input [[stage_in]]"
                ));
                if depth {
                    source.push_str(if msaa {
                        ", depth2d_ms<float> d [[texture(0)]]"
                    } else {
                        ", depth2d<float> d [[texture(0)]], sampler smp [[sampler(0)]]"
                    });
                }
                if stencil {
                    source.push_str(if msaa {
                        ", texture2d_ms<uint> s [[texture(1)]]"
                    } else {
                        ", texture2d<uint> s [[texture(1)]]"
                    });
                }
                if msaa && !resolve {
                    source.push_str(", uint sample_index [[sample_id]]");
                }
                let tex = if depth { "d" } else { "s" };
                source.push_str(&format!(") {{\n{function}_out result;\nint2 coord = int2(floor(input.uv * float2({tex}.get_width(), {tex}.get_height())));\nbool inside = all(coord >= int2(0)) && all(coord < int2({tex}.get_width(), {tex}.get_height()));\n"));
                let sample = if resolve { "0" } else { "sample_index" };
                if depth {
                    source.push_str(&if msaa {
                        format!("result.d = d.read(uint2(coord), {sample});\n")
                    } else {
                        "result.d = d.sample(smp, input.uv, level(0.0));\n".into()
                    });
                }
                if stencil {
                    source.push_str(&if msaa {
                        format!("result.s = s.read(uint2(coord), {sample}).r;\n")
                    } else {
                        "result.s = inside ? s.read(uint2(coord), 0).r : 1u;\n".into()
                    });
                }
                source.push_str("return result;\n}\n");
            }
        }
        for index in 0..crate::texture_cache::types::NUM_RT {
            source.push_str(&format!(
                r#"
struct ClearFloatOut{index} {{ float4 color [[color({index})]]; }};
fragment ClearFloatOut{index} clear_float_{index}(constant ClearParameters& p [[buffer(0)]]) {{ return {{ p.color }}; }}
struct ClearSintOut{index} {{ int4 color [[color({index})]]; }};
fragment ClearSintOut{index} clear_sint_{index}(constant ClearParameters& p [[buffer(0)]]) {{ return {{ p.signed_color }}; }}
struct ClearUintOut{index} {{ uint4 color [[color({index})]]; }};
fragment ClearUintOut{index} clear_uint_{index}(constant ClearParameters& p [[buffer(0)]]) {{ return {{ p.unsigned_color }}; }}
struct ClearFloatDepthOut{index} {{ float4 color [[color({index})]]; float depth [[depth(any)]]; }};
fragment ClearFloatDepthOut{index} clear_float_depth_{index}(constant ClearParameters& p [[buffer(0)]]) {{ return {{ p.color, p.depth }}; }}
struct ClearSintDepthOut{index} {{ int4 color [[color({index})]]; float depth [[depth(any)]]; }};
fragment ClearSintDepthOut{index} clear_sint_depth_{index}(constant ClearParameters& p [[buffer(0)]]) {{ return {{ p.signed_color, p.depth }}; }}
struct ClearUintDepthOut{index} {{ uint4 color [[color({index})]]; float depth [[depth(any)]]; }};
fragment ClearUintDepthOut{index} clear_uint_depth_{index}(constant ClearParameters& p [[buffer(0)]]) {{ return {{ p.unsigned_color, p.depth }}; }}
"#
            ));
        }
        let source = NSString::from_str(&source);
        let options = MTLCompileOptions::new();
        options.setLanguageVersion(MTLLanguageVersion::Version2_3);
        #[allow(deprecated)]
        options.setFastMathEnabled(false);
        let library = device
            .device()
            .newLibraryWithSource_options_error(&source, Some(&options))
            .map_err(|error| {
                MetalBlitError::ShaderCompile(error.localizedDescription().to_string())
            })?;
        let vertex = library
            .newFunctionWithName(&NSString::from_str("blit_vertex"))
            .ok_or_else(|| MetalBlitError::MissingEntryPoint("blit_vertex".into()))?;
        let fragment = library
            .newFunctionWithName(&NSString::from_str("blit_fragment"))
            .ok_or_else(|| MetalBlitError::MissingEntryPoint("blit_fragment".into()))?;
        let sampler = |filter| {
            let descriptor = MTLSamplerDescriptor::new();
            descriptor.setMinFilter(filter);
            descriptor.setMagFilter(filter);
            descriptor.setMipFilter(MTLSamplerMipFilter::Nearest);
            descriptor.setLodMinClamp(0.0);
            descriptor.setLodMaxClamp(0.0);
            descriptor.setSAddressMode(MTLSamplerAddressMode::ClampToBorderColor);
            descriptor.setTAddressMode(MTLSamplerAddressMode::ClampToBorderColor);
            descriptor.setRAddressMode(MTLSamplerAddressMode::ClampToBorderColor);
            descriptor.setBorderColor(MTLSamplerBorderColor::OpaqueWhite);
            descriptor.setNormalizedCoordinates(true);
            device
                .device()
                .newSamplerStateWithDescriptor(&descriptor)
                .ok_or(MetalBlitError::NoSampler)
        };
        Ok(Self {
            device: device.clone(),
            library,
            vertex,
            fragment,
            blit_pipelines: HashMap::new(),
            image_blit_pipelines: HashMap::new(),
            nearest_sampler: sampler(MTLSamplerMinMagFilter::Nearest)?,
            linear_sampler: sampler(MTLSamplerMinMagFilter::Linear)?,
            clear_pipelines: HashMap::new(),
            clear_depth_states: HashMap::new(),
        })
    }

    fn pipeline(
        &mut self,
        signature: MetalFramebufferSignature,
    ) -> Result<Retained<ProtocolObject<dyn MTLRenderPipelineState>>, MetalBlitError> {
        if let Some(pipeline) = self.blit_pipelines.get(&signature) {
            return Ok(pipeline.clone());
        }
        let descriptor = MTLRenderPipelineDescriptor::new();
        descriptor.setVertexFunction(Some(&self.vertex));
        descriptor.setFragmentFunction(Some(&self.fragment));
        descriptor.setRasterSampleCount(signature.samples as usize);
        descriptor.setDepthAttachmentPixelFormat(signature.depth_format);
        descriptor.setStencilAttachmentPixelFormat(signature.stencil_format);
        let attachments = descriptor.colorAttachments();
        for (index, format) in signature.color_formats.iter().enumerate() {
            unsafe { attachments.objectAtIndexedSubscript(index) }.setPixelFormat(*format);
        }
        let pipeline = self
            .device
            .device()
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .map_err(|error| MetalBlitError::Pipeline(error.localizedDescription().to_string()))?;
        self.blit_pipelines.insert(signature, pipeline.clone());
        Ok(pipeline)
    }

    fn clear_pipeline(
        &mut self,
        key: ClearPipelineKey,
    ) -> Result<Retained<ProtocolObject<dyn MTLRenderPipelineState>>, MetalBlitError> {
        if let Some(pipeline) = self.clear_pipelines.get(&key) {
            return Ok(pipeline.clone());
        }
        let fragment_name = match (key.color_attachment, key.color_type, key.depth) {
            (Some(index), MetalClearColorType::Float, false) => format!("clear_float_{index}"),
            (Some(index), MetalClearColorType::Sint, false) => format!("clear_sint_{index}"),
            (Some(index), MetalClearColorType::Uint, false) => format!("clear_uint_{index}"),
            (Some(index), MetalClearColorType::Float, true) => format!("clear_float_depth_{index}"),
            (Some(index), MetalClearColorType::Sint, true) => format!("clear_sint_depth_{index}"),
            (Some(index), MetalClearColorType::Uint, true) => format!("clear_uint_depth_{index}"),
            (None, _, true) => "clear_depth_fragment".into(),
            (None, _, false) => "clear_stencil_fragment".into(),
        };
        let fragment = self
            .library
            .newFunctionWithName(&NSString::from_str(&fragment_name))
            .ok_or_else(|| MetalBlitError::MissingEntryPoint(fragment_name.clone()))?;
        let clear_vertex = self
            .library
            .newFunctionWithName(&NSString::from_str("clear_vertex"))
            .ok_or_else(|| MetalBlitError::MissingEntryPoint("clear_vertex".into()))?;
        let descriptor = MTLRenderPipelineDescriptor::new();
        descriptor.setVertexFunction(Some(&clear_vertex));
        descriptor.setFragmentFunction(Some(&fragment));
        descriptor.setRasterSampleCount(key.signature.samples as usize);
        descriptor.setDepthAttachmentPixelFormat(key.signature.depth_format);
        descriptor.setStencilAttachmentPixelFormat(key.signature.stencil_format);
        let attachments = descriptor.colorAttachments();
        for (index, format) in key.signature.color_formats.iter().enumerate() {
            let attachment = unsafe { attachments.objectAtIndexedSubscript(index) };
            attachment.setPixelFormat(*format);
            if key.color_attachment == Some(index as u8) {
                attachment.setWriteMask(color_write_mask(key.color_mask));
            } else {
                attachment.setWriteMask(MTLColorWriteMask::None);
            }
        }
        let pipeline = self
            .device
            .device()
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .map_err(|error| MetalBlitError::Pipeline(error.localizedDescription().to_string()))?;
        self.clear_pipelines.insert(key, pipeline.clone());
        Ok(pipeline)
    }

    fn clear_depth_state(
        &mut self,
        key: ClearPipelineKey,
    ) -> Result<Retained<ProtocolObject<dyn MTLDepthStencilState>>, MetalBlitError> {
        if let Some(state) = self.clear_depth_states.get(&key) {
            return Ok(state.clone());
        }
        let descriptor = MTLDepthStencilDescriptor::new();
        descriptor.setDepthCompareFunction(MTLCompareFunction::Always);
        descriptor.setDepthWriteEnabled(key.depth);
        if key.stencil {
            let stencil = MTLStencilDescriptor::new();
            stencil.setStencilCompareFunction(MTLCompareFunction::Always);
            stencil.setStencilFailureOperation(MTLStencilOperation::Keep);
            stencil.setDepthFailureOperation(MTLStencilOperation::Keep);
            stencil.setDepthStencilPassOperation(MTLStencilOperation::Replace);
            stencil.setReadMask(u32::MAX);
            stencil.setWriteMask(key.stencil_write_mask);
            descriptor.setFrontFaceStencil(Some(&stencil));
            descriptor.setBackFaceStencil(Some(&stencil));
        }
        let state = self
            .device
            .device()
            .newDepthStencilStateWithDescriptor(&descriptor)
            .ok_or(MetalBlitError::NoDepthStencilState)?;
        self.clear_depth_states.insert(key, state.clone());
        Ok(state)
    }

    fn image_blit_pipeline(
        &mut self,
        key: BlitPipelineKey,
    ) -> Result<Retained<ProtocolObject<dyn MTLRenderPipelineState>>, MetalBlitError> {
        if let Some(pipeline) = self.image_blit_pipelines.get(&key) {
            return Ok(pipeline.clone());
        }
        let aspect = match key.aspect {
            BlitAspect::Color => match key.color_type {
                MetalClearColorType::Float => "color",
                MetalClearColorType::Sint => "color_sint",
                MetalClearColorType::Uint => "color_uint",
            },
            BlitAspect::Depth => "depth",
            BlitAspect::Stencil => "stencil",
            BlitAspect::DepthStencil => "depth_stencil",
        };
        let name = if key.source_msaa {
            format!(
                "blit_{aspect}_{}",
                if key.signature.samples == 1 {
                    "resolve"
                } else {
                    "msaa"
                }
            )
        } else if key.aspect == BlitAspect::Color && key.color_type == MetalClearColorType::Float {
            "blit_fragment".into()
        } else {
            format!("blit_{aspect}")
        };
        let fragment = self
            .library
            .newFunctionWithName(&NSString::from_str(&name))
            .ok_or_else(|| MetalBlitError::MissingEntryPoint(name))?;
        let descriptor = MTLRenderPipelineDescriptor::new();
        descriptor.setVertexFunction(Some(&self.vertex));
        descriptor.setFragmentFunction(Some(&fragment));
        descriptor.setRasterSampleCount(key.signature.samples as usize);
        descriptor.setDepthAttachmentPixelFormat(key.signature.depth_format);
        descriptor.setStencilAttachmentPixelFormat(key.signature.stencil_format);
        for (index, format) in key.signature.color_formats.iter().enumerate() {
            let attachment = unsafe {
                descriptor
                    .colorAttachments()
                    .objectAtIndexedSubscript(index)
            };
            attachment.setPixelFormat(*format);
            // Eden keys color pipelines by operation but leaves blending disabled.
            attachment.setBlendingEnabled(false);
        }
        let pipeline = self
            .device
            .device()
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .map_err(|error| MetalBlitError::Pipeline(error.localizedDescription().to_string()))?;
        self.image_blit_pipelines.insert(key, pipeline.clone());
        Ok(pipeline)
    }

    /// Native counterpart of BlitImageHelper::BlitColor, with fixed blit samplers.
    #[allow(clippy::too_many_arguments)]
    pub fn blit_color(
        &mut self,
        scheduler: &mut MetalScheduler,
        framebuffer: &MetalFramebuffer,
        source: &MetalImageView,
        dst: MetalBlitRegion,
        src: MetalBlitRegion,
        filter: Filter,
        operation: Operation,
    ) -> Result<(), MetalBlitError> {
        self.record_image_blit(
            scheduler,
            framebuffer,
            source,
            dst,
            src,
            filter,
            operation,
            BlitAspect::Color,
        )
    }

    /// MSAA-to-MSAA preserves gl_SampleID; resolving color averages samples.
    pub fn blit_color_msaa(
        &mut self,
        scheduler: &mut MetalScheduler,
        framebuffer: &MetalFramebuffer,
        source: &MetalImageView,
        dst: MetalBlitRegion,
        src: MetalBlitRegion,
    ) -> Result<(), MetalBlitError> {
        self.record_image_blit(
            scheduler,
            framebuffer,
            source,
            dst,
            src,
            Filter::Point,
            Operation::SrcCopy,
            BlitAspect::Color,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn blit_depth_stencil(
        &mut self,
        scheduler: &mut MetalScheduler,
        framebuffer: &MetalFramebuffer,
        source: &MetalImageView,
        dst: MetalBlitRegion,
        src: MetalBlitRegion,
        filter: Filter,
        operation: Operation,
    ) -> Result<(), MetalBlitError> {
        let aspect = match get_format_type(source.base().format) {
            SurfaceType::Depth => BlitAspect::Depth,
            SurfaceType::Stencil => BlitAspect::Stencil,
            SurfaceType::DepthStencil => BlitAspect::DepthStencil,
            _ => {
                return Err(MetalBlitError::InvalidBlit(
                    "expected a depth/stencil source",
                ))
            }
        };
        self.record_image_blit(
            scheduler,
            framebuffer,
            source,
            dst,
            src,
            filter,
            operation,
            aspect,
        )
    }

    pub fn resolve_depth_stencil(
        &mut self,
        scheduler: &mut MetalScheduler,
        framebuffer: &MetalFramebuffer,
        source: &MetalImageView,
        dst: MetalBlitRegion,
        src: MetalBlitRegion,
    ) -> Result<(), MetalBlitError> {
        self.blit_depth_stencil(
            scheduler,
            framebuffer,
            source,
            dst,
            src,
            Filter::Point,
            Operation::SrcCopy,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn record_image_blit(
        &mut self,
        scheduler: &mut MetalScheduler,
        framebuffer: &MetalFramebuffer,
        source: &MetalImageView,
        dst: MetalBlitRegion,
        src: MetalBlitRegion,
        filter: Filter,
        operation: Operation,
        aspect: BlitAspect,
    ) -> Result<(), MetalBlitError> {
        if dst.start.0 == dst.end.0 || dst.start.1 == dst.end.1 {
            return Ok(());
        }
        let key = BlitPipelineKey {
            signature: framebuffer.signature(),
            aspect,
            color_type: if crate::surface::is_pixel_format_signed_integer(source.base().format) {
                MetalClearColorType::Sint
            } else if crate::surface::is_pixel_format_integer(source.base().format) {
                MetalClearColorType::Uint
            } else {
                MetalClearColorType::Float
            },
            source_msaa: source.samples() > 1,
            operation,
        };
        let pipeline = self.image_blit_pipeline(key)?;
        let has_depth = matches!(aspect, BlitAspect::Depth | BlitAspect::DepthStencil);
        let has_stencil = matches!(aspect, BlitAspect::Stencil | BlitAspect::DepthStencil);
        let depth_state = self.clear_depth_state(ClearPipelineKey {
            signature: key.signature,
            color_attachment: None,
            color_type: MetalClearColorType::Float,
            color_mask: 0,
            depth: has_depth,
            stencil: has_stencil,
            stencil_write_mask: 0xff,
        })?;
        let texture = if has_depth {
            source.depth_blit_view()
        } else if has_stencil {
            source.stencil_blit_view()
        } else {
            source.handle(TextureType::Color2D)
        }
        .ok_or(MetalBlitError::InvalidBlit("missing sampled aspect view"))?;
        let stencil = if has_stencil {
            Some(
                source
                    .stencil_blit_view()
                    .ok_or(MetalBlitError::InvalidBlit("missing stencil view"))?,
            )
        } else {
            None
        };
        let render_area = framebuffer.render_area();
        let size = (
            texture.width().max(1) as f32,
            texture.height().max(1) as f32,
        );
        let parameters = BlitParameters {
            dst: [
                dst.start.0 as f32,
                dst.start.1 as f32,
                dst.end.0 as f32,
                dst.end.1 as f32,
            ],
            src: [
                src.start.0 as f32 / size.0,
                src.start.1 as f32 / size.1,
                src.end.0 as f32 / size.0,
                src.end.1 as f32 / size.1,
            ],
            target_size: [render_area.0 as f32, render_area.1 as f32],
            _padding: [0.0; 2],
        };
        let sampler = if filter == Filter::Bilinear {
            &self.linear_sampler
        } else {
            &self.nearest_sampler
        };
        // Starting a fresh encoder both orders prior texture writes and prevents
        // helper state from leaking into a retained guest render pass.
        scheduler.begin_render_pass(&framebuffer.render_pass_descriptor())?;
        scheduler.with_render_encoder(|encoder| unsafe {
            MetalQueryCache::configure_draw(encoder, None);
            encoder.setRenderPipelineState(&pipeline);
            encoder.setDepthStencilState(Some(&depth_state));
            encoder.setCullMode(MTLCullMode::None);
            encoder.setViewport(MTLViewport {
                originX: 0.0,
                originY: 0.0,
                width: render_area.0 as f64,
                height: render_area.1 as f64,
                znear: 0.0,
                zfar: 1.0,
            });
            encoder.setScissorRect(MTLScissorRect {
                x: 0,
                y: 0,
                width: render_area.0 as usize,
                height: render_area.1 as usize,
            });
            encoder.setVertexBytes_length_atIndex(
                NonNull::from(&parameters).cast(),
                std::mem::size_of_val(&parameters),
                0,
            );
            encoder.setFragmentTexture_atIndex(Some(texture), 0);
            encoder.setFragmentTexture_atIndex(stencil, 1);
            encoder.setFragmentSamplerState_atIndex(Some(sampler), 0);
            encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::TriangleStrip, 0, 4);
        })?;
        scheduler.end_render_pass();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn blit_color_with_sampler(
        &mut self,
        scheduler: &mut MetalScheduler,
        render_pass: &MTLRenderPassDescriptor,
        signature: MetalFramebufferSignature,
        render_area: (u32, u32),
        source: &ProtocolObject<dyn MTLTexture>,
        sampler: &ProtocolObject<dyn MTLSamplerState>,
        dst: MetalBlitRegion,
        src: MetalBlitRegion,
        source_size: (u32, u32),
        visibility_query: Option<MetalVisibilityQuery>,
    ) -> Result<(), MetalBlitError> {
        let pipeline = self.pipeline(signature)?;
        let parameters = BlitParameters {
            dst: [
                dst.start.0 as f32,
                dst.start.1 as f32,
                dst.end.0 as f32,
                dst.end.1 as f32,
            ],
            src: [
                src.start.0 as f32 / source_size.0.max(1) as f32,
                src.start.1 as f32 / source_size.1.max(1) as f32,
                src.end.0 as f32 / source_size.0.max(1) as f32,
                src.end.1 as f32 / source_size.1.max(1) as f32,
            ],
            target_size: [render_area.0.max(1) as f32, render_area.1.max(1) as f32],
            _padding: [0.0; 2],
        };
        scheduler.begin_render_pass(render_pass)?;
        scheduler.with_render_encoder(|encoder| unsafe {
            MetalQueryCache::configure_draw(encoder, visibility_query);
            encoder.setRenderPipelineState(&pipeline);
            encoder.setVertexBytes_length_atIndex(
                NonNull::new(
                    (&parameters as *const BlitParameters)
                        .cast_mut()
                        .cast::<c_void>(),
                )
                .expect("stack parameter pointer is non-null"),
                std::mem::size_of::<BlitParameters>(),
                0,
            );
            encoder.setFragmentTexture_atIndex(Some(source), 0);
            encoder.setFragmentSamplerState_atIndex(Some(sampler), 0);
            encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::TriangleStrip, 0, 4);
        })?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn clear_attachments(
        &mut self,
        scheduler: &mut MetalScheduler,
        render_pass: &MTLRenderPassDescriptor,
        signature: MetalFramebufferSignature,
        color_attachment: Option<u8>,
        color_type: MetalClearColorType,
        color_mask: u8,
        depth: bool,
        stencil: bool,
        stencil_write_mask: u32,
        parameters: MetalClearParameters,
    ) -> Result<(), MetalBlitError> {
        let key = ClearPipelineKey {
            signature,
            color_attachment,
            color_type,
            color_mask,
            depth,
            stencil,
            stencil_write_mask,
        };
        let pipeline = self.clear_pipeline(key)?;
        let depth_state = self.clear_depth_state(key)?;
        let shader_parameters = ClearShaderParameters {
            dst: [
                parameters.region.start.0 as f32,
                parameters.region.start.1 as f32,
                parameters.region.end.0 as f32,
                parameters.region.end.1 as f32,
            ],
            target_size: [
                parameters.render_area.0.max(1) as f32,
                parameters.render_area.1.max(1) as f32,
            ],
            depth: parameters.depth,
            _padding: 0.0,
            color: parameters.color,
            signed_color: parameters.signed_color,
            unsigned_color: parameters.unsigned_color,
        };
        let pointer = NonNull::new(
            (&shader_parameters as *const ClearShaderParameters)
                .cast_mut()
                .cast::<c_void>(),
        )
        .expect("stack clear parameter pointer is non-null");
        scheduler.begin_render_pass(render_pass)?;
        scheduler.with_render_encoder(|encoder| unsafe {
            encoder.setRenderPipelineState(&pipeline);
            encoder.setDepthStencilState(Some(&depth_state));
            if stencil {
                encoder.setStencilReferenceValue(parameters.stencil);
            }
            encoder.setVertexBytes_length_atIndex(
                pointer,
                std::mem::size_of::<ClearShaderParameters>(),
                0,
            );
            encoder.setFragmentBytes_length_atIndex(
                pointer,
                std::mem::size_of::<ClearShaderParameters>(),
                0,
            );
            encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::TriangleStrip, 0, 4);
        })?;
        Ok(())
    }
}

fn color_write_mask(mask: u8) -> MTLColorWriteMask {
    let mut result = MTLColorWriteMask::None;
    if mask & 1 != 0 {
        result |= MTLColorWriteMask::Red;
    }
    if mask & 2 != 0 {
        result |= MTLColorWriteMask::Green;
    }
    if mask & 4 != 0 {
        result |= MTLColorWriteMask::Blue;
    }
    if mask & 8 != 0 {
        result |= MTLColorWriteMask::Alpha;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer_metal::metal_image::MetalImage;
    use crate::surface::PixelFormat;
    use crate::texture_cache::image_info::ImageInfo;
    use crate::texture_cache::image_view_base::ImageViewBase;
    use crate::texture_cache::image_view_info::ImageViewInfo;
    use crate::texture_cache::render_targets::RenderTargets;
    use crate::texture_cache::types::*;
    use objc2_metal::{MTLClearColor, MTLLoadAction};

    struct Target {
        image: MetalImage,
        view: MetalImageView,
        framebuffer: MetalFramebuffer,
        _base: Box<ImageViewBase>,
    }

    fn target(device: &MetalDevice, format: PixelFormat, samples: u32) -> Target {
        target_subresource(device, format, samples, 0, 0)
    }

    fn target_subresource(
        device: &MetalDevice,
        format: PixelFormat,
        samples: u32,
        level: i32,
        layer: i32,
    ) -> Target {
        let scale = if samples == 4 { 2 } else { 1 };
        let info = ImageInfo {
            format,
            image_type: ImageType::E2D,
            size: Extent3D {
                width: (4 * scale) << level,
                height: (4 * scale) << level,
                depth: 1,
            },
            resources: SubresourceExtent {
                levels: level + 1,
                layers: layer + 1,
            },
            num_samples: samples,
            ..ImageInfo::default()
        };
        let image = MetalImage::new(device, &info).unwrap();
        let view_info = ImageViewInfo::for_render_target(
            ImageViewType::E2D,
            format,
            SubresourceRange {
                base: SubresourceBase { level, layer },
                extent: SubresourceExtent {
                    levels: 1,
                    layers: 1,
                },
            },
        );
        let mut base = Box::new(ImageViewBase::new(
            &view_info,
            &info,
            ImageId { index: 1 },
            0x1000,
        ));
        let view = MetalImageView::new(NonNull::from(base.as_mut()), &view_info, &image).unwrap();
        let mut colors = [None; NUM_RT];
        let color = get_format_type(format) == SurfaceType::ColorTexture;
        if color {
            colors[0] = Some(&view);
        }
        let framebuffer = MetalFramebuffer::new(
            colors,
            if color { None } else { Some(&view) },
            &RenderTargets {
                size: Extent2D {
                    width: 4,
                    height: 4,
                },
                ..RenderTargets::default()
            },
        )
        .unwrap();
        Target {
            image,
            view,
            framebuffer,
            _base: base,
        }
    }

    fn clear(scheduler: &mut MetalScheduler, target: &Target, depth: f64, stencil: u32) {
        let descriptor = target.framebuffer.render_pass_descriptor();
        if target.framebuffer.num_color_buffers() != 0 {
            let attachment = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
            attachment.setLoadAction(MTLLoadAction::Clear);
            attachment.setClearColor(MTLClearColor {
                red: 0.25,
                green: 0.5,
                blue: 0.75,
                alpha: 1.0,
            });
        }
        if target.framebuffer.has_depth() {
            descriptor
                .depthAttachment()
                .setLoadAction(MTLLoadAction::Clear);
            descriptor.depthAttachment().setClearDepth(depth);
        }
        if target.framebuffer.has_stencil() {
            descriptor
                .stencilAttachment()
                .setLoadAction(MTLLoadAction::Clear);
            descriptor.stencilAttachment().setClearStencil(stencil);
        }
        scheduler.begin_render_pass(&descriptor).unwrap();
        scheduler.end_render_pass();
    }

    #[test]
    fn native_blit_variants_preserve_color_depth_and_stencil() {
        let device = MetalDevice::new().unwrap();
        let mut helper = MetalBlitHelper::new(&device).unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let packer = MetalDepthStencilCopy::new(&device).unwrap();
        let region = MetalBlitRegion {
            start: (0, 0),
            end: (4, 4),
        };
        for format in [
            PixelFormat::A8B8G8R8Unorm,
            PixelFormat::D32Float,
            PixelFormat::D32FloatS8Uint,
            PixelFormat::S8Uint,
        ] {
            for (src_samples, dst_samples) in [(1, 1), (4, 1), (4, 4)] {
                let source = target(&device, format, src_samples);
                let destination = target(&device, format, dst_samples);
                clear(&mut scheduler, &source, 0.375, 0x9b);
                clear(&mut scheduler, &destination, 0.875, 0x12);
                if get_format_type(format) == SurfaceType::ColorTexture {
                    helper
                        .blit_color(
                            &mut scheduler,
                            &destination.framebuffer,
                            &source.view,
                            region,
                            region,
                            Filter::Point,
                            Operation::SrcCopy,
                        )
                        .unwrap();
                } else {
                    helper
                        .blit_depth_stencil(
                            &mut scheduler,
                            &destination.framebuffer,
                            &source.view,
                            region,
                            region,
                            Filter::Point,
                            Operation::SrcCopy,
                        )
                        .unwrap();
                }
                let resolved;
                let result = if dst_samples > 1 {
                    resolved = target(&device, format, 1);
                    if get_format_type(format) == SurfaceType::ColorTexture {
                        helper
                            .blit_color_msaa(
                                &mut scheduler,
                                &resolved.framebuffer,
                                &destination.view,
                                region,
                                region,
                            )
                            .unwrap();
                    } else {
                        helper
                            .resolve_depth_stencil(
                                &mut scheduler,
                                &resolved.framebuffer,
                                &destination.view,
                                region,
                                region,
                            )
                            .unwrap();
                    }
                    &resolved
                } else {
                    &destination
                };
                let bytes_per_pixel = match format {
                    PixelFormat::S8Uint => 1,
                    PixelFormat::D32FloatS8Uint => 8,
                    _ => 4,
                };
                let buffer = MetalBuffer::new(&device, 1024).unwrap();
                if format == PixelFormat::D32FloatS8Uint {
                    packer
                        .copy(
                            &mut scheduler,
                            result.image.handle(),
                            &buffer,
                            MetalDepthStencilBufferCopy {
                                buffer_offset: 0,
                                bytes_per_row: 256,
                                bytes_per_image: 1024,
                                slice: 0,
                                level: 0,
                                origin: MTLOrigin { x: 0, y: 0, z: 0 },
                                size: MTLSize {
                                    width: 4,
                                    height: 4,
                                    depth: 1,
                                },
                            },
                            false,
                        )
                        .unwrap();
                } else {
                    scheduler.with_blit_encoder(|encoder| unsafe {
                        encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                            result.image.handle(), 0, 0, MTLOrigin { x: 0, y: 0, z: 0 }, MTLSize { width: 4, height: 4, depth: 1 }, buffer.handle(), 0, 256, 1024);
                    }).unwrap();
                }
                scheduler.finish_all().unwrap();
                let mut bytes = [0; 1024];
                buffer.read(0, &mut bytes).unwrap();
                for y in 0..4 {
                    for x in 0..4 {
                        let offset = y * 256 + x * bytes_per_pixel;
                        let pixel = &bytes[offset..offset + bytes_per_pixel];
                        match format {
                            PixelFormat::S8Uint => assert_eq!(pixel, &[0x9b]),
                            PixelFormat::D32FloatS8Uint => {
                                assert_eq!(
                                    f32::from_le_bytes(pixel[..4].try_into().unwrap()),
                                    0.375
                                );
                                assert_eq!(&pixel[4..], &[0x9b, 0, 0, 0]);
                            }
                            PixelFormat::D32Float => {
                                assert_eq!(f32::from_le_bytes(pixel.try_into().unwrap()), 0.375)
                            }
                            _ => assert_eq!(pixel, &[64, 128, 191, 255]),
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn color_blits_preserve_subresources_flips_filtering_and_untouched_pixels() {
        let device = MetalDevice::new().unwrap();
        let source = target_subresource(&device, PixelFormat::A8B8G8R8Unorm, 1, 1, 1);
        let destination = target_subresource(&device, PixelFormat::A8B8G8R8Unorm, 1, 1, 1);
        let upload = MetalBuffer::new(&device, 64).unwrap();
        let flipped_download = MetalBuffer::new(&device, 64).unwrap();
        let linear_download = MetalBuffer::new(&device, 64).unwrap();
        let pixels = (0..16)
            .flat_map(|i| [(i % 4 * 64) as u8, (i / 4 * 64) as u8, 0, 255])
            .collect::<Vec<_>>();
        upload.write(0, &pixels).unwrap();
        let copy = BufferImageCopy {
            buffer_size: 64,
            image_extent: Extent3D {
                width: 4,
                height: 4,
                depth: 1,
            },
            image_subresource: SubresourceLayers {
                base_level: 1,
                base_layer: 1,
                num_layers: 1,
            },
            ..BufferImageCopy::default()
        };
        let mut scheduler = MetalScheduler::new(&device);
        let mut helper = MetalBlitHelper::new(&device).unwrap();
        source
            .image
            .upload_memory(&mut scheduler, &upload, 0, &[copy])
            .unwrap();
        clear(&mut scheduler, &destination, 0.0, 0);
        helper
            .blit_color(
                &mut scheduler,
                &destination.framebuffer,
                &source.view,
                MetalBlitRegion {
                    start: (1, 1),
                    end: (3, 3),
                },
                MetalBlitRegion {
                    start: (3, 3),
                    end: (1, 1),
                },
                Filter::Point,
                Operation::SrcCopy,
            )
            .unwrap();
        destination
            .image
            .download_memory(&mut scheduler, &flipped_download, 0, &[copy])
            .unwrap();
        helper
            .blit_color(
                &mut scheduler,
                &destination.framebuffer,
                &source.view,
                MetalBlitRegion {
                    start: (0, 0),
                    end: (4, 4),
                },
                MetalBlitRegion {
                    start: (1, 1),
                    end: (3, 3),
                },
                Filter::Bilinear,
                Operation::SrcCopy,
            )
            .unwrap();
        destination
            .image
            .download_memory(&mut scheduler, &linear_download, 0, &[copy])
            .unwrap();
        scheduler.finish_all().unwrap();
        let mut flipped = [0; 64];
        let mut linear = [0; 64];
        flipped_download.read(0, &mut flipped).unwrap();
        linear_download.read(0, &mut linear).unwrap();
        for y in 0..4 {
            for x in 0..4 {
                let offset = (y * 4 + x) * 4;
                let expected = if (1..3).contains(&x) && (1..3).contains(&y) {
                    [((3 - x) * 64) as u8, ((3 - y) * 64) as u8, 0, 255]
                } else {
                    [64, 128, 191, 255]
                };
                assert_eq!(
                    &flipped[offset..offset + 4],
                    &expected,
                    "flipped ({x}, {y})"
                );
                assert_eq!(
                    &linear[offset..offset + 4],
                    &[48 + 32 * x as u8, 48 + 32 * y as u8, 0, 255],
                    "linear ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn depth_stencil_blit_uses_selected_mip_and_layer() {
        let device = MetalDevice::new().unwrap();
        let source = target_subresource(&device, PixelFormat::D32FloatS8Uint, 1, 1, 1);
        let destination = target_subresource(&device, PixelFormat::D32FloatS8Uint, 1, 1, 1);
        let packer = MetalDepthStencilCopy::new(&device).unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut helper = MetalBlitHelper::new(&device).unwrap();
        clear(&mut scheduler, &source, 0.375, 0xd7);
        clear(&mut scheduler, &destination, 0.875, 0x28);
        helper
            .blit_depth_stencil(
                &mut scheduler,
                &destination.framebuffer,
                &source.view,
                MetalBlitRegion {
                    start: (1, 0),
                    end: (3, 2),
                },
                MetalBlitRegion {
                    start: (3, 4),
                    end: (1, 2),
                },
                Filter::Point,
                Operation::SrcCopy,
            )
            .unwrap();
        let buffer = MetalBuffer::new(&device, 1024).unwrap();
        packer
            .copy(
                &mut scheduler,
                destination.image.handle(),
                &buffer,
                MetalDepthStencilBufferCopy {
                    buffer_offset: 0,
                    bytes_per_row: 256,
                    bytes_per_image: 1024,
                    slice: 1,
                    level: 1,
                    origin: MTLOrigin { x: 0, y: 0, z: 0 },
                    size: MTLSize {
                        width: 4,
                        height: 4,
                        depth: 1,
                    },
                },
                false,
            )
            .unwrap();
        scheduler.finish_all().unwrap();
        let mut bytes = [0; 1024];
        buffer.read(0, &mut bytes).unwrap();
        for y in 0..4 {
            for x in 0..4 {
                let offset = y * 256 + x * 8;
                let copied = (1..3).contains(&x) && y < 2;
                assert_eq!(
                    f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()),
                    if copied { 0.375 } else { 0.875 }
                );
                assert_eq!(bytes[offset + 4], if copied { 0xd7 } else { 0x28 });
            }
        }
    }

    #[test]
    fn integer_color_blits_preserve_all_word_bits() {
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut helper = MetalBlitHelper::new(&device).unwrap();
        for format in [PixelFormat::R32Uint, PixelFormat::R32Sint] {
            let source = target(&device, format, 1);
            let destination = target(&device, format, 1);
            let words = [0u32, 0xffff_ffff, 0x8000_0000, 0x7fff_ffff];
            let bytes = (0..16)
                .flat_map(|i| words[i % 4].to_le_bytes())
                .collect::<Vec<_>>();
            let upload = MetalBuffer::new(&device, 64).unwrap();
            let download = MetalBuffer::new(&device, 64).unwrap();
            upload.write(0, &bytes).unwrap();
            let copy = BufferImageCopy {
                buffer_size: 64,
                image_extent: Extent3D {
                    width: 4,
                    height: 4,
                    depth: 1,
                },
                ..BufferImageCopy::default()
            };
            source
                .image
                .upload_memory(&mut scheduler, &upload, 0, &[copy])
                .unwrap();
            let region = MetalBlitRegion {
                start: (0, 0),
                end: (4, 4),
            };
            helper
                .blit_color(
                    &mut scheduler,
                    &destination.framebuffer,
                    &source.view,
                    region,
                    region,
                    Filter::Point,
                    Operation::SrcCopy,
                )
                .unwrap();
            destination
                .image
                .download_memory(&mut scheduler, &download, 0, &[copy])
                .unwrap();
            scheduler.finish_all().unwrap();
            let mut actual = [0; 64];
            download.read(0, &mut actual).unwrap();
            assert_eq!(actual.as_slice(), bytes);
        }
    }

    #[test]
    fn msaa_blits_preserve_sample_ids_and_resolve_depth_from_sample_zero() {
        let device = MetalDevice::new().unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let mut helper = MetalBlitHelper::new(&device).unwrap();
        let source = target(&device, PixelFormat::D32FloatS8Uint, 4);
        let destination = target(&device, PixelFormat::D32FloatS8Uint, 4);
        let resolved = target(&device, PixelFormat::D32FloatS8Uint, 1);
        let shader = NSString::from_str(
            r#"
#include <metal_stdlib>
using namespace metal;
struct Out { float depth [[depth(any)]]; uint stencil [[stencil]]; };
fragment Out sample_values(uint sample_index [[sample_id]]) {
    return { 0.125f * float(sample_index + 1), 40u + sample_index };
}
"#,
        );
        let options = MTLCompileOptions::new();
        options.setLanguageVersion(MTLLanguageVersion::Version2_3);
        let library = device
            .device()
            .newLibraryWithSource_options_error(&shader, Some(&options))
            .unwrap();
        let fragment = library
            .newFunctionWithName(&NSString::from_str("sample_values"))
            .unwrap();
        let descriptor = MTLRenderPipelineDescriptor::new();
        descriptor.setVertexFunction(Some(&helper.vertex));
        descriptor.setFragmentFunction(Some(&fragment));
        descriptor.setRasterSampleCount(4);
        descriptor.setDepthAttachmentPixelFormat(MTLPixelFormat::Depth32Float_Stencil8);
        descriptor.setStencilAttachmentPixelFormat(MTLPixelFormat::Depth32Float_Stencil8);
        let pipeline = device
            .device()
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .unwrap();
        let depth_state = helper
            .clear_depth_state(ClearPipelineKey {
                signature: source.framebuffer.signature(),
                color_attachment: None,
                color_type: MetalClearColorType::Float,
                color_mask: 0,
                depth: true,
                stencil: true,
                stencil_write_mask: 0xff,
            })
            .unwrap();
        let parameters = BlitParameters {
            dst: [0.0, 0.0, 4.0, 4.0],
            src: [0.0, 0.0, 1.0, 1.0],
            target_size: [4.0, 4.0],
            _padding: [0.0; 2],
        };
        scheduler
            .begin_render_pass(&source.framebuffer.render_pass_descriptor())
            .unwrap();
        scheduler
            .with_render_encoder(|encoder| unsafe {
                encoder.setRenderPipelineState(&pipeline);
                encoder.setDepthStencilState(Some(&depth_state));
                encoder.setVertexBytes_length_atIndex(
                    NonNull::from(&parameters).cast(),
                    std::mem::size_of_val(&parameters),
                    0,
                );
                encoder.drawPrimitives_vertexStart_vertexCount(
                    MTLPrimitiveType::TriangleStrip,
                    0,
                    4,
                );
            })
            .unwrap();
        let region = MetalBlitRegion {
            start: (0, 0),
            end: (4, 4),
        };
        helper
            .blit_depth_stencil(
                &mut scheduler,
                &destination.framebuffer,
                &source.view,
                region,
                region,
                Filter::Point,
                Operation::SrcCopy,
            )
            .unwrap();
        // Read every destination sample on the GPU, independently of the resolve.
        let inspect = NSString::from_str(
            r#"
#include <metal_stdlib>
using namespace metal;
kernel void inspect_samples(depth2d_ms<float> d [[texture(0)]],
                            texture2d_ms<uint> s [[texture(1)]],
                            device uint2* out [[buffer(0)]], uint i [[thread_position_in_grid]]) {
    out[i] = uint2(as_type<uint>(d.read(uint2(1, 1), i)), s.read(uint2(1, 1), i).r);
}
"#,
        );
        let inspect_lib = device
            .device()
            .newLibraryWithSource_options_error(&inspect, Some(&options))
            .unwrap();
        let inspect_function = inspect_lib
            .newFunctionWithName(&NSString::from_str("inspect_samples"))
            .unwrap();
        let compute = device
            .device()
            .newComputePipelineStateWithFunction_error(&inspect_function)
            .unwrap();
        let samples = MetalBuffer::new(&device, 32).unwrap();
        scheduler
            .with_compute_encoder(|encoder| unsafe {
                encoder.setComputePipelineState(&compute);
                encoder.setTexture_atIndex(destination.view.depth_blit_view(), 0);
                encoder.setTexture_atIndex(destination.view.stencil_blit_view(), 1);
                encoder.setBuffer_offset_atIndex(Some(samples.handle()), 0, 0);
                encoder.dispatchThreads_threadsPerThreadgroup(
                    MTLSize {
                        width: 4,
                        height: 1,
                        depth: 1,
                    },
                    MTLSize {
                        width: 4,
                        height: 1,
                        depth: 1,
                    },
                );
            })
            .unwrap();
        helper
            .resolve_depth_stencil(
                &mut scheduler,
                &resolved.framebuffer,
                &destination.view,
                region,
                region,
            )
            .unwrap();
        let packed = MetalBuffer::new(&device, 1024).unwrap();
        MetalDepthStencilCopy::new(&device)
            .unwrap()
            .copy(
                &mut scheduler,
                resolved.image.handle(),
                &packed,
                MetalDepthStencilBufferCopy {
                    buffer_offset: 0,
                    bytes_per_row: 256,
                    bytes_per_image: 1024,
                    slice: 0,
                    level: 0,
                    origin: MTLOrigin { x: 0, y: 0, z: 0 },
                    size: MTLSize {
                        width: 4,
                        height: 4,
                        depth: 1,
                    },
                },
                false,
            )
            .unwrap();
        scheduler.finish_all().unwrap();
        let mut sample_bytes = [0; 32];
        samples.read(0, &mut sample_bytes).unwrap();
        for i in 0..4 {
            assert_eq!(
                f32::from_le_bytes(sample_bytes[i * 8..i * 8 + 4].try_into().unwrap()),
                0.125 * (i + 1) as f32
            );
            assert_eq!(
                u32::from_le_bytes(sample_bytes[i * 8 + 4..i * 8 + 8].try_into().unwrap()),
                40 + i as u32
            );
        }
        let mut pixel = [0; 8];
        packed.read(0, &mut pixel).unwrap();
        assert_eq!(f32::from_le_bytes(pixel[..4].try_into().unwrap()), 0.125);
        assert_eq!(&pixel[4..], &[40, 0, 0, 0]);
    }
}
