// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native counterpart of present/window_adapt_pass.{h,cpp}.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlendFactor, MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCompileOptions,
    MTLDevice, MTLLanguageVersion, MTLLibrary, MTLLoadAction, MTLPixelFormat,
    MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLRenderPipelineDescriptor,
    MTLRenderPipelineState, MTLSamplerDescriptor, MTLSamplerMinMagFilter, MTLSamplerMipFilter,
    MTLSamplerState, MTLStoreAction, MTLTexture, MTLViewport,
};
use thiserror::Error;

use super::super::metal_device::MetalDevice;
use super::super::metal_scheduler::MetalSchedulerError;
use super::layer::Layer;
use crate::framebuffer_config::BlendMode;

#[derive(Debug, Error)]
pub enum MetalPresenterError {
    #[error("CAMetalLayer did not return a drawable")]
    NoDrawable,
    #[error("Metal did not create a render command encoder")]
    NoRenderEncoder,
    #[error("Metal presentation shader compilation failed: {0}")]
    ShaderCompile(String),
    #[error("Metal presentation shader entry point {0} is missing")]
    MissingEntryPoint(&'static str),
    #[error("Metal presentation pipeline creation failed: {0}")]
    Pipeline(String),
    #[error("Metal presentation sampler creation failed")]
    NoSampler,
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
}

pub struct WindowAdaptPass {
    pipelines: [Retained<ProtocolObject<dyn MTLRenderPipelineState>>; 3],
    sampler: Retained<ProtocolObject<dyn MTLSamplerState>>,
}

impl WindowAdaptPass {
    pub fn new(device: &MetalDevice) -> Result<Self, MetalPresenterError> {
        let source = NSString::from_str(
            r#"
#include <metal_stdlib>
using namespace metal;
struct PresentOut { float4 position [[position]]; float2 uv; };
vertex PresentOut present_vertex(uint id [[vertex_id]], constant float4& crop [[buffer(0)]]) {
    const float2 positions[3] = { float2(-1.0, -1.0), float2(3.0, -1.0), float2(-1.0, 3.0) };
    const float2 texcoords[3] = { float2(0.0, 1.0), float2(2.0, 1.0), float2(0.0, -1.0) };
    PresentOut output;
    output.position = float4(positions[id], 0.0, 1.0);
    output.uv = mix(crop.xy, crop.zw, texcoords[id]);
    return output;
}
fragment float4 present_fragment(PresentOut input [[stage_in]],
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
                MetalPresenterError::ShaderCompile(error.localizedDescription().to_string())
            })?;
        let vertex_name = NSString::from_str("present_vertex");
        let fragment_name = NSString::from_str("present_fragment");
        let vertex = library
            .newFunctionWithName(&vertex_name)
            .ok_or(MetalPresenterError::MissingEntryPoint("present_vertex"))?;
        let fragment = library
            .newFunctionWithName(&fragment_name)
            .ok_or(MetalPresenterError::MissingEntryPoint("present_fragment"))?;
        let descriptor = MTLRenderPipelineDescriptor::new();
        descriptor.setVertexFunction(Some(&vertex));
        descriptor.setFragmentFunction(Some(&fragment));
        let color = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
        color.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        let pipeline = device
            .device()
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .map_err(|error| {
                MetalPresenterError::Pipeline(error.localizedDescription().to_string())
            })?;
        color.setBlendingEnabled(true);
        color.setSourceRGBBlendFactor(MTLBlendFactor::One);
        color.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        // Eden overwrites destination alpha in both blending modes.
        color.setSourceAlphaBlendFactor(MTLBlendFactor::One);
        color.setDestinationAlphaBlendFactor(MTLBlendFactor::Zero);
        let premultiplied = device
            .device()
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .map_err(|e| MetalPresenterError::Pipeline(e.localizedDescription().to_string()))?;
        color.setSourceRGBBlendFactor(MTLBlendFactor::SourceAlpha);
        let coverage = device
            .device()
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .map_err(|e| MetalPresenterError::Pipeline(e.localizedDescription().to_string()))?;
        let sampler_descriptor = MTLSamplerDescriptor::new();
        sampler_descriptor.setMinFilter(MTLSamplerMinMagFilter::Linear);
        sampler_descriptor.setMagFilter(MTLSamplerMinMagFilter::Linear);
        sampler_descriptor.setMipFilter(MTLSamplerMipFilter::NotMipmapped);
        let sampler = device
            .device()
            .newSamplerStateWithDescriptor(&sampler_descriptor)
            .ok_or(MetalPresenterError::NoSampler)?;
        Ok(Self {
            pipelines: [pipeline, premultiplied, coverage],
            sampler,
        })
    }

    /// The same source-image pass serves presentation and offscreen capture.
    /// The caller owns target lifetime, command submission and any readback.
    pub fn draw(
        &self,
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
        layers: &[Layer],
        target: &ProtocolObject<dyn MTLTexture>,
        viewport: MTLViewport,
        clear: Option<MTLClearColor>,
    ) -> Result<(), MetalPresenterError> {
        let descriptor = MTLRenderPassDescriptor::renderPassDescriptor();
        let color_attachments = descriptor.colorAttachments();
        let color = unsafe { color_attachments.objectAtIndexedSubscript(0) };
        color.setTexture(Some(target));
        if let Some(clear) = clear {
            color.setLoadAction(MTLLoadAction::Clear);
            color.setClearColor(clear);
        } else {
            color.setLoadAction(MTLLoadAction::DontCare);
        }
        color.setStoreAction(MTLStoreAction::Store);

        let encoder = command_buffer
            .renderCommandEncoderWithDescriptor(&descriptor)
            .ok_or(MetalPresenterError::NoRenderEncoder)?;
        unsafe {
            encoder.setViewport(viewport);
            encoder.setFragmentSamplerState_atIndex(Some(&self.sampler), 0);
        }
        for layer in layers {
            let index = match layer.blending {
                BlendMode::Opaque => 0,
                BlendMode::Premultiplied => 1,
                BlendMode::Coverage => 2,
            };
            encoder.setRenderPipelineState(&self.pipelines[index]);
            unsafe {
                encoder.setVertexBytes_length_atIndex(
                    std::ptr::NonNull::from(&layer.crop).cast(),
                    std::mem::size_of_val(&layer.crop),
                    0,
                );
                encoder.setFragmentTexture_atIndex(Some(&layer.texture), 0);
                encoder.drawPrimitives_vertexStart_vertexCount(
                    objc2_metal::MTLPrimitiveType::Triangle,
                    0,
                    3,
                );
            }
        }
        encoder.endEncoding();

        Ok(())
    }
}
