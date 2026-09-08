// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native drawable ownership, counterpart of Eden's PresentManager.
pub use super::present::window_adapt_pass::MetalPresenterError;
use super::present::{layer::Layer, window_adapt_pass::WindowAdaptPass};
use super::{metal_device::MetalDevice, metal_layer::MetalLayer, metal_scheduler::MetalScheduler};
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLClearColor, MTLCommandBuffer, MTLDrawable, MTLTexture, MTLViewport};
use objc2_quartz_core::CAMetalDrawable;

pub struct MetalPresenter {
    layer: MetalLayer,
    compositor: WindowAdaptPass,
}

impl MetalPresenter {
    pub fn new(layer: MetalLayer, device: &MetalDevice) -> Result<Self, MetalPresenterError> {
        Ok(Self {
            layer,
            compositor: WindowAdaptPass::new(device)?,
        })
    }

    pub fn present_layers(
        &self,
        scheduler: &mut MetalScheduler,
        layers: &[Layer],
    ) -> Result<(), MetalPresenterError> {
        scheduler.flush()?;
        let drawable = self
            .layer
            .as_ref()
            .nextDrawable()
            .ok_or(MetalPresenterError::NoDrawable)?;
        let command = scheduler.begin()?;
        let target = drawable.texture();
        self.draw_layers(
            &command,
            layers,
            &target,
            MTLViewport {
                originX: 0.0,
                originY: 0.0,
                width: target.width() as f64,
                height: target.height() as f64,
                znear: 0.0,
                zfar: 1.0,
            },
            Some(Self::background()),
        )?;
        let drawable: &ProtocolObject<dyn MTLDrawable> = ProtocolObject::from_ref(&*drawable);
        command.presentDrawable(drawable);
        scheduler.commit_presentation(command)?;
        Ok(())
    }

    pub fn background() -> MTLClearColor {
        let settings = common::settings::values();
        MTLClearColor {
            red: *settings.bg_red.get_value() as f64 / 255.0,
            green: *settings.bg_green.get_value() as f64 / 255.0,
            blue: *settings.bg_blue.get_value() as f64 / 255.0,
            alpha: 1.0,
        }
    }

    pub fn draw_layers(
        &self,
        command: &ProtocolObject<dyn MTLCommandBuffer>,
        layers: &[Layer],
        target: &ProtocolObject<dyn MTLTexture>,
        viewport: MTLViewport,
        clear: Option<MTLClearColor>,
    ) -> Result<(), MetalPresenterError> {
        self.compositor
            .draw(command, layers, target, viewport, clear)
    }

    pub fn layer(&self) -> &MetalLayer {
        &self.layer
    }
}
