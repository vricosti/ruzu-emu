// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal drawable acquisition and presentation.

use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLDrawable, MTLLoadAction,
    MTLRenderPassDescriptor, MTLStoreAction,
};
use objc2_quartz_core::CAMetalDrawable;
use thiserror::Error;

use super::metal_layer::MetalLayer;
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};

#[derive(Debug, Error)]
pub enum MetalPresenterError {
    #[error("CAMetalLayer did not return a drawable")]
    NoDrawable,
    #[error("Metal did not create a render command encoder")]
    NoRenderEncoder,
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
}

pub struct MetalPresenter {
    layer: MetalLayer,
}

impl MetalPresenter {
    pub fn new(layer: MetalLayer) -> Self {
        Self { layer }
    }

    /// Acquire, clear and present one native drawable.
    ///
    /// This is the presentation primitive used by the compositor. It is kept
    /// independent from guest rendering so later compositing can replace the
    /// clear encoder with the fullscreen source-image pass without changing
    /// drawable ownership or submission ordering.
    pub fn present_clear(
        &self,
        scheduler: &mut MetalScheduler,
        clear_color: MTLClearColor,
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
        color.setLoadAction(MTLLoadAction::Clear);
        color.setStoreAction(MTLStoreAction::Store);
        color.setClearColor(clear_color);

        let encoder = command_buffer
            .renderCommandEncoderWithDescriptor(&descriptor)
            .ok_or(MetalPresenterError::NoRenderEncoder)?;
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
