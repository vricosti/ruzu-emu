// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `shader_recompiler/host_translate_info.h`
//!
//! Miscellaneous information about the host GPU that affects shader translation.

/// Misc information about the host GPU/driver.
///
/// Try to keep entries here to a minimum -- they can accidentally change
/// the cached information in a shader.
#[derive(Debug, Clone, Default)]
pub struct HostTranslateInfo {
    /// Minimum alignment supported by the device for SSBOs.
    pub min_ssbo_alignment: u64,
    /// Maximum sampled-image descriptors available to one shader stage.
    pub max_per_stage_descriptor_sampled_images: u32,
    /// Maximum aggregate resources available to one shader stage.
    pub max_per_stage_resources: u32,
    pub max_descriptor_set_samplers: u32,
    pub max_descriptor_set_uniform_buffers: u32,
    pub max_descriptor_set_uniform_buffers_dynamic: u32,
    pub max_descriptor_set_storage_buffers: u32,
    pub max_descriptor_set_storage_buffers_dynamic: u32,
    pub max_descriptor_set_sampled_images: u32,
    pub max_descriptor_set_storage_images: u32,
    pub max_descriptor_set_input_attachements: u32,
    /// True when the device supports 64-bit floats.
    pub support_float64: bool,
    /// True when the device supports 16-bit floats.
    pub support_float16: bool,
    /// True when the device supports 64-bit integers.
    pub support_int64: bool,
    /// True when the device needs DemoteToHelperInvocation reordered.
    pub needs_demote_reorder: bool,
    /// True when the device supports SNORM render buffers.
    pub support_snorm_render_buffer: bool,
    /// True when the device supports gl_Layer in VS.
    pub support_viewport_index_layer: bool,
    /// True when the device supports geometry passthrough shaders.
    pub support_geometry_shader_passthrough: bool,
    /// True when the device supports barriers in conditional control flow.
    pub support_conditional_barrier: bool,
    /// True when a handle read from a table at a per-invocation index may
    /// become a descriptor array of the table's entries (not OpenGL: its
    /// pipelines bind at most 64 textures; Vulkan only with nonuniform sampled
    /// image array indexing).
    ///
    /// Lichii extension: `shader_recompiler/host_translate_info.h:51`.
    pub support_handle_table_arrays: bool,
}

impl HostTranslateInfo {
    /// Upstream fallback used when a backend reports a zero descriptor limit.
    pub const DEFAULT_DESCRIPTOR_LIMIT: u32 = 1024;

    /// Apply Eden's descriptor-limit fallback policy before shader translation.
    pub fn apply_descriptor_limit_policy(&mut self) {
        if self.min_ssbo_alignment == 0 {
            self.min_ssbo_alignment = 1;
        }
        Self::apply_descriptor_limit_fallback(&mut self.max_per_stage_descriptor_sampled_images);
        Self::apply_descriptor_limit_fallback(&mut self.max_per_stage_resources);
        Self::apply_descriptor_limit_fallback(&mut self.max_descriptor_set_samplers);
        Self::apply_descriptor_limit_fallback(&mut self.max_descriptor_set_uniform_buffers);
        Self::apply_descriptor_limit_fallback(&mut self.max_descriptor_set_uniform_buffers_dynamic);
        Self::apply_descriptor_limit_fallback(&mut self.max_descriptor_set_storage_buffers);
        Self::apply_descriptor_limit_fallback(&mut self.max_descriptor_set_storage_buffers_dynamic);
        Self::apply_descriptor_limit_fallback(&mut self.max_descriptor_set_sampled_images);
        Self::apply_descriptor_limit_fallback(&mut self.max_descriptor_set_storage_images);
        Self::apply_descriptor_limit_fallback(&mut self.max_descriptor_set_input_attachements);
    }

    fn apply_descriptor_limit_fallback(limit: &mut u32) {
        if *limit == 0 {
            *limit = Self::DEFAULT_DESCRIPTOR_LIMIT;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HostTranslateInfo;

    #[test]
    fn descriptor_limit_policy_matches_upstream_fallbacks() {
        let mut info = HostTranslateInfo::default();

        info.apply_descriptor_limit_policy();

        assert_eq!(info.min_ssbo_alignment, 1);
        assert_eq!(
            info.max_per_stage_descriptor_sampled_images,
            HostTranslateInfo::DEFAULT_DESCRIPTOR_LIMIT
        );
        assert_eq!(
            info.max_per_stage_resources,
            HostTranslateInfo::DEFAULT_DESCRIPTOR_LIMIT
        );
        assert_eq!(
            info.max_descriptor_set_input_attachements,
            HostTranslateInfo::DEFAULT_DESCRIPTOR_LIMIT
        );
    }

    #[test]
    fn descriptor_limit_policy_preserves_nonzero_values() {
        let mut info = HostTranslateInfo {
            min_ssbo_alignment: 256,
            max_per_stage_descriptor_sampled_images: 128,
            max_per_stage_resources: 64,
            max_descriptor_set_samplers: 32,
            max_descriptor_set_uniform_buffers: 31,
            max_descriptor_set_uniform_buffers_dynamic: 30,
            max_descriptor_set_storage_buffers: 29,
            max_descriptor_set_storage_buffers_dynamic: 28,
            max_descriptor_set_sampled_images: 27,
            max_descriptor_set_storage_images: 26,
            max_descriptor_set_input_attachements: 25,
            ..HostTranslateInfo::default()
        };

        info.apply_descriptor_limit_policy();

        assert_eq!(info.min_ssbo_alignment, 256);
        assert_eq!(info.max_per_stage_descriptor_sampled_images, 128);
        assert_eq!(info.max_per_stage_resources, 64);
        assert_eq!(info.max_descriptor_set_input_attachements, 25);
    }
}
