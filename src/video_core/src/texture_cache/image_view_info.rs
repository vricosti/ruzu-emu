// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/texture_cache/image_view_info.h and image_view_info.cpp
//!
//! `ImageViewInfo` describes the properties used to look up or create an image
//! view (sub-resource of a texture).

use super::format_lookup_table::PixelFormat;
use super::types::*;
use super::util::pixel_format_from_tic;
pub use crate::textures::texture::SwizzleSource;
use crate::textures::texture::{TextureType, TicEntry};

fn fail_soft(message: String) {
    log::error!("{message}");
    if *common::settings::values().use_debug_asserts.get_value() {
        panic!("{message}");
    }
}

/// Sentinel value used by render-target views (no real swizzle).
const RENDER_TARGET_SWIZZLE: u8 = u8::MAX;

// ── ImageViewInfo ──────────────────────────────────────────────────────

/// Properties used to determine an image view.
///
/// Port of `VideoCommon::ImageViewInfo`.
///
/// The struct has a trivial object representation in C++ (checked via
/// `static_assert(std::has_unique_object_representations_v<ImageViewInfo>)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct ImageViewInfo {
    pub view_type: ImageViewType,
    pub format: PixelFormat,
    pub range: SubresourceRange,
    pub x_source: u8,
    pub y_source: u8,
    pub z_source: u8,
    pub w_source: u8,
}

impl Default for ImageViewInfo {
    fn default() -> Self {
        Self {
            view_type: ImageViewType::E1D,
            format: PixelFormat::Invalid,
            range: SubresourceRange::default(),
            x_source: SwizzleSource::R as u8,
            y_source: SwizzleSource::G as u8,
            z_source: SwizzleSource::B as u8,
            w_source: SwizzleSource::A as u8,
        }
    }
}

impl ImageViewInfo {
    /// Construct from a TIC entry and base layer.
    ///
    /// Port of `ImageViewInfo::ImageViewInfo(const TICEntry&, s32 base_layer)`.
    ///
    pub fn from_tic_entry(config: &TicEntry, base_layer: i32) -> Self {
        let mut info = Self {
            format: pixel_format_from_tic(config),
            x_source: cast_swizzle(config.x_source()),
            y_source: cast_swizzle(config.y_source()),
            z_source: cast_swizzle(config.z_source()),
            w_source: cast_swizzle(config.w_source()),
            range: SubresourceRange {
                base: SubresourceBase {
                    level: config.res_min_mip_level() as i32,
                    layer: base_layer,
                },
                extent: SubresourceExtent {
                    levels: config
                        .res_max_mip_level()
                        .wrapping_sub(config.res_min_mip_level())
                        .wrapping_add(1) as i32,
                    layers: 1,
                },
            },
            ..Self::default()
        };

        let Some(mut texture_type) = TextureType::from_raw(config.texture_type()) else {
            fail_soft(format!("Invalid texture_type={}", config.texture_type()));
            return info;
        };
        if config.depth() > 1 || base_layer != 0 {
            texture_type = match texture_type {
                TextureType::Texture1D => TextureType::Texture1DArray,
                TextureType::Texture2D => TextureType::Texture2DArray,
                TextureType::TextureCubemap => TextureType::TextureCubeArray,
                other => other,
            };
        }

        match texture_type {
            TextureType::Texture1D => {
                if config.height() != 1 {
                    fail_soft(format!(
                        "Texture1D height is {} instead of 1",
                        config.height()
                    ));
                }
                if config.depth() != 1 {
                    fail_soft(format!(
                        "Texture1D depth is {} instead of 1",
                        config.depth()
                    ));
                }
                info.view_type = ImageViewType::E1D;
            }
            TextureType::Texture2D | TextureType::Texture2DNoMipmap => {
                if config.depth() != 1 {
                    fail_soft(format!(
                        "Texture2D depth is {} instead of 1",
                        config.depth()
                    ));
                }
                info.view_type = if config.normalized_coords() != 0 {
                    ImageViewType::E2D
                } else {
                    ImageViewType::Rect
                };
            }
            TextureType::Texture3D => {
                info.view_type = ImageViewType::E3D;
            }
            TextureType::TextureCubemap => {
                if config.depth() != 1 {
                    fail_soft(format!(
                        "TextureCubemap depth is {} instead of 1",
                        config.depth()
                    ));
                }
                info.view_type = ImageViewType::Cube;
                info.range.extent.layers = 6;
            }
            TextureType::Texture1DArray => {
                if config.height() != 1 {
                    fail_soft(format!(
                        "Texture1DArray height is {} instead of 1",
                        config.height()
                    ));
                }
                info.view_type = ImageViewType::E1DArray;
                info.range.extent.layers = config.depth() as i32;
            }
            TextureType::Texture2DArray => {
                info.view_type = ImageViewType::E2DArray;
                info.range.extent.layers = config.depth() as i32;
            }
            TextureType::Texture1DBuffer => {
                info.view_type = ImageViewType::Buffer;
            }
            TextureType::TextureCubeArray => {
                info.view_type = ImageViewType::CubeArray;
                info.range.extent.layers = config.depth().wrapping_mul(6) as i32;
            }
        }

        info
    }

    /// Construct for a render target (no real swizzle).
    ///
    /// Port of `ImageViewInfo::ImageViewInfo(ImageViewType, PixelFormat, SubresourceRange)`.
    pub fn for_render_target(
        view_type: ImageViewType,
        format: PixelFormat,
        range: SubresourceRange,
    ) -> Self {
        Self {
            view_type,
            format,
            range,
            x_source: RENDER_TARGET_SWIZZLE,
            y_source: RENDER_TARGET_SWIZZLE,
            z_source: RENDER_TARGET_SWIZZLE,
            w_source: RENDER_TARGET_SWIZZLE,
        }
    }

    /// Returns the swizzle sources as an array.
    pub fn swizzle(&self) -> [SwizzleSource; 4] {
        let decode = |source| {
            SwizzleSource::from_raw(source as u32).unwrap_or_else(|| {
                fail_soft(format!("Invalid swizzle source={source}"));
                SwizzleSource::Invalid
            })
        };
        [
            decode(self.x_source),
            decode(self.y_source),
            decode(self.z_source),
            decode(self.w_source),
        ]
    }

    /// Whether this view was created as a render target (all swizzle fields
    /// set to the sentinel value).
    ///
    /// Port of `ImageViewInfo::IsRenderTarget`.
    pub fn is_render_target(&self) -> bool {
        self.x_source == RENDER_TARGET_SWIZZLE
            && self.y_source == RENDER_TARGET_SWIZZLE
            && self.z_source == RENDER_TARGET_SWIZZLE
            && self.w_source == RENDER_TARGET_SWIZZLE
    }
}

fn cast_swizzle(source: u32) -> u8 {
    let casted = source as u8;
    if casted as u32 != source {
        fail_soft(format!("Swizzle source {source} does not fit in u8"));
    }
    casted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture_cache::format_lookup_table::PixelFormat;
    use crate::textures::texture::{ComponentType, TextureFormat};

    fn tic_entry(
        texture_type: TextureType,
        normalized_coords: bool,
        depth: u32,
        min_mip: u32,
        max_mip: u32,
    ) -> TicEntry {
        let word0 = (TextureFormat::A8B8G8R8 as u32)
            | ((ComponentType::Unorm as u32) << 7)
            | ((ComponentType::Unorm as u32) << 10)
            | ((ComponentType::Unorm as u32) << 13)
            | ((ComponentType::Unorm as u32) << 16)
            | ((SwizzleSource::R as u32) << 19)
            | ((SwizzleSource::G as u32) << 22)
            | ((SwizzleSource::B as u32) << 25)
            | ((SwizzleSource::A as u32) << 28);
        let word4 = 63 | ((texture_type as u32) << 23);
        let word5 = 31 | ((depth - 1) << 16) | ((normalized_coords as u32) << 31);
        let word7 = min_mip | (max_mip << 4);

        TicEntry {
            raw: [
                word0 as u64,
                0,
                ((word5 as u64) << 32) | word4 as u64,
                (word7 as u64) << 32,
            ],
        }
    }

    #[test]
    fn image_view_info_layout_matches_upstream_unique_representation() {
        assert_eq!(std::mem::size_of::<ImageViewInfo>(), 28);
        assert_eq!(std::mem::align_of::<ImageViewInfo>(), 4);
        assert_eq!(std::mem::offset_of!(ImageViewInfo, view_type), 0);
        assert_eq!(std::mem::offset_of!(ImageViewInfo, format), 4);
        assert_eq!(std::mem::offset_of!(ImageViewInfo, range), 8);
        assert_eq!(std::mem::offset_of!(ImageViewInfo, x_source), 24);
        assert_eq!(std::mem::offset_of!(ImageViewInfo, w_source), 27);
    }

    #[test]
    fn tic_2d_normalized_maps_to_2d_view() {
        let tic = tic_entry(TextureType::Texture2D, true, 1, 2, 4);
        let info = ImageViewInfo::from_tic_entry(&tic, 0);

        assert_eq!(info.view_type, ImageViewType::E2D);
        assert_eq!(info.format, PixelFormat::A8B8G8R8Unorm);
        assert_eq!(info.range.base.level, 2);
        assert_eq!(info.range.base.layer, 0);
        assert_eq!(info.range.extent.levels, 3);
        assert_eq!(info.range.extent.layers, 1);
        assert_eq!(
            [info.x_source, info.y_source, info.z_source, info.w_source],
            [
                SwizzleSource::R as u8,
                SwizzleSource::G as u8,
                SwizzleSource::B as u8,
                SwizzleSource::A as u8,
            ]
        );
    }

    #[test]
    fn layered_2d_tic_maps_to_2d_array_view() {
        let tic = tic_entry(TextureType::Texture2D, true, 6, 0, 0);
        let info = ImageViewInfo::from_tic_entry(&tic, 0);

        assert_eq!(info.view_type, ImageViewType::E2DArray);
        assert_eq!(info.range.extent.layers, 6);
    }

    #[test]
    fn nonzero_base_layer_promotes_2d_tic_to_array_view() {
        let tic = tic_entry(TextureType::Texture2D, true, 1, 0, 0);
        let info = ImageViewInfo::from_tic_entry(&tic, 3);

        assert_eq!(info.view_type, ImageViewType::E2DArray);
        assert_eq!(info.range.base.layer, 3);
        assert_eq!(info.range.extent.layers, 1);
    }

    #[test]
    fn tic_rect_and_array_layer_counts_match_upstream() {
        let rect = tic_entry(TextureType::Texture2D, false, 1, 0, 0);
        assert_eq!(
            ImageViewInfo::from_tic_entry(&rect, 0).view_type,
            ImageViewType::Rect
        );

        let array = tic_entry(TextureType::Texture2DArray, true, 4, 0, 0);
        let array_info = ImageViewInfo::from_tic_entry(&array, 3);
        assert_eq!(array_info.view_type, ImageViewType::E2DArray);
        assert_eq!(array_info.range.extent.layers, 4);

        let cube_array = tic_entry(TextureType::TextureCubeArray, true, 2, 0, 0);
        let cube_array_info = ImageViewInfo::from_tic_entry(&cube_array, 0);
        assert_eq!(cube_array_info.view_type, ImageViewType::CubeArray);
        assert_eq!(cube_array_info.range.extent.layers, 12);
    }

    #[test]
    fn tic_assertions_are_fail_soft_and_mip_count_wraps_like_upstream() {
        let one_d_array = tic_entry(TextureType::Texture1DArray, true, 3, 4, 2);
        let info = ImageViewInfo::from_tic_entry(&one_d_array, 0);

        assert_eq!(info.view_type, ImageViewType::E1DArray);
        assert_eq!(info.range.extent.layers, 3);
        assert_eq!(info.range.extent.levels, -1);
    }

    #[test]
    fn invalid_texture_type_keeps_initialized_default_type_like_upstream() {
        let mut tic = tic_entry(TextureType::Texture2D, true, 1, 0, 0);
        tic.raw[2] &= !(0xfu64 << 23);
        tic.raw[2] |= 0xfu64 << 23;

        let info = ImageViewInfo::from_tic_entry(&tic, 0);

        assert_eq!(info.view_type, ImageViewType::E1D);
        assert_eq!(info.format, PixelFormat::A8B8G8R8Unorm);
    }

    #[test]
    fn swizzle_returns_the_upstream_texture_enum() {
        let info = ImageViewInfo::default();
        assert_eq!(
            info.swizzle(),
            [
                crate::textures::texture::SwizzleSource::R,
                crate::textures::texture::SwizzleSource::G,
                crate::textures::texture::SwizzleSource::B,
                crate::textures::texture::SwizzleSource::A,
            ]
        );
    }

    #[test]
    fn unnamed_tic_swizzle_value_reaches_backend_validation() {
        let mut tic = tic_entry(TextureType::Texture2D, true, 1, 0, 0);
        tic.raw[0] &= !(0x7u64 << 19);
        tic.raw[0] |= 1u64 << 19;

        let info = ImageViewInfo::from_tic_entry(&tic, 0);

        assert_eq!(info.x_source, 1);
        assert_eq!(info.swizzle()[0], SwizzleSource::Invalid);
    }
}
