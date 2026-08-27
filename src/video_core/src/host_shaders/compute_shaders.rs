// SPDX-FileCopyrightText: Copyright 2026 Ruzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Compute-shader sources corresponding to Eden's `video_core/host_shaders/*.comp` files.
//!
//! OpenGL compiles these strings at runtime. Vulkan compiles the same source files through
//! `build.rs`, so keeping the `.comp` files as the single source of truth prevents backend drift.

pub const ASTC_DECODER_COMP: &str = include_str!("astc_decoder.comp");
pub const BLOCK_LINEAR_UNSWIZZLE_2D_COMP: &str = include_str!("block_linear_unswizzle_2d.comp");
pub const BLOCK_LINEAR_UNSWIZZLE_3D_COMP: &str = include_str!("block_linear_unswizzle_3d.comp");
pub const CONVERT_MSAA_TO_NON_MSAA_COMP: &str = include_str!("convert_msaa_to_non_msaa.comp");
pub const CONVERT_NON_MSAA_TO_MSAA_COMP: &str = include_str!("convert_non_msaa_to_msaa.comp");
pub const OPENGL_CONVERT_S8D24_COMP: &str = include_str!("opengl_convert_s8d24.comp");
pub const OPENGL_COPY_BC4_COMP: &str = include_str!("opengl_copy_bc4.comp");
pub const OPENGL_LMEM_WARMUP_COMP: &str = include_str!("opengl_lmem_warmup.comp");
pub const PITCH_UNSWIZZLE_COMP: &str = include_str!("pitch_unswizzle.comp");
pub const QUERIES_PREFIX_SCAN_SUM_COMP: &str = include_str!("queries_prefix_scan_sum.comp");
pub const QUERIES_PREFIX_SCAN_SUM_NOSUBGROUPS_COMP: &str =
    include_str!("queries_prefix_scan_sum_nosubgroups.comp");
pub const RESOLVE_CONDITIONAL_RENDER_COMP: &str = include_str!("resolve_conditional_render.comp");
pub const VULKAN_QUAD_INDEXED_COMP: &str = include_str!("vulkan_quad_indexed.comp");
pub const VULKAN_TURBO_MODE_COMP: &str = include_str!("vulkan_turbo_mode.comp");
pub const VULKAN_UINT8_COMP: &str = include_str!("vulkan_uint8.comp");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn astc_decoder_writes_color_values_at_the_post_increment_index() {
        assert!(ASTC_DECODER_COMP
            .contains("color_values[out_index++] = FastReplicateTo8(bitval, bitlen);"));
        assert!(ASTC_DECODER_COMP.contains("color_values[out_index++] = T;"));
        assert!(!ASTC_DECODER_COMP.contains("color_values[++out_index]"));
    }

    #[test]
    fn subgroup_query_scan_accumulates_the_base_value() {
        assert!(
            QUERIES_PREFIX_SCAN_SUM_COMP.contains("results[i] = AddUint64(results[i], base_data);")
        );
    }

    #[test]
    fn conditional_render_supports_both_comparison_modes() {
        assert!(RESOLVE_CONDITIONAL_RENDER_COMP.contains("uint compare_to_zero;"));
        assert!(RESOLVE_CONDITIONAL_RENDER_COMP.contains("if (compare_to_zero != 0u)"));
        assert!(RESOLVE_CONDITIONAL_RENDER_COMP
            .contains("result = (data[0] != 0u && data[1] != 0u) ? 1u : 0u;"));
        assert!(RESOLVE_CONDITIONAL_RENDER_COMP
            .contains("result = (data[0] == data[4] && data[1] == data[5]) ? 1u : 0u;"));
    }
}
