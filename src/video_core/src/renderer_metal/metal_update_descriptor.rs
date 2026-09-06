// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native descriptor encoding counterpart of Eden's vk_update_descriptor.
//! Only sampler references use argument buffers; data buffers and textures keep
//! their explicit bindings. Metal's encoder owns the opaque binary layout.

use std::sync::Arc;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSArray;
use objc2_metal::{MTLArgumentDescriptor, MTLArgumentEncoder, MTLDataType, MTLDevice, MTLSamplerState};
use thiserror::Error;

use super::metal_buffer::{MetalBuffer, MetalBufferError};
use super::metal_device::MetalDevice;
use super::metal_shader::{validate_native_binding_layout, MetalResourceKind, MetalShaderBindingLayout, MetalShaderError};

#[derive(Debug, Error)]
pub enum MetalDescriptorError {
    #[error("Metal failed to create a sampler argument encoder")]
    Encoder,
    #[error("invalid or missing sampler argument ID {0}")]
    Sampler(u32),
    #[error(transparent)]
    Buffer(#[from] MetalBufferError),
    #[error(transparent)]
    Shader(#[from] MetalShaderError),
}

#[derive(Clone)]
pub struct MetalSamplerArgumentBuffer {
    pub index: u32,
    pub buffer: Arc<MetalBuffer>,
}

impl MetalSamplerArgumentBuffer {
    pub fn new<'a>(
        device: &MetalDevice,
        layout: &MetalShaderBindingLayout,
        samplers: impl IntoIterator<Item = (u32, &'a Retained<ProtocolObject<dyn MTLSamplerState>>)>,
    ) -> Result<Option<Self>, MetalDescriptorError> {
        let Some(index) = layout.sampler_argument_buffer_index else { return Ok(None) };
        validate_native_binding_layout(device.profile(), layout)?;
        let mut descriptors = Vec::new();
        let mut ids = vec![false; layout.sampler_count as usize];
        for resource in &layout.resources {
            if !matches!(resource.kind, MetalResourceKind::SampledImage | MetalResourceKind::SeparateSampler) {
                continue;
            }
            let count = resource.count.map_or(1, |count| count.get());
            let end = resource.sampler_index.checked_add(count)
                .filter(|end| *end <= layout.sampler_count)
                .ok_or(MetalDescriptorError::Sampler(resource.sampler_index))?;
            for id in resource.sampler_index..end {
                if std::mem::replace(&mut ids[id as usize], true) {
                    return Err(MetalDescriptorError::Sampler(id));
                }
            }
            let descriptor = MTLArgumentDescriptor::argumentDescriptor();
            descriptor.setDataType(MTLDataType::Sampler);
            descriptor.setIndex(resource.sampler_index as usize);
            // MSL arrays and individual members preserve their declared layout.
            descriptor.setArrayLength(resource.count.map_or(0, |count| count.get() as usize));
            descriptors.push(descriptor);
        }
        if let Some(id) = ids.iter().position(|present| !*present) {
            return Err(MetalDescriptorError::Sampler(id as u32));
        }
        let encoder = device.device()
            .newArgumentEncoderWithArguments(&NSArray::from_retained_slice(&descriptors))
            .ok_or(MetalDescriptorError::Encoder)?;
        let buffer = Arc::new(MetalBuffer::new(device, encoder.encodedLength())?);
        // New, CPU-visible storage; no in-flight argument buffer is overwritten.
        unsafe { encoder.setArgumentBuffer_offset(Some(buffer.handle()), 0) };
        ids.fill(false);
        for (id, sampler) in samplers {
            let present = ids.get_mut(id as usize).ok_or(MetalDescriptorError::Sampler(id))?;
            if std::mem::replace(present, true) {
                return Err(MetalDescriptorError::Sampler(id));
            }
            unsafe { encoder.setSamplerState_atIndex(Some(sampler), id as usize) };
        }
        if let Some(id) = ids.iter().position(|present| !*present) {
            return Err(MetalDescriptorError::Sampler(id as u32));
        }
        Ok(Some(Self { index, buffer }))
    }
}
