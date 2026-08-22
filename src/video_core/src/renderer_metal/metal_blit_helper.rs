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
    MTLColorWriteMask, MTLCompareFunction, MTLCompileOptions, MTLDepthStencilDescriptor,
    MTLDepthStencilState, MTLDevice as _, MTLFunction, MTLLanguageVersion, MTLLibrary,
    MTLPrimitiveType, MTLRenderCommandEncoder, MTLRenderPassDescriptor,
    MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLSamplerState, MTLStencilDescriptor,
    MTLStencilOperation, MTLTexture,
};
use thiserror::Error;

use super::metal_device::MetalDevice;
use super::metal_framebuffer::MetalFramebufferSignature;
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};

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

#[derive(Debug, Error)]
pub enum MetalBlitError {
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

pub struct MetalBlitHelper {
    device: MetalDevice,
    library: Retained<ProtocolObject<dyn MTLLibrary>>,
    vertex: Retained<ProtocolObject<dyn MTLFunction>>,
    fragment: Retained<ProtocolObject<dyn MTLFunction>>,
    blit_pipelines:
        HashMap<MetalFramebufferSignature, Retained<ProtocolObject<dyn MTLRenderPipelineState>>>,
    clear_pipelines: HashMap<ClearPipelineKey, Retained<ProtocolObject<dyn MTLRenderPipelineState>>>,
    clear_depth_states: HashMap<ClearPipelineKey, Retained<ProtocolObject<dyn MTLDepthStencilState>>>,
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
    return source.sample(source_sampler, input.uv);
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
        Ok(Self {
            device: device.clone(),
            library,
            vertex,
            fragment,
            blit_pipelines: HashMap::new(),
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
