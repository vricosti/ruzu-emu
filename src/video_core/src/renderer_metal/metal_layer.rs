// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native `CAMetalLayer` ownership and configuration.

use objc2::rc::Retained;
use objc2_metal::MTLPixelFormat;
use objc2_quartz_core::CAMetalLayer;
use thiserror::Error;

use super::metal_device::MetalDevice;

#[derive(Debug, Error)]
pub enum MetalLayerError {
    #[error("the frontend did not provide a CAMetalLayer")]
    MissingLayer,
}

/// Retains the frontend-owned layer while the renderer is alive.
///
/// SDL and GTK create the layer on the UI thread and keep its containing view
/// alive. Metal permits obtaining drawables from the renderer thread. All
/// methods on this owner are called only by that renderer thread.
pub struct MetalLayer {
    layer: Retained<CAMetalLayer>,
}

// SAFETY: the retained object is never accessed concurrently. RendererBase is
// moved to the GPU thread before emulation starts and owns this value until
// shutdown; the frontend only changes geometry on the main thread through
// CoreAnimation's layer properties, as it already does for MoltenVK.
unsafe impl Send for MetalLayer {}

impl MetalLayer {
    /// Retain the `CAMetalLayer*` supplied through `WindowSystemInfo`.
    ///
    /// # Safety
    ///
    /// `raw_layer` must be either null or a valid `CAMetalLayer*` whose
    /// frontend owner remains alive for the renderer lifetime.
    pub unsafe fn from_raw(
        raw_layer: usize,
        device: &MetalDevice,
    ) -> Result<Self, MetalLayerError> {
        if raw_layer == 0 {
            return Err(MetalLayerError::MissingLayer);
        }
        let layer = unsafe { Retained::retain(raw_layer as *mut CAMetalLayer) }
            .ok_or(MetalLayerError::MissingLayer)?;
        layer.setDevice(Some(device.device()));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        // The compositor samples/copies the drawable as well as rendering to
        // it, so it cannot use framebuffer-only storage.
        layer.setFramebufferOnly(false);
        layer.setMaximumDrawableCount(3);
        layer.setPresentsWithTransaction(false);
        layer.setAllowsNextDrawableTimeout(true);
        Ok(Self { layer })
    }

    pub fn as_ref(&self) -> &CAMetalLayer {
        &self.layer
    }

    pub fn pixel_format(&self) -> MTLPixelFormat {
        self.layer.pixelFormat()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configures_native_layer_for_renderer_presentation() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let raw_layer = CAMetalLayer::new();
        let layer = unsafe {
            MetalLayer::from_raw(Retained::as_ptr(&raw_layer) as usize, &device)
                .expect("standalone CAMetalLayer must be retainable")
        };
        assert_eq!(layer.pixel_format(), MTLPixelFormat::BGRA8Unorm);
        assert!(!layer.as_ref().framebufferOnly());
        assert_eq!(layer.as_ref().maximumDrawableCount(), 3);
    }
}
