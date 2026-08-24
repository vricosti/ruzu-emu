// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/texture_cache/image_view_info.h and image_view_info.cpp
//!
//! `ImageViewInfo` describes the properties used to look up or create an image
//! view (sub-resource of a texture).

use super::format_lookup_table::PixelFormat;
use super::types::*;
use super::util::pixel_format_from_tic;
use crate::textures::texture::{SwizzleSource as TegraSwizzleSource, TextureType, TicEntry};

// ── SwizzleSource placeholder ──────────────────────────────────────────
// Upstream: Tegra::Texture::SwizzleSource in textures/texture.h
// Minimal stand-in; replace with real type once available.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SwizzleSource {
    Zero = 0,
    R = 2,
    G = 3,
    B = 4,
    A = 5,
    OneInt = 6,
    OneFloat = 7,
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
                    levels: (config.res_max_mip_level() - config.res_min_mip_level() + 1) as i32,
                    layers: 1,
                },
            },
            ..Self::default()
        };

        let mut texture_type =
            TextureType::from_raw(config.texture_type()).expect("Invalid TIC texture_type");
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
                assert_eq!(config.height(), 1);
                assert_eq!(config.depth(), 1);
                info.view_type = ImageViewType::E1D;
            }
            TextureType::Texture2D | TextureType::Texture2DNoMipmap => {
                assert_eq!(config.depth(), 1);
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
                assert_eq!(config.depth(), 1);
                info.view_type = ImageViewType::Cube;
                info.range.extent.layers = 6;
            }
            TextureType::Texture1DArray => {
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
                info.range.extent.layers = (config.depth() * 6) as i32;
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
        [
            // Safety: values are validated on construction in the full port.
            unsafe { std::mem::transmute(self.x_source) },
            unsafe { std::mem::transmute(self.y_source) },
            unsafe { std::mem::transmute(self.z_source) },
            unsafe { std::mem::transmute(self.w_source) },
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
    let source = TegraSwizzleSource::from_raw(source).expect("Invalid TIC swizzle source");
    source as u8
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
            | ((TegraSwizzleSource::R as u32) << 19)
            | ((TegraSwizzleSource::G as u32) << 22)
            | ((TegraSwizzleSource::B as u32) << 25)
            | ((TegraSwizzleSource::A as u32) << 28);
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
}
