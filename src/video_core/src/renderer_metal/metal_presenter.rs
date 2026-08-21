// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal drawable acquisition and presentation.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLCommandBuffer, MTLCommandEncoder, MTLCompileOptions, MTLDevice, MTLDrawable,
    MTLLanguageVersion, MTLLibrary, MTLLoadAction, MTLPixelFormat, MTLRenderCommandEncoder,
    MTLRenderPassDescriptor, MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLSamplerDescriptor,
    MTLSamplerMinMagFilter, MTLSamplerMipFilter, MTLSamplerState, MTLStoreAction, MTLTexture,
};
use objc2_quartz_core::CAMetalDrawable;
use thiserror::Error;

use super::metal_layer::MetalLayer;
use super::metal_device::MetalDevice;
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};

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

pub struct MetalPresenter {
    layer: MetalLayer,
    pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    sampler: Retained<ProtocolObject<dyn MTLSamplerState>>,
}

impl MetalPresenter {
    pub fn new(layer: MetalLayer, device: &MetalDevice) -> Result<Self, MetalPresenterError> {
        let source = NSString::from_str(
            r#"
#include <metal_stdlib>
using namespace metal;
struct PresentOut { float4 position [[position]]; float2 uv; };
vertex PresentOut present_vertex(uint id [[vertex_id]]) {
    const float2 positions[3] = { float2(-1.0, -1.0), float2(3.0, -1.0), float2(-1.0, 3.0) };
    const float2 texcoords[3] = { float2(0.0, 1.0), float2(2.0, 1.0), float2(0.0, -1.0) };
    PresentOut output;
    output.position = float4(positions[id], 0.0, 1.0);
    output.uv = texcoords[id];
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
        let sampler_descriptor = MTLSamplerDescriptor::new();
        sampler_descriptor.setMinFilter(MTLSamplerMinMagFilter::Linear);
        sampler_descriptor.setMagFilter(MTLSamplerMinMagFilter::Linear);
        sampler_descriptor.setMipFilter(MTLSamplerMipFilter::NotMipmapped);
        let sampler = device
            .device()
            .newSamplerStateWithDescriptor(&sampler_descriptor)
            .ok_or(MetalPresenterError::NoSampler)?;
        Ok(Self {
            layer,
            pipeline,
            sampler,
        })
    }

    /// Acquire, clear and present one native drawable.
    ///
    /// This is the presentation primitive used by the compositor. It is kept
    /// independent from guest rendering so later compositing can replace the
    /// clear encoder with the fullscreen source-image pass without changing
    /// drawable ownership or submission ordering.
    pub fn present_texture(
        &self,
        scheduler: &mut MetalScheduler,
        source: &ProtocolObject<dyn MTLTexture>,
    ) -> Result<(), MetalPresenterError> {
        // Commit all guest rendering before the presentation command buffer.
        // One Metal queue preserves submission order, matching Eden's
        // `PresentManager::Present` flush-before-copy contract.
        scheduler.flush()?;
        let drawable = self
            .layer
            .as_ref()
            .nextDrawable()
            .ok_or(MetalPresenterError::NoDrawable)?;
        let command_buffer = scheduler.begin()?;
        let descriptor = MTLRenderPassDescriptor::renderPassDescriptor();
        let color_attachments = descriptor.colorAttachments();
        let color = unsafe { color_attachments.objectAtIndexedSubscript(0) };
        color.setTexture(Some(&drawable.texture()));
        color.setLoadAction(MTLLoadAction::DontCare);
        color.setStoreAction(MTLStoreAction::Store);

        let encoder = command_buffer
            .renderCommandEncoderWithDescriptor(&descriptor)
            .ok_or(MetalPresenterError::NoRenderEncoder)?;
        encoder.setRenderPipelineState(&self.pipeline);
        unsafe {
            encoder.setFragmentTexture_atIndex(Some(source), 0);
            encoder.setFragmentSamplerState_atIndex(Some(&self.sampler), 0);
            encoder.drawPrimitives_vertexStart_vertexCount(
                objc2_metal::MTLPrimitiveType::Triangle,
                0,
                3,
            );
        }
        encoder.endEncoding();

        let metal_drawable: &ProtocolObject<dyn MTLDrawable> = ProtocolObject::from_ref(&*drawable);
        command_buffer.presentDrawable(metal_drawable);
        scheduler.commit(command_buffer)?;
        Ok(())
    }

    pub fn layer(&self) -> &MetalLayer {
        &self.layer
    }
}
