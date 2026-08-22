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
    MTLCompileOptions, MTLDevice as _, MTLFunction, MTLLanguageVersion, MTLLibrary,
    MTLPrimitiveType, MTLRenderCommandEncoder, MTLRenderPassDescriptor,
    MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLSamplerState, MTLTexture,
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

#[derive(Debug, Error)]
pub enum MetalBlitError {
    #[error("Metal blit shader compilation failed: {0}")]
    ShaderCompile(String),
    #[error("Metal blit shader entry point {0} is missing")]
    MissingEntryPoint(&'static str),
    #[error("Metal blit pipeline creation failed: {0}")]
    Pipeline(String),
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
}

pub struct MetalBlitHelper {
    device: MetalDevice,
    vertex: Retained<ProtocolObject<dyn MTLFunction>>,
    fragment: Retained<ProtocolObject<dyn MTLFunction>>,
    pipelines:
        HashMap<MetalFramebufferSignature, Retained<ProtocolObject<dyn MTLRenderPipelineState>>>,
}

impl MetalBlitHelper {
    pub fn new(device: &MetalDevice) -> Result<Self, MetalBlitError> {
        let source = NSString::from_str(
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
"#,
        );
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
            .ok_or(MetalBlitError::MissingEntryPoint("blit_vertex"))?;
        let fragment = library
            .newFunctionWithName(&NSString::from_str("blit_fragment"))
            .ok_or(MetalBlitError::MissingEntryPoint("blit_fragment"))?;
        Ok(Self {
            device: device.clone(),
            vertex,
            fragment,
            pipelines: HashMap::new(),
        })
    }

    fn pipeline(
        &mut self,
        signature: MetalFramebufferSignature,
    ) -> Result<Retained<ProtocolObject<dyn MTLRenderPipelineState>>, MetalBlitError> {
        if let Some(pipeline) = self.pipelines.get(&signature) {
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
        self.pipelines.insert(signature, pipeline.clone());
        Ok(pipeline)
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
}
