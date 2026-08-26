// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/texture_cache/image_info.h and image_info.cpp
//!
//! Contains `ImageInfo`, which describes the properties of a GPU image (texture
//! or render target).  Multiple constructors map from different Tegra register
//! configurations (TIC entry, render target config, depth/stencil config,
//! Fermi2D surface, DMA operand).

use super::types::*;
use crate::surface::{self, SurfaceType};

// ── PixelFormat placeholder ────────────────────────────────────────────
// Upstream lives in video_core/surface.h.  Until that module is ported we
// carry a minimal stand-in so the struct compiles.  Replace with the real
// type once video_core::surface is available.
pub use super::format_lookup_table::PixelFormat;

// ── Constants (from image_info.cpp) ────────────────────────────────────

pub const RESCALE_HEIGHT_THRESHOLD: u32 = 288;
pub const DOWNSCALE_HEIGHT_THRESHOLD: u32 = 512;

fn force_pitch_flush(is_pitch_linear: bool) -> bool {
    is_pitch_linear && !*common::settings::values().use_reactive_flushing.get_value()
}

fn decode_msaa_mode(raw: u32, owner: &str) -> super::samples_helper::MsaaMode {
    super::samples_helper::MsaaMode::from_raw(raw)
        .unwrap_or_else(|| panic!("{owner}: invalid MSAA mode={raw}"))
}

fn stop_unimplemented_byte_size_to_format(bytes_per_pixel: u32) -> ! {
    panic!("ByteSizeToFormat: unimplemented bpp={bytes_per_pixel}");
}

// ── ImageInfo ──────────────────────────────────────────────────────────

/// Describes an image's format, dimensions, tiling, and resource layout.
///
/// Port of `VideoCommon::ImageInfo`.
#[derive(Debug, Clone)]
pub struct ImageInfo {
    pub format: PixelFormat,
    pub image_type: ImageType,
    pub resources: SubresourceExtent,
    pub size: Extent3D,
    /// Block-linear tiling parameters (width/height/depth log2).
    /// When the image is pitch-linear, `pitch` is used instead (overlapping
    /// via the C++ anonymous union).  We model this with an enum.
    pub tiling: TilingMode,
    pub layer_stride: u32,
    pub maybe_unaligned_layer_stride: u32,
    pub num_samples: u32,
    pub tile_width_spacing: u32,
    pub rescaleable: bool,
    pub downscaleable: bool,
    pub forced_flushed: bool,
    pub dma_downloaded: bool,
    pub is_sparse: bool,
}

/// Discriminated union replacing the C++ anonymous `union { Extent3D block; u32 pitch; }`.
#[derive(Debug, Clone, Copy)]
pub enum TilingMode {
    BlockLinear(Extent3D),
    PitchLinear(u32),
}

impl Default for TilingMode {
    fn default() -> Self {
        TilingMode::BlockLinear(Extent3D {
            width: 0,
            height: 0,
            depth: 0,
        })
    }
}

impl Default for ImageInfo {
    fn default() -> Self {
        Self {
            format: PixelFormat::Invalid,
            image_type: ImageType::E1D,
            resources: SubresourceExtent::default(),
            size: Extent3D {
                width: 1,
                height: 1,
                depth: 1,
            },
            tiling: TilingMode::default(),
            layer_stride: 0,
            maybe_unaligned_layer_stride: 0,
            num_samples: 1,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed: false,
            dma_downloaded: false,
            is_sparse: false,
        }
    }
}

impl ImageInfo {
    /// Helper to get the block extent (only valid for block-linear images).
    pub fn block(&self) -> Extent3D {
        match self.tiling {
            TilingMode::BlockLinear(b) => b,
            TilingMode::PitchLinear(_) => Extent3D {
                width: 0,
                height: 0,
                depth: 0,
            },
        }
    }

    /// Helper to get the pitch (only valid for pitch-linear images).
    pub fn pitch(&self) -> u32 {
        match self.tiling {
            TilingMode::PitchLinear(p) => p,
            TilingMode::BlockLinear(_) => 0,
        }
    }

    /// Construct from a TIC entry.
    ///
    /// Port of upstream `ImageInfo::ImageInfo(const TICEntry& config)`
    /// (image_info.cpp:28-125). Decodes format, tiling, MSAA, type and
    /// per-dimension sizes off `TicEntry`'s bitfield accessors.
    ///
    pub fn from_tic_entry(config: &crate::textures::texture::TicEntry) -> Self {
        // Decode from the raw TIC bitfields exactly once; the lookup table uses
        // the canonical `textures::texture` enum discriminants.
        use crate::textures::texture::TextureType;

        let mut info = Self::default();
        let forced_flushed = force_pitch_flush(config.is_pitch_linear());
        info.forced_flushed = forced_flushed;
        info.dma_downloaded = forced_flushed;

        let is_srgb = config.srgb_conversion() != 0;
        info.format = super::format_lookup_table::pixel_format_from_texture_info_raw(
            config.format(),
            config.r_type(),
            config.g_type(),
            config.b_type(),
            config.a_type(),
            is_srgb,
        );

        let msaa_mode = decode_msaa_mode(config.msaa_mode(), "ImageInfo::from_tic_entry");
        info.num_samples = super::samples_helper::num_samples(msaa_mode) as u32;
        info.resources.levels = (config.max_mip_level() + 1) as i32;

        // Tiling: pitch- or block-linear branch.
        if config.is_pitch_linear() {
            info.tiling = TilingMode::PitchLinear(config.pitch());
        } else if config.is_block_linear() {
            info.tiling = TilingMode::BlockLinear(Extent3D {
                width: config.block_width(),
                height: config.block_height(),
                depth: config.block_depth(),
            });
        }

        info.is_sparse = config.is_sparse() != 0;
        info.tile_width_spacing = config.tile_width_spacing();

        // TextureType → ImageType + size.
        let mut texture_type = TextureType::from_raw(config.texture_type()).unwrap_or_else(|| {
            panic!(
                "ImageInfo::from_tic_entry: invalid texture_type={}",
                config.texture_type()
            )
        });
        if texture_type != TextureType::Texture2D
            && texture_type != TextureType::Texture2DNoMipmap
            && config.is_pitch_linear()
        {
            panic!(
                "ImageInfo::from_tic_entry: non-2D pitch-linear TIC texture_type={}",
                config.texture_type()
            );
        }
        if config.depth() > 1 || config.base_layer() != 0 {
            texture_type = match texture_type {
                TextureType::Texture1D => TextureType::Texture1DArray,
                TextureType::Texture2D => TextureType::Texture2DArray,
                TextureType::TextureCubemap => TextureType::TextureCubeArray,
                other => other,
            };
        }
        match texture_type {
            TextureType::Texture1D => {
                assert_eq!(
                    config.base_layer(),
                    0,
                    "ImageInfo::from_tic_entry: Texture1D base layer must be zero"
                );
                info.image_type = ImageType::E1D;
                info.size.width = config.width();
                info.resources.layers = 1;
            }
            TextureType::Texture1DArray => {
                info.image_type = ImageType::E1D;
                info.size.width = config.width();
                info.resources.layers = (config.base_layer() + config.depth()) as i32;
            }
            TextureType::Texture2D | TextureType::Texture2DNoMipmap => {
                assert_eq!(
                    config.depth(),
                    1,
                    "ImageInfo::from_tic_entry: Texture2D depth must be one"
                );
                info.image_type = if config.is_pitch_linear() {
                    ImageType::Linear
                } else {
                    ImageType::E2D
                };
                info.size.width = config.width();
                info.size.height = config.height();
                info.resources.layers = config.base_layer() as i32 + 1;
                info.rescaleable = !config.is_pitch_linear();
            }
            TextureType::Texture2DArray => {
                info.image_type = ImageType::E2D;
                info.size.width = config.width();
                info.size.height = config.height();
                info.resources.layers = (config.base_layer() + config.depth()) as i32;
                info.rescaleable = true;
            }
            TextureType::TextureCubemap => {
                assert_eq!(
                    config.depth(),
                    1,
                    "ImageInfo::from_tic_entry: TextureCubemap depth must be one"
                );
                info.image_type = ImageType::E2D;
                info.size.width = config.width();
                info.size.height = config.height();
                info.resources.layers = config.base_layer() as i32 + 6;
            }
            TextureType::TextureCubeArray => {
                if config.load_store_hint() != 0 {
                    panic!(
                        "ImageInfo::from_tic_entry: TextureCubeArray load_store_hint is not zero"
                    );
                }
                info.image_type = ImageType::E2D;
                info.size.width = config.width();
                info.size.height = config.height();
                info.resources.layers = (config.base_layer() + config.depth() * 6) as i32;
            }
            TextureType::Texture3D => {
                assert_eq!(
                    config.base_layer(),
                    0,
                    "ImageInfo::from_tic_entry: Texture3D base layer must be zero"
                );
                info.image_type = ImageType::E3D;
                info.size.width = config.width();
                info.size.height = config.height();
                info.size.depth = config.depth();
                info.resources.layers = 1;
            }
            TextureType::Texture1DBuffer => {
                info.image_type = ImageType::Buffer;
                info.size.width = config.width();
                info.resources.layers = 1;
            }
        }

        if info.num_samples > 1 {
            info.size.width *= super::samples_helper::num_samples_x(msaa_mode) as u32;
            info.size.height *= super::samples_helper::num_samples_y(msaa_mode) as u32;
        }

        if info.image_type != ImageType::Linear {
            info.layer_stride = super::util::calculate_layer_stride(&info);
            info.maybe_unaligned_layer_stride = super::util::calculate_layer_size(&info);
            let block = info.block();
            info.rescaleable &= block.depth == 0 && info.resources.levels == 1;
            info.rescaleable &= info.size.height > RESCALE_HEIGHT_THRESHOLD
                || surface::get_format_type(info.format) != SurfaceType::ColorTexture;
            info.downscaleable = info.size.height > DOWNSCALE_HEIGHT_THRESHOLD;
        }

        info
    }

    /// Construct from a render target config.
    ///
    /// Port of `ImageInfo::ImageInfo(const RenderTargetConfig& ct, MsaaMode)`.
    pub fn from_render_target_info(
        config: &crate::engines::maxwell_3d::RenderTargetInfo,
        msaa_mode: u32,
    ) -> Self {
        let format = crate::surface::pixel_format_from_render_target_format(config.format);
        let is_pitch_linear = (config.tile_mode & (1 << 12)) != 0;
        let forced_flushed = force_pitch_flush(is_pitch_linear);
        let dim_control_define_depth_size = (config.tile_mode & (1 << 16)) != 0;
        let mut info = Self {
            format,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: config.width,
                height: config.height,
                depth: 1,
            },
            tiling: TilingMode::default(),
            layer_stride: 0,
            maybe_unaligned_layer_stride: 0,
            num_samples: 1,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed,
            dma_downloaded: forced_flushed,
            is_sparse: false,
        };

        if is_pitch_linear {
            assert!(
                !dim_control_define_depth_size,
                "ImageInfo::from_render_target_info: pitch-linear dim_control must define array size"
            );
            let pitch = config.width;
            info.image_type = ImageType::Linear;
            info.tiling = TilingMode::PitchLinear(pitch);
            info.size.width = pitch / crate::surface::bytes_per_block(format);
            return info;
        }

        let msaa_mode = decode_msaa_mode(msaa_mode, "ImageInfo::from_render_target_info");
        info.num_samples = super::samples_helper::num_samples(msaa_mode) as u32;
        info.layer_stride = config.array_pitch.wrapping_mul(4);
        info.maybe_unaligned_layer_stride = info.layer_stride;
        info.tiling = TilingMode::BlockLinear(Extent3D {
            width: config.tile_mode & 0xF,
            height: (config.tile_mode >> 4) & 0xF,
            depth: (config.tile_mode >> 8) & 0xF,
        });
        if dim_control_define_depth_size {
            info.image_type = ImageType::E3D;
            info.size.depth = config.depth & 0xFFFF;
        } else {
            info.image_type = ImageType::E2D;
            info.resources.layers = (config.depth & 0xFFFF) as i32;
            info.rescaleable =
                info.block().depth == 0 && info.size.height > RESCALE_HEIGHT_THRESHOLD;
            info.downscaleable = info.size.height > DOWNSCALE_HEIGHT_THRESHOLD;
        }

        info
    }

    /// Construct from depth/stencil config.
    ///
    /// Port of `ImageInfo::ImageInfo(const Zeta&, const ZetaSize&, MsaaMode)`.
    pub fn from_zeta_info(config: &crate::engines::maxwell_3d::ZetaInfo, msaa_mode: u32) -> Self {
        let format = crate::surface::pixel_format_from_depth_format(config.format);
        let is_pitch_linear = (config.tile_mode & (1 << 12)) != 0;
        let forced_flushed = force_pitch_flush(is_pitch_linear);
        let dim_control_define_depth_size = (config.tile_mode & (1 << 16)) != 0;
        let msaa_mode = decode_msaa_mode(msaa_mode, "ImageInfo::from_zeta_info");
        let mut info = Self {
            format,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: config.width,
                height: config.height,
                depth: 1,
            },
            tiling: TilingMode::default(),
            layer_stride: config.array_pitch.wrapping_mul(4),
            maybe_unaligned_layer_stride: config.array_pitch.wrapping_mul(4),
            num_samples: super::samples_helper::num_samples(msaa_mode) as u32,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed,
            dma_downloaded: forced_flushed,
            is_sparse: false,
        };

        if is_pitch_linear {
            assert!(
                !dim_control_define_depth_size,
                "ImageInfo::from_zeta_info: pitch-linear dim_control must define array size"
            );
            info.image_type = ImageType::Linear;
            info.tiling = TilingMode::PitchLinear(
                config
                    .width
                    .wrapping_mul(crate::surface::bytes_per_block(format)),
            );
            return info;
        }

        info.tiling = TilingMode::BlockLinear(Extent3D {
            width: config.tile_mode & 0xF,
            height: (config.tile_mode >> 4) & 0xF,
            depth: (config.tile_mode >> 8) & 0xF,
        });
        if dim_control_define_depth_size {
            let array_size_is_one = ((config.depth >> 16) & 1) != 0;
            assert!(
                array_size_is_one,
                "ImageInfo::from_zeta_info: 3D zeta size dim_control must be ArraySizeIsOne"
            );
            info.image_type = ImageType::E3D;
            info.size.depth = config.depth & 0xFFFF;
        } else {
            info.image_type = ImageType::E2D;
            let array_size_is_one = ((config.depth >> 16) & 1) != 0;
            info.resources.layers = if array_size_is_one {
                1
            } else {
                (config.depth & 0xFFFF) as i32
            };
            info.rescaleable = info.block().depth == 0;
            info.downscaleable = info.size.height > DOWNSCALE_HEIGHT_THRESHOLD;
        }

        info
    }

    /// Construct from a Fermi2D surface.
    ///
    /// Port of `ImageInfo::ImageInfo(const Fermi2D::Surface& config)`.
    pub fn from_fermi2d_surface(config: &crate::engines::fermi_2d::Surface) -> Self {
        if config.layer != 0 {
            log::error!("ImageInfo::from_fermi2d_surface: surface layer is not zero");
            if *common::settings::values().use_debug_asserts.get_value() {
                panic!("ImageInfo::from_fermi2d_surface: surface layer is not zero");
            }
        }
        let format = crate::surface::pixel_format_from_render_target_format(config.format);
        let forced_flushed = force_pitch_flush(
            config.linear == crate::engines::fermi_2d::MemoryLayout::Pitch as u32,
        );
        let mut info = Self {
            format,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: config.width,
                height: config.height,
                depth: 1,
            },
            tiling: TilingMode::default(),
            layer_stride: 0,
            maybe_unaligned_layer_stride: 0,
            num_samples: 1,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed,
            dma_downloaded: forced_flushed,
            is_sparse: false,
        };

        if config.linear == crate::engines::fermi_2d::MemoryLayout::Pitch as u32 {
            info.image_type = ImageType::Linear;
            info.size.width = config.pitch / crate::surface::bytes_per_block(format);
            info.tiling = TilingMode::PitchLinear(config.pitch);
        } else {
            info.image_type = if config.block_depth() > 0 {
                ImageType::E3D
            } else {
                ImageType::E2D
            };
            info.tiling = TilingMode::BlockLinear(Extent3D {
                width: config.block_width(),
                height: config.block_height(),
                depth: config.block_depth(),
            });
            info.rescaleable =
                info.block().depth == 0 && info.size.height > RESCALE_HEIGHT_THRESHOLD;
            info.downscaleable = info.size.height > DOWNSCALE_HEIGHT_THRESHOLD;
        }

        info
    }

    /// Construct from a DMA image operand.
    ///
    /// Port of `ImageInfo::ImageInfo(const DMA::ImageOperand& config)`.
    ///
    pub fn from_dma_operand(config: &crate::engines::maxwell_dma::dma::ImageOperand) -> Self {
        let format = byte_size_to_format(config.bytes_per_pixel);
        let block = Extent3D {
            width: config.params.block_size.width(),
            height: config.params.block_size.height(),
            depth: config.params.block_size.depth(),
        };
        let mut info = Self {
            format,
            image_type: if block.depth > 0 {
                ImageType::E3D
            } else {
                ImageType::E2D
            },
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: config.params.width,
                height: config.params.height,
                depth: config.params.depth,
            },
            tiling: TilingMode::BlockLinear(block),
            layer_stride: 0,
            maybe_unaligned_layer_stride: 0,
            num_samples: 1,
            tile_width_spacing: 0,
            rescaleable: false,
            downscaleable: false,
            forced_flushed: false,
            dma_downloaded: false,
            is_sparse: false,
        };
        info.layer_stride = super::util::calculate_layer_stride(&info);
        info.maybe_unaligned_layer_stride = super::util::calculate_layer_size(&info);
        info.rescaleable = block.depth == 0 && info.size.height > RESCALE_HEIGHT_THRESHOLD;
        info.downscaleable = info.size.height > DOWNSCALE_HEIGHT_THRESHOLD;
        info
    }
}

/// Map a byte-per-pixel value to a PixelFormat suitable for raw DMA copies.
///
/// Port of the file-static `ByteSizeToFormat` in image_info.cpp.
pub fn byte_size_to_format(bytes_per_pixel: u32) -> PixelFormat {
    match bytes_per_pixel {
        1 => PixelFormat::R8Uint,
        2 => PixelFormat::R8G8Uint,
        4 => PixelFormat::A8B8G8R8Uint,
        8 => PixelFormat::R16G16B16A16Uint,
        16 => PixelFormat::R32G32B32A32Uint,
        _ => stop_unimplemented_byte_size_to_format(bytes_per_pixel),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::fermi_2d::{MemoryLayout, Surface};
    use crate::engines::maxwell_3d::{RenderTargetInfo, ZetaInfo};
    use crate::engines::maxwell_dma::dma::{BlockSize, ImageOperand, Parameters};
    use crate::gpu::RenderTargetFormat;
    use crate::surface::PixelFormat as SurfacePixelFormat;
    use crate::textures::texture::{
        ComponentType, TextureFormat, TextureType, TicEntry, TicHeaderVersion,
    };

    fn tic_for_validation(
        texture_type: u32,
        header_version: TicHeaderVersion,
        depth: u32,
        base_layer: u32,
        load_store_hint: u32,
    ) -> TicEntry {
        let word0 = (TextureFormat::A8B8G8R8 as u32)
            | ((ComponentType::Unorm as u32) << 7)
            | ((ComponentType::Unorm as u32) << 10)
            | ((ComponentType::Unorm as u32) << 13)
            | ((ComponentType::Unorm as u32) << 16);
        let word2 = (((base_layer >> 3) & 0x1F) << 16)
            | ((header_version as u32) << 21)
            | ((load_store_hint & 1) << 24)
            | (((base_layer >> 8) & 0x7) << 29);
        let word3 = 0;
        let word4 = 63 | ((base_layer & 0x7) << 16) | (texture_type << 23);
        let word5 = 63 | (depth.saturating_sub(1) << 16);
        TicEntry {
            raw: [
                word0 as u64,
                ((word3 as u64) << 32) | word2 as u64,
                ((word5 as u64) << 32) | word4 as u64,
                0,
            ],
        }
    }

    #[test]
    fn render_target_info_decodes_block_linear_layout() {
        let info = ImageInfo::from_render_target_info(
            &RenderTargetInfo {
                address: 0x5000_0000,
                width: 1920,
                height: 1080,
                format: 0xD5,
                tile_mode: 2 | (3 << 4) | (1 << 8),
                depth: 4,
                array_pitch: 0x200,
                base_layer: 0,
            },
            0,
        );

        assert_eq!(info.format, SurfacePixelFormat::A8B8G8R8Unorm);
        assert_eq!(info.image_type, ImageType::E2D);
        assert_eq!(info.size.width, 1920);
        assert_eq!(info.size.height, 1080);
        assert_eq!(info.resources.layers, 4);
        assert_eq!(info.layer_stride, 0x800);
        assert_eq!(info.block().width, 2);
        assert_eq!(info.block().height, 3);
        assert_eq!(info.block().depth, 1);
    }

    #[test]
    fn render_target_info_decodes_pitch_linear_layout() {
        let info = ImageInfo::from_render_target_info(
            &RenderTargetInfo {
                address: 0x5000_0000,
                width: 256,
                height: 16,
                format: 0xD5,
                tile_mode: 1 << 12,
                depth: 1,
                array_pitch: 0,
                base_layer: 0,
            },
            0,
        );

        assert_eq!(info.image_type, ImageType::Linear);
        assert_eq!(info.pitch(), 256);
        assert_eq!(info.size.width, 64);
        assert_eq!(info.size.height, 16);
        assert!(!info.forced_flushed);
        assert!(!info.dma_downloaded);
    }

    #[test]
    fn render_target_pitch_linear_returns_before_msaa_decode_like_upstream() {
        let info = ImageInfo::from_render_target_info(
            &RenderTargetInfo {
                address: 0x5000_0000,
                width: 256,
                height: 16,
                format: 0xD5,
                tile_mode: 1 << 12,
                depth: 1,
                array_pitch: 0,
                base_layer: 0,
            },
            15,
        );

        assert_eq!(info.image_type, ImageType::Linear);
        assert_eq!(info.num_samples, 1);
    }

    #[test]
    fn render_target_and_zeta_info_use_msaa_mode() {
        let render_target = RenderTargetInfo {
            address: 0x5000_0000,
            width: 1280,
            height: 720,
            format: 0xD5,
            tile_mode: 2 | (1 << 4),
            depth: 1,
            array_pitch: 0x200,
            base_layer: 0,
        };
        let zeta = ZetaInfo {
            enabled: true,
            address: 0x6000_0000,
            format: 0xA,
            tile_mode: 2 | (1 << 4),
            array_pitch: 0x200,
            width: 1280,
            height: 720,
            depth: 1,
        };

        assert_eq!(
            ImageInfo::from_render_target_info(&render_target, 3).num_samples,
            8
        );
        assert_eq!(ImageInfo::from_zeta_info(&zeta, 3).num_samples, 8);
    }

    #[test]
    #[should_panic(expected = "ImageInfo::from_render_target_info: invalid MSAA mode=15")]
    fn render_target_invalid_msaa_mode_is_fatal_like_upstream() {
        let render_target = RenderTargetInfo {
            address: 0x5000_0000,
            width: 1280,
            height: 720,
            format: 0xD5,
            tile_mode: 2 | (1 << 4),
            depth: 1,
            array_pitch: 0x200,
            base_layer: 0,
        };

        let _ = ImageInfo::from_render_target_info(&render_target, 15);
    }

    #[test]
    #[should_panic(expected = "ImageInfo::from_zeta_info: invalid MSAA mode=15")]
    fn zeta_invalid_msaa_mode_is_fatal_like_upstream() {
        let zeta = ZetaInfo {
            enabled: true,
            address: 0x6000_0000,
            format: 0xA,
            tile_mode: 2 | (1 << 4),
            array_pitch: 0x200,
            width: 1280,
            height: 720,
            depth: 1,
        };

        let _ = ImageInfo::from_zeta_info(&zeta, 15);
    }

    #[test]
    #[should_panic(expected = "pitch-linear dim_control must define array size")]
    fn render_target_pitch_linear_depth_dim_control_is_fatal_like_upstream() {
        let render_target = RenderTargetInfo {
            address: 0x5000_0000,
            width: 256,
            height: 16,
            format: 0xD5,
            tile_mode: (1 << 12) | (1 << 16),
            depth: 1,
            array_pitch: 0,
            base_layer: 0,
        };

        let _ = ImageInfo::from_render_target_info(&render_target, 0);
    }

    #[test]
    #[should_panic(expected = "pitch-linear dim_control must define array size")]
    fn zeta_pitch_linear_depth_dim_control_is_fatal_like_upstream() {
        let zeta = ZetaInfo {
            enabled: true,
            address: 0x6000_0000,
            format: 0xA,
            tile_mode: (1 << 12) | (1 << 16),
            array_pitch: 0,
            width: 256,
            height: 16,
            depth: 1,
        };

        let _ = ImageInfo::from_zeta_info(&zeta, 0);
    }

    #[test]
    #[should_panic(expected = "3D zeta size dim_control must be ArraySizeIsOne")]
    fn zeta_3d_without_array_size_one_is_fatal_like_upstream() {
        let zeta = ZetaInfo {
            enabled: true,
            address: 0x6000_0000,
            format: 0xA,
            tile_mode: 2 | (1 << 4) | (1 << 16),
            array_pitch: 0x200,
            width: 1280,
            height: 720,
            depth: 4,
        };

        let _ = ImageInfo::from_zeta_info(&zeta, 0);
    }

    #[test]
    fn render_target_and_zeta_layers_preserve_zero_depth_like_upstream() {
        let render_target = RenderTargetInfo {
            address: 0x5000_0000,
            width: 1280,
            height: 720,
            format: 0xD5,
            tile_mode: 2 | (1 << 4),
            depth: 0,
            array_pitch: 0x200,
            base_layer: 0,
        };
        let zeta = ZetaInfo {
            enabled: true,
            address: 0x6000_0000,
            format: 0xA,
            tile_mode: 2 | (1 << 4),
            array_pitch: 0x200,
            width: 1280,
            height: 720,
            depth: 0,
        };

        assert_eq!(
            ImageInfo::from_render_target_info(&render_target, 0)
                .resources
                .layers,
            0
        );
        assert_eq!(ImageInfo::from_zeta_info(&zeta, 0).resources.layers, 0);
    }

    #[test]
    fn tic_entry_decodes_mips_msaa_and_layer_stride() {
        let word0 = (TextureFormat::A8B8G8R8 as u32)
            | ((ComponentType::Unorm as u32) << 7)
            | ((ComponentType::Unorm as u32) << 10)
            | ((ComponentType::Unorm as u32) << 13)
            | ((ComponentType::Unorm as u32) << 16);
        let word3 = (2 << 10) | (3 << 28);
        let word4 = 63 | ((TextureType::Texture2D as u32) << 23);
        let word5 = 31 | (1 << 31);
        let word7 = 2 << 8;
        let word2 = 3 << 21;
        let tic = TicEntry {
            raw: [
                word0 as u64,
                ((word3 as u64) << 32) | word2 as u64,
                ((word5 as u64) << 32) | word4 as u64,
                (word7 as u64) << 32,
            ],
        };

        let info = ImageInfo::from_tic_entry(&tic);

        assert_eq!(info.resources.levels, 4);
        assert_eq!(info.num_samples, 4);
        assert_eq!(info.tile_width_spacing, 2);
        assert_eq!(info.size.width, 128);
        assert_eq!(info.size.height, 64);
        assert_ne!(info.layer_stride, 0);
        assert_ne!(info.maybe_unaligned_layer_stride, 0);
    }

    #[test]
    fn tic_entry_promotes_nonzero_depth_or_base_layer_like_upstream() {
        let promoted_1d = tic_for_validation(
            TextureType::Texture1D as u32,
            TicHeaderVersion::BlockLinear,
            2,
            3,
            0,
        );
        let promoted_2d = tic_for_validation(
            TextureType::Texture2D as u32,
            TicHeaderVersion::BlockLinear,
            2,
            4,
            0,
        );
        let promoted_cube = tic_for_validation(
            TextureType::TextureCubemap as u32,
            TicHeaderVersion::BlockLinear,
            2,
            5,
            0,
        );

        let info_1d = ImageInfo::from_tic_entry(&promoted_1d);
        let info_2d = ImageInfo::from_tic_entry(&promoted_2d);
        let info_cube = ImageInfo::from_tic_entry(&promoted_cube);

        assert_eq!(info_1d.image_type, ImageType::E1D);
        assert_eq!(info_1d.resources.layers, 5);
        assert_eq!(info_2d.image_type, ImageType::E2D);
        assert_eq!(info_2d.resources.layers, 6);
        assert_eq!(info_cube.image_type, ImageType::E2D);
        assert_eq!(info_cube.resources.layers, 17);
    }

    #[test]
    #[should_panic(expected = "invalid texture_type=15")]
    fn tic_entry_invalid_texture_type_is_fatal_like_upstream() {
        let tic = tic_for_validation(15, TicHeaderVersion::BlockLinear, 1, 0, 0);

        let _ = ImageInfo::from_tic_entry(&tic);
    }

    #[test]
    fn tic_entry_unmapped_raw_format_components_use_upstream_fallback() {
        let word0 = TextureFormat::A8B8G8R8 as u32;
        let word3 = 0;
        let word4 = 63 | ((TextureType::Texture2D as u32) << 23);
        let word5 = 63;
        let tic = TicEntry {
            raw: [
                word0 as u64,
                ((word3 as u64) << 32) | ((TicHeaderVersion::BlockLinear as u64) << 21),
                ((word5 as u64) << 32) | word4 as u64,
                0,
            ],
        };

        let info = ImageInfo::from_tic_entry(&tic);

        assert_eq!(info.format, PixelFormat::A8B8G8R8Unorm);
    }

    #[test]
    #[should_panic(expected = "invalid MSAA mode=15")]
    fn tic_entry_invalid_msaa_mode_is_fatal_like_upstream() {
        let mut tic = tic_for_validation(
            TextureType::Texture2D as u32,
            TicHeaderVersion::BlockLinear,
            1,
            0,
            0,
        );
        tic.raw[3] = (15u64) << 40;

        let _ = ImageInfo::from_tic_entry(&tic);
    }

    #[test]
    #[should_panic(expected = "non-2D pitch-linear TIC")]
    fn tic_entry_non_2d_pitch_linear_is_fatal_like_upstream() {
        let tic = tic_for_validation(
            TextureType::Texture3D as u32,
            TicHeaderVersion::Pitch,
            1,
            0,
            0,
        );

        let _ = ImageInfo::from_tic_entry(&tic);
    }

    #[test]
    fn tic_entry_type_validation_guards_are_fatal_like_upstream() {
        let cases = [
            (
                TextureType::TextureCubeArray as u32,
                1,
                0,
                1,
                "TextureCubeArray load_store_hint is not zero",
            ),
            (
                TextureType::Texture3D as u32,
                1,
                1,
                0,
                "Texture3D base layer must be zero",
            ),
        ];

        for (texture_type, depth, base_layer, load_store_hint, expected) in cases {
            let tic = tic_for_validation(
                texture_type,
                TicHeaderVersion::BlockLinear,
                depth,
                base_layer,
                load_store_hint,
            );
            let panic =
                std::panic::catch_unwind(|| ImageInfo::from_tic_entry(&tic)).expect_err(expected);
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("<non-string panic>");
            assert!(
                message.contains(expected),
                "expected panic containing `{expected}`, got `{message}`"
            );
        }
    }

    #[test]
    fn dma_operand_decodes_2d_block_linear_image_info() {
        let operand = ImageOperand {
            bytes_per_pixel: 4,
            params: Parameters {
                block_size: BlockSize { raw: 2 | (3 << 4) },
                width: 320,
                height: 240,
                depth: 1,
                layer: 0,
                origin: Default::default(),
            },
            address: 0x5000_0000,
        };

        let info = ImageInfo::from_dma_operand(&operand);

        assert_eq!(info.format, PixelFormat::A8B8G8R8Uint);
        assert_eq!(info.image_type, ImageType::E2D);
        assert_eq!(info.num_samples, 1);
        assert_eq!(info.resources.levels, 1);
        assert_eq!(info.resources.layers, 1);
        assert_eq!(info.size.width, 320);
        assert_eq!(info.size.height, 240);
        assert_eq!(info.size.depth, 1);
        assert_eq!(info.block().width, 2);
        assert_eq!(info.block().height, 3);
        assert_eq!(info.block().depth, 0);
        assert_eq!(info.tile_width_spacing, 0);
        assert_ne!(info.layer_stride, 0);
        assert_ne!(info.maybe_unaligned_layer_stride, 0);
        assert!(!info.rescaleable);
        assert!(!info.downscaleable);
    }

    #[test]
    fn dma_operand_depth_block_selects_3d_image_type() {
        let operand = ImageOperand {
            bytes_per_pixel: 8,
            params: Parameters {
                block_size: BlockSize {
                    raw: 1 | (2 << 4) | (1 << 8),
                },
                width: 64,
                height: 600,
                depth: 4,
                layer: 0,
                origin: Default::default(),
            },
            address: 0x6000_0000,
        };

        let info = ImageInfo::from_dma_operand(&operand);

        assert_eq!(info.format, PixelFormat::R16G16B16A16Uint);
        assert_eq!(info.image_type, ImageType::E3D);
        assert_eq!(info.size.depth, 4);
        assert_eq!(info.block().depth, 1);
        assert!(!info.rescaleable);
        assert!(info.downscaleable);
    }

    #[test]
    fn fermi2d_block_linear_sets_rescale_flags_like_upstream() {
        let info = ImageInfo::from_fermi2d_surface(&Surface {
            format: RenderTargetFormat::A8B8G8R8Unorm as u32,
            linear: MemoryLayout::BlockLinear as u32,
            block_dimensions: 2 | (3 << 4),
            depth: 1,
            layer: 0,
            pitch: 0,
            width: 640,
            height: 600,
            addr_upper: 0,
            addr_lower: 0,
        });

        assert_eq!(info.image_type, ImageType::E2D);
        assert_eq!(info.size.width, 640);
        assert_eq!(info.size.height, 600);
        assert_eq!(info.size.depth, 1);
        assert_eq!(info.block().depth, 0);
        assert!(info.rescaleable);
        assert!(info.downscaleable);
    }

    #[test]
    fn fermi2d_3d_block_linear_is_not_rescaleable() {
        let info = ImageInfo::from_fermi2d_surface(&Surface {
            format: RenderTargetFormat::A8B8G8R8Unorm as u32,
            linear: MemoryLayout::BlockLinear as u32,
            block_dimensions: 2 | (3 << 4) | (1 << 8),
            depth: 4,
            layer: 0,
            pitch: 0,
            width: 640,
            height: 600,
            addr_upper: 0,
            addr_lower: 0,
        });

        assert_eq!(info.image_type, ImageType::E3D);
        assert_eq!(info.size.depth, 1);
        assert_eq!(info.block().depth, 1);
        assert!(!info.rescaleable);
        assert!(info.downscaleable);
    }

    #[test]
    fn fermi2d_surface_nonzero_layer_is_fail_soft_like_upstream() {
        let info = ImageInfo::from_fermi2d_surface(&Surface {
            format: RenderTargetFormat::A8B8G8R8Unorm as u32,
            linear: MemoryLayout::BlockLinear as u32,
            block_dimensions: 0,
            depth: 1,
            layer: 1,
            pitch: 0,
            width: 64,
            height: 64,
            addr_upper: 0,
            addr_lower: 0,
        });
        assert_eq!(info.size.depth, 1);
    }

    #[test]
    #[should_panic(expected = "ByteSizeToFormat: unimplemented bpp=3")]
    fn byte_size_to_format_unknown_bpp_is_fatal_like_upstream() {
        let _ = byte_size_to_format(3);
    }
}
