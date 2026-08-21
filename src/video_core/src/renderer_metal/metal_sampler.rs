// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal sampler ownership.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLCompareFunction, MTLDevice, MTLSamplerAddressMode, MTLSamplerBorderColor,
    MTLSamplerDescriptor, MTLSamplerMinMagFilter, MTLSamplerMipFilter, MTLSamplerReductionMode,
    MTLSamplerState,
};
use thiserror::Error;

use crate::textures::texture::{
    DepthCompareFunc, SamplerReduction, TextureFilter, TextureMipmapFilter, TscEntry, WrapMode,
};

use super::metal_device::MetalDevice;

#[derive(Debug, Error)]
pub enum MetalSamplerError {
    #[error("Metal failed to create a sampler state")]
    CreationFailed,
}

/// Backend-owned counterpart of Eden's `Vulkan::Sampler`.
///
/// The four native states preserve the upstream selection contract. Metal
/// only exposes three fixed border colors; arbitrary Maxwell border colors
/// and mirror-once-to-border are retained as metadata for shader emulation.
pub struct MetalSampler {
    sampler: Retained<ProtocolObject<dyn MTLSamplerState>>,
    sampler_default_anisotropy: Option<Retained<ProtocolObject<dyn MTLSamplerState>>>,
    sampler_nearest: Option<Retained<ProtocolObject<dyn MTLSamplerState>>>,
    sampler_noncompare: Option<Retained<ProtocolObject<dyn MTLSamplerState>>>,
    border_color: [f32; 4],
    requires_border_emulation: bool,
}

impl MetalSampler {
    pub fn new(device: &MetalDevice, tsc: &TscEntry) -> Result<Self, MetalSamplerError> {
        let mag_filter = texture_filter(tsc.mag_filter());
        let min_filter = texture_filter(tsc.min_filter());
        let mipmap_filter = texture_mipmap_filter(tsc.mipmap_filter());
        let reduction = sampler_reduction(tsc.reduction_filter());
        let maxwell_border_color = tsc.computed_border_color();
        let (native_border_color, custom_border) = border_color(maxwell_border_color);
        let requires_border_emulation = custom_border
            || [tsc.wrap_u(), tsc.wrap_v(), tsc.wrap_p()]
                .into_iter()
                .any(|wrap| wrap == WrapMode::MirrorOnceBorder as u32);
        let max_anisotropy = tsc.computed_max_anisotropy().clamp(1.0, 16.0);

        let create_sampler = |anisotropy: f32,
                              force_nearest: bool,
                              disable_compare: bool|
         -> Result<
            Retained<ProtocolObject<dyn MTLSamplerState>>,
            MetalSamplerError,
        > {
            let descriptor = MTLSamplerDescriptor::new();
            descriptor.setMagFilter(if force_nearest {
                MTLSamplerMinMagFilter::Nearest
            } else {
                mag_filter
            });
            descriptor.setMinFilter(if force_nearest {
                MTLSamplerMinMagFilter::Nearest
            } else {
                min_filter
            });
            descriptor.setMipFilter(if force_nearest {
                MTLSamplerMipFilter::Nearest
            } else {
                mipmap_filter
            });
            descriptor.setSAddressMode(wrap_mode(tsc.wrap_u(), tsc.mag_filter()));
            descriptor.setTAddressMode(wrap_mode(tsc.wrap_v(), tsc.mag_filter()));
            descriptor.setRAddressMode(wrap_mode(tsc.wrap_p(), tsc.mag_filter()));
            descriptor.setBorderColor(native_border_color);
            descriptor.setReductionMode(reduction);
            descriptor.setLodBias(tsc.lod_bias());
            descriptor.setMaxAnisotropy(if force_nearest {
                1
            } else {
                anisotropy.round() as usize
            });
            descriptor.setCompareFunction(if disable_compare || tsc.depth_compare_enabled() == 0 {
                MTLCompareFunction::Never
            } else {
                depth_compare_function(tsc.depth_compare_func())
            });
            descriptor.setLodMinClamp(if tsc.mipmap_filter() == TextureMipmapFilter::None as u32 {
                0.0
            } else {
                tsc.min_lod()
            });
            descriptor.setLodMaxClamp(if tsc.mipmap_filter() == TextureMipmapFilter::None as u32 {
                0.25
            } else {
                tsc.max_lod()
            });
            descriptor.setNormalizedCoordinates(true);
            device
                .device()
                .newSamplerStateWithDescriptor(&descriptor)
                .ok_or(MetalSamplerError::CreationFailed)
        };

        let sampler = create_sampler(max_anisotropy, false, false)?;
        let max_anisotropy_default = (1u32 << tsc.max_anisotropy_raw()) as f32;
        let sampler_default_anisotropy = (max_anisotropy > max_anisotropy_default)
            .then(|| create_sampler(max_anisotropy_default, false, false))
            .transpose()?;
        let has_linear_filtering = mag_filter == MTLSamplerMinMagFilter::Linear
            || min_filter == MTLSamplerMinMagFilter::Linear
            || mipmap_filter == MTLSamplerMipFilter::Linear;
        let sampler_nearest = has_linear_filtering
            .then(|| create_sampler(1.0, true, false))
            .transpose()?;
        let sampler_noncompare = (tsc.depth_compare_enabled() != 0)
            .then(|| create_sampler(max_anisotropy, false, true))
            .transpose()?;

        Ok(Self {
            sampler,
            sampler_default_anisotropy,
            sampler_nearest,
            sampler_noncompare,
            border_color: maxwell_border_color,
            requires_border_emulation,
        })
    }

    pub fn handle(&self) -> &ProtocolObject<dyn MTLSamplerState> {
        &self.sampler
    }

    pub(crate) fn retained_handle(&self) -> Retained<ProtocolObject<dyn MTLSamplerState>> {
        self.sampler.clone()
    }

    pub fn handle_with_default_anisotropy(&self) -> &ProtocolObject<dyn MTLSamplerState> {
        self.sampler_default_anisotropy
            .as_deref()
            .unwrap_or(&self.sampler)
    }

    pub(crate) fn retained_handle_with_default_anisotropy(
        &self,
    ) -> Retained<ProtocolObject<dyn MTLSamplerState>> {
        self.sampler_default_anisotropy
            .clone()
            .unwrap_or_else(|| self.sampler.clone())
    }

    pub fn has_added_anisotropy(&self) -> bool {
        self.sampler_default_anisotropy.is_some()
    }

    pub fn handle_with_nearest_filter(&self) -> &ProtocolObject<dyn MTLSamplerState> {
        self.sampler_nearest.as_deref().unwrap_or(&self.sampler)
    }

    pub(crate) fn retained_handle_with_nearest_filter(
        &self,
    ) -> Retained<ProtocolObject<dyn MTLSamplerState>> {
        self.sampler_nearest
            .clone()
            .unwrap_or_else(|| self.sampler.clone())
    }

    pub fn has_linear_filtering(&self) -> bool {
        self.sampler_nearest.is_some()
    }

    pub fn handle_without_depth_comparison(&self) -> &ProtocolObject<dyn MTLSamplerState> {
        self.sampler_noncompare.as_deref().unwrap_or(&self.sampler)
    }

    pub(crate) fn retained_handle_without_depth_comparison(
        &self,
    ) -> Retained<ProtocolObject<dyn MTLSamplerState>> {
        self.sampler_noncompare
            .clone()
            .unwrap_or_else(|| self.sampler.clone())
    }

    pub fn has_depth_comparison(&self) -> bool {
        self.sampler_noncompare.is_some()
    }

    pub fn border_color(&self) -> [f32; 4] {
        self.border_color
    }

    pub fn requires_border_emulation(&self) -> bool {
        self.requires_border_emulation
    }
}

fn texture_filter(raw: u32) -> MTLSamplerMinMagFilter {
    if raw == TextureFilter::Linear as u32 {
        MTLSamplerMinMagFilter::Linear
    } else {
        MTLSamplerMinMagFilter::Nearest
    }
}

fn texture_mipmap_filter(raw: u32) -> MTLSamplerMipFilter {
    match raw {
        value if value == TextureMipmapFilter::None as u32 => MTLSamplerMipFilter::Nearest,
        value if value == TextureMipmapFilter::Linear as u32 => MTLSamplerMipFilter::Linear,
        _ => MTLSamplerMipFilter::Nearest,
    }
}

fn wrap_mode(raw: u32, filter: u32) -> MTLSamplerAddressMode {
    match raw {
        value if value == WrapMode::Wrap as u32 => MTLSamplerAddressMode::Repeat,
        value if value == WrapMode::Mirror as u32 => MTLSamplerAddressMode::MirrorRepeat,
        value if value == WrapMode::ClampToEdge as u32 => MTLSamplerAddressMode::ClampToEdge,
        value if value == WrapMode::Border as u32 => MTLSamplerAddressMode::ClampToBorderColor,
        value if value == WrapMode::Clamp as u32 => {
            if filter == TextureFilter::Linear as u32 {
                MTLSamplerAddressMode::ClampToBorderColor
            } else {
                MTLSamplerAddressMode::ClampToEdge
            }
        }
        value
            if matches!(
                value,
                x if x == WrapMode::MirrorOnceClampToEdge as u32
                    || x == WrapMode::MirrorOnceBorder as u32
                    || x == WrapMode::MirrorOnceClampOgl as u32
            ) =>
        {
            MTLSamplerAddressMode::MirrorClampToEdge
        }
        _ => MTLSamplerAddressMode::ClampToEdge,
    }
}

fn depth_compare_function(raw: u32) -> MTLCompareFunction {
    match DepthCompareFunc::from_raw(raw) {
        DepthCompareFunc::Never => MTLCompareFunction::Never,
        DepthCompareFunc::Less => MTLCompareFunction::Less,
        DepthCompareFunc::Equal => MTLCompareFunction::Equal,
        DepthCompareFunc::LessEqual => MTLCompareFunction::LessEqual,
        DepthCompareFunc::Greater => MTLCompareFunction::Greater,
        DepthCompareFunc::NotEqual => MTLCompareFunction::NotEqual,
        DepthCompareFunc::GreaterEqual => MTLCompareFunction::GreaterEqual,
        DepthCompareFunc::Always => MTLCompareFunction::Always,
    }
}

fn sampler_reduction(raw: u32) -> MTLSamplerReductionMode {
    match raw {
        value if value == SamplerReduction::Min as u32 => MTLSamplerReductionMode::Minimum,
        value if value == SamplerReduction::Max as u32 => MTLSamplerReductionMode::Maximum,
        _ => MTLSamplerReductionMode::WeightedAverage,
    }
}

fn border_color(color: [f32; 4]) -> (MTLSamplerBorderColor, bool) {
    if color == [0.0, 0.0, 0.0, 0.0] {
        (MTLSamplerBorderColor::TransparentBlack, false)
    } else if color == [0.0, 0.0, 0.0, 1.0] {
        (MTLSamplerBorderColor::OpaqueBlack, false)
    } else if color == [1.0, 1.0, 1.0, 1.0] {
        (MTLSamplerBorderColor::OpaqueWhite, false)
    } else {
        // Preserve Eden's fallback while retaining the exact color above for
        // the shader-emulated custom-border path.
        let fallback = if color[0] + color[1] + color[2] > 1.35 {
            MTLSamplerBorderColor::OpaqueWhite
        } else if color[3] > 0.5 {
            MTLSamplerBorderColor::OpaqueBlack
        } else {
            MTLSamplerBorderColor::TransparentBlack
        };
        (fallback, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predefined_and_custom_border_colors_are_distinguished() {
        assert_eq!(
            border_color([0.0, 0.0, 0.0, 0.0]),
            (MTLSamplerBorderColor::TransparentBlack, false)
        );
        assert_eq!(
            border_color([0.2, 0.3, 0.4, 0.5]),
            (MTLSamplerBorderColor::TransparentBlack, true)
        );
    }

    #[test]
    fn creates_all_upstream_sampler_variants() {
        let device = MetalDevice::new().unwrap();
        let mut tsc = TscEntry { raw: [0; 4] };
        let word0 = (WrapMode::Wrap as u32)
            | ((WrapMode::Wrap as u32) << 3)
            | ((WrapMode::Wrap as u32) << 6)
            | (1 << 9)
            | ((DepthCompareFunc::LessEqual as u32) << 10)
            | (2 << 20);
        let word1 = (TextureFilter::Linear as u32)
            | ((TextureFilter::Linear as u32) << 4)
            | ((TextureMipmapFilter::Linear as u32) << 6);
        tsc.raw[0] = word0 as u64 | ((word1 as u64) << 32);
        tsc.raw[1] = (256u64 << 12) | 0;
        let sampler = MetalSampler::new(&device, &tsc).unwrap();
        assert!(sampler.has_linear_filtering());
        assert!(sampler.has_depth_comparison());
    }
}
