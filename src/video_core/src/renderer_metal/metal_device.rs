// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal device and command-queue ownership.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLArgumentBuffersTier, MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice, MTLGPUFamily,
    MTLReadWriteTextureTier,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MetalDeviceError {
    #[error("macOS did not expose a default Metal device")]
    NoDevice,
    #[error("Metal device did not create a command queue")]
    NoCommandQueue,
}

/// Owns the native objects shared by all Metal backend services.
///
/// Both protocols are explicitly `Send + Sync` in Apple's API bindings. The
/// `CAMetalLayer` remains presentation-owned and is deliberately not stored
/// here because CoreAnimation has a different threading contract.
#[derive(Clone)]
pub struct MetalDevice {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    command_queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    profile: MetalDeviceProfile,
}

/// Runtime capability snapshot used by every Metal backend policy decision.
///
/// Apple product generations are deliberately absent: features are queried
/// from `MTLDevice` because similarly named chips and OS versions can expose
/// different Metal capabilities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetalDeviceProfile {
    pub architecture_name: String,
    pub registry_id: u64,
    pub highest_apple_family: Option<u8>,
    pub supports_mac2_family: bool,
    pub supports_metal3_family: bool,
    pub supports_metal4_family: bool,
    pub has_unified_memory: bool,
    pub recommended_max_working_set_size: u64,
    pub max_buffer_length: usize,
    pub max_threads_per_threadgroup: (usize, usize, usize),
    pub max_threadgroup_memory_length: usize,
    pub max_argument_buffer_sampler_count: usize,
    pub max_vertex_attributes: u32,
    pub max_buffer_bindings_per_stage: u32,
    pub max_texture_bindings_per_stage: u32,
    pub max_sampler_bindings_per_stage: u32,
    pub max_color_render_targets: u32,
    pub max_argument_buffer_samplers_per_stage: u32,
    pub argument_buffers_tier: MTLArgumentBuffersTier,
    pub read_write_texture_tier: MTLReadWriteTextureTier,
    pub supports_raster_order_groups: bool,
    pub supports_32bit_float_filtering: bool,
    pub supports_32bit_msaa: bool,
    pub supports_query_texture_lod: bool,
    pub supports_bc_texture_compression: bool,
    pub supports_astc_texture_compression: bool,
    pub supports_etc2_texture_compression: bool,
    pub supports_texture_swizzle: bool,
    pub supports_pull_model_interpolation: bool,
    pub supports_shader_barycentrics: bool,
    pub supports_dynamic_libraries: bool,
    pub supports_render_dynamic_libraries: bool,
    pub supports_function_pointers: bool,
    pub supports_render_function_pointers: bool,
    pub supports_raytracing: bool,
    pub supports_render_raytracing: bool,
    pub supports_depth24_stencil8: bool,
    sample_counts: [bool; 5],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetalArgumentBindingModel {
    Direct,
    Tier1ArgumentBuffers,
    Tier2ArgumentBuffers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MetalFamilyLimits {
    max_vertex_attributes: u32,
    max_buffer_bindings_per_stage: u32,
    max_texture_bindings_per_stage: u32,
    max_sampler_bindings_per_stage: u32,
    max_color_render_targets: u32,
    max_argument_buffer_samplers_per_stage: u32,
}

fn family_limits(highest_apple_family: Option<u8>) -> MetalFamilyLimits {
    MetalFamilyLimits {
        max_vertex_attributes: 31,
        max_buffer_bindings_per_stage: 31,
        max_texture_bindings_per_stage: if highest_apple_family.is_some_and(|family| family >= 6)
        {
            128
        } else if highest_apple_family.is_some_and(|family| family >= 4) {
            96
        } else {
            31
        },
        max_sampler_bindings_per_stage: 16,
        max_color_render_targets: 8,
        max_argument_buffer_samplers_per_stage: match highest_apple_family {
            Some(9..) => 500_000,
            Some(7..) => 996,
            Some(6) => 128,
            _ => 16,
        },
    }
}

impl MetalDeviceProfile {
    fn query(device: &ProtocolObject<dyn MTLDevice>) -> Self {
        let highest_apple_family = [
            (10, MTLGPUFamily::Apple10),
            (9, MTLGPUFamily::Apple9),
            (8, MTLGPUFamily::Apple8),
            (7, MTLGPUFamily::Apple7),
            (6, MTLGPUFamily::Apple6),
            (5, MTLGPUFamily::Apple5),
            (4, MTLGPUFamily::Apple4),
            (3, MTLGPUFamily::Apple3),
            (2, MTLGPUFamily::Apple2),
            (1, MTLGPUFamily::Apple1),
        ]
        .into_iter()
        .find_map(|(family, value)| device.supportsFamily(value).then_some(family));
        let max_threads = device.maxThreadsPerThreadgroup();
        let supports_mac2_family = device.supportsFamily(MTLGPUFamily::Mac2);
        let supports_metal3_family =
            objc2::available!(macos = 13.0, ..) && device.supportsFamily(MTLGPUFamily::Metal3);
        let supports_metal4_family =
            objc2::available!(macos = 26.0, ..) && device.supportsFamily(MTLGPUFamily::Metal4);
        // Apple publishes these implementation limits by GPU family rather
        // than through individual MTLDevice selectors. All Apple Silicon
        // devices are Apple7 or newer; direct binding limits remain stable,
        // while Tier 2 argument-buffer sampler capacity grows at Apple9.
        let family_limits = family_limits(highest_apple_family);
        Self {
            architecture_name: if objc2::available!(macos = 14.0, ..) {
                device.architecture().name().to_string()
            } else {
                device.name().to_string()
            },
            registry_id: device.registryID(),
            highest_apple_family,
            supports_mac2_family,
            supports_metal3_family,
            supports_metal4_family,
            has_unified_memory: device.hasUnifiedMemory(),
            recommended_max_working_set_size: device.recommendedMaxWorkingSetSize(),
            max_buffer_length: device.maxBufferLength(),
            max_threads_per_threadgroup: (max_threads.width, max_threads.height, max_threads.depth),
            max_threadgroup_memory_length: device.maxThreadgroupMemoryLength(),
            max_argument_buffer_sampler_count: device.maxArgumentBufferSamplerCount(),
            max_vertex_attributes: family_limits.max_vertex_attributes,
            max_buffer_bindings_per_stage: family_limits.max_buffer_bindings_per_stage,
            max_texture_bindings_per_stage: family_limits.max_texture_bindings_per_stage,
            max_sampler_bindings_per_stage: family_limits.max_sampler_bindings_per_stage,
            max_color_render_targets: family_limits.max_color_render_targets,
            max_argument_buffer_samplers_per_stage: family_limits
                .max_argument_buffer_samplers_per_stage,
            argument_buffers_tier: device.argumentBuffersSupport(),
            read_write_texture_tier: device.readWriteTextureSupport(),
            supports_raster_order_groups: device.areRasterOrderGroupsSupported(),
            supports_32bit_float_filtering: device.supports32BitFloatFiltering(),
            supports_32bit_msaa: device.supports32BitMSAA(),
            supports_query_texture_lod: device.supportsQueryTextureLOD(),
            supports_bc_texture_compression: device.supportsBCTextureCompression(),
            // Metal exposes no independent ASTC/ETC query. GPU-family feature
            // tables define both for Apple GPUs; family checks are cumulative.
            supports_astc_texture_compression: device.supportsFamily(MTLGPUFamily::Apple2),
            supports_etc2_texture_compression: device.supportsFamily(MTLGPUFamily::Apple1),
            supports_texture_swizzle: device.supportsFamily(MTLGPUFamily::Apple6)
                || supports_metal3_family,
            supports_pull_model_interpolation: device.supportsPullModelInterpolation(),
            supports_shader_barycentrics: device.supportsShaderBarycentricCoordinates(),
            supports_dynamic_libraries: objc2::available!(macos = 11.0, ..)
                && device.supportsDynamicLibraries(),
            supports_render_dynamic_libraries: objc2::available!(macos = 12.0, ..)
                && device.supportsRenderDynamicLibraries(),
            supports_function_pointers: objc2::available!(macos = 11.0, ..)
                && device.supportsFunctionPointers(),
            supports_render_function_pointers: objc2::available!(macos = 12.0, ..)
                && device.supportsFunctionPointersFromRender(),
            supports_raytracing: objc2::available!(macos = 11.0, ..) && device.supportsRaytracing(),
            supports_render_raytracing: objc2::available!(macos = 12.0, ..)
                && device.supportsRaytracingFromRender(),
            supports_depth24_stencil8: device.isDepth24Stencil8PixelFormatSupported(),
            sample_counts: [1, 2, 4, 8, 16]
                .map(|samples| device.supportsTextureSampleCount(samples)),
        }
    }

    pub fn supports_sample_count(&self, samples: u32) -> bool {
        [1, 2, 4, 8, 16]
            .into_iter()
            .position(|candidate| candidate == samples)
            .is_some_and(|index| self.sample_counts[index])
    }

    pub fn supports_argument_buffer_tier2(&self) -> bool {
        self.argument_buffers_tier == MTLArgumentBuffersTier::Tier2
    }

    pub fn supports_read_write_textures(&self) -> bool {
        self.read_write_texture_tier != MTLReadWriteTextureTier::TierNone
    }

    pub fn argument_binding_model(&self) -> MetalArgumentBindingModel {
        if self.max_argument_buffer_sampler_count == 0 {
            return MetalArgumentBindingModel::Direct;
        }
        if self.supports_argument_buffer_tier2() {
            MetalArgumentBindingModel::Tier2ArgumentBuffers
        } else {
            MetalArgumentBindingModel::Tier1ArgumentBuffers
        }
    }

    pub fn best_supported_sample_count(&self, requested: u32) -> u32 {
        [16, 8, 4, 2, 1]
            .into_iter()
            .find(|samples| *samples <= requested && self.supports_sample_count(*samples))
            .unwrap_or(1)
    }

    /// Keep allocations below Metal's good-performance working-set estimate.
    /// The cache still performs resource-specific accounting and eviction.
    pub fn recommended_resource_budget(&self) -> u64 {
        self.recommended_max_working_set_size.saturating_mul(4) / 5
    }
}

impl MetalDevice {
    pub fn new() -> Result<Self, MetalDeviceError> {
        let device = MTLCreateSystemDefaultDevice().ok_or(MetalDeviceError::NoDevice)?;
        let profile = MetalDeviceProfile::query(&device);
        let command_queue = device
            .newCommandQueue()
            .ok_or(MetalDeviceError::NoCommandQueue)?;
        Ok(Self {
            device,
            command_queue,
            profile,
        })
    }

    pub fn device(&self) -> &ProtocolObject<dyn MTLDevice> {
        &self.device
    }

    pub fn command_queue(&self) -> &ProtocolObject<dyn MTLCommandQueue> {
        &self.command_queue
    }

    pub fn profile(&self) -> &MetalDeviceProfile {
        &self.profile
    }

    pub(crate) fn retained_command_queue(&self) -> Retained<ProtocolObject<dyn MTLCommandQueue>> {
        self.command_queue.clone()
    }

    pub fn name(&self) -> String {
        self.device.name().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native_profile() -> MetalDeviceProfile {
        MetalDevice::new()
            .expect("Metal device must exist on macOS test hosts")
            .profile
    }

    #[test]
    fn creates_native_device_and_command_queue() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        assert!(!device.name().is_empty());
        assert!(!Retained::as_ptr(&device.command_queue).is_null());
        assert!(device.profile().supports_sample_count(1));
        assert!(device.profile().highest_apple_family.is_some());
    }

    #[test]
    fn policy_falls_back_on_a_limited_profile() {
        let mut profile = native_profile();
        profile.argument_buffers_tier = MTLArgumentBuffersTier::Tier1;
        profile.max_argument_buffer_sampler_count = 16;
        profile.read_write_texture_tier = MTLReadWriteTextureTier::TierNone;
        profile.sample_counts = [true, true, true, false, false];
        profile.recommended_max_working_set_size = 1_000;
        profile.highest_apple_family = Some(7);
        profile.max_argument_buffer_samplers_per_stage = 996;

        assert_eq!(
            profile.argument_binding_model(),
            MetalArgumentBindingModel::Tier1ArgumentBuffers
        );
        assert!(!profile.supports_read_write_textures());
        assert_eq!(profile.best_supported_sample_count(16), 4);
        assert_eq!(profile.recommended_resource_budget(), 800);
        assert_eq!(profile.max_argument_buffer_samplers_per_stage, 996);
    }

    #[test]
    fn policy_uses_modern_features_only_when_reported() {
        let mut profile = native_profile();
        profile.argument_buffers_tier = MTLArgumentBuffersTier::Tier2;
        profile.max_argument_buffer_sampler_count = 96;
        profile.read_write_texture_tier = MTLReadWriteTextureTier::Tier2;
        profile.sample_counts = [true, true, true, true, true];
        profile.highest_apple_family = Some(10);
        profile.max_argument_buffer_samplers_per_stage = 500_000;

        assert_eq!(
            profile.argument_binding_model(),
            MetalArgumentBindingModel::Tier2ArgumentBuffers
        );
        assert!(profile.supports_read_write_textures());
        assert_eq!(profile.best_supported_sample_count(16), 16);
        assert_eq!(profile.max_argument_buffer_samplers_per_stage, 500_000);
    }

    #[test]
    fn family_limits_distinguish_m1_and_m5_without_marketing_names() {
        let apple7 = family_limits(Some(7));
        let apple10 = family_limits(Some(10));

        assert_eq!(apple7.max_buffer_bindings_per_stage, 31);
        assert_eq!(apple7.max_texture_bindings_per_stage, 128);
        assert_eq!(apple7.max_sampler_bindings_per_stage, 16);
        assert_eq!(apple7.max_argument_buffer_samplers_per_stage, 996);
        assert_eq!(apple10.max_argument_buffer_samplers_per_stage, 500_000);
    }
}
