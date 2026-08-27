// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/texture_cache/image_view_base.h and image_view_base.cpp
//!
//! Defines `ImageViewBase`, the backend-independent base for image views.

use super::format_lookup_table::PixelFormat;
use super::image_base::GPUVAddr;
use super::image_info::ImageInfo;
use super::image_view_info::ImageViewInfo;
use super::types::*;

fn fail_soft(message: String) {
    log::error!("{message}");
    if *common::settings::values().use_debug_asserts.get_value() {
        panic!("{message}");
    }
}

// ── ImageViewFlagBits ──────────────────────────────────────────────────

bitflags::bitflags! {
    /// Flags associated with a cached image view.
    ///
    /// Port of `VideoCommon::ImageViewFlagBits`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ImageViewFlagBits: u16 {
        const PREEMTIVE_DOWNLOAD = 1 << 0;
        const STRONG             = 1 << 1;
        const SLICE              = 1 << 2;
    }
}

// ── NullImageViewParams ────────────────────────────────────────────────

/// Tag type for constructing a null/sentinel image view.
pub struct NullImageViewParams;

// ── ImageViewBase ──────────────────────────────────────────────────────

/// Backend-independent base for a cached image view.
///
/// Port of `VideoCommon::ImageViewBase`.
#[derive(Debug, Clone)]
pub struct ImageViewBase {
    pub image_id: ImageId,
    pub gpu_addr: GPUVAddr,
    pub format: PixelFormat,
    pub view_type: ImageViewType,
    pub range: SubresourceRange,
    pub size: Extent3D,
    pub flags: ImageViewFlagBits,

    pub invalidation_tick: u64,
    pub modification_tick: u64,
}

impl ImageViewBase {
    /// Construct from image view info, parent image info, image id, and address.
    ///
    /// Port of `ImageViewBase::ImageViewBase(const ImageViewInfo&, const ImageInfo&,
    ///                                        ImageId, GPUVAddr)`.
    pub fn new(
        info: &ImageViewInfo,
        image_info: &ImageInfo,
        image_id: ImageId,
        addr: GPUVAddr,
    ) -> Self {
        let level = info.range.base.level as u32;
        let mut view = Self {
            image_id,
            gpu_addr: addr,
            format: info.format,
            view_type: info.view_type,
            range: info.range,
            size: Extent3D {
                width: (image_info.size.width >> level).max(1),
                height: (image_info.size.height >> level).max(1),
                depth: (image_info.size.depth >> level).max(1),
            },
            flags: ImageViewFlagBits::empty(),
            invalidation_tick: 0,
            modification_tick: 0,
        };
        if !crate::compatible_formats::is_view_compatible(
            image_info.format,
            info.format,
            false,
            true,
        ) {
            fail_soft(format!(
                "Image view format {:?} is incompatible with image format {:?}",
                info.format, image_info.format
            ));
        }
        if image_info.forced_flushed {
            view.flags |= ImageViewFlagBits::PREEMTIVE_DOWNLOAD;
        }
        if image_info.image_type == ImageType::E3D && info.view_type != ImageViewType::E3D {
            view.flags |= ImageViewFlagBits::SLICE;
        }
        view
    }

    /// Construct a buffer-type view.
    ///
    /// Port of `ImageViewBase::ImageViewBase(const ImageInfo&, const ImageViewInfo&, GPUVAddr)`.
    pub fn new_buffer(info: &ImageInfo, view_info: &ImageViewInfo, addr: GPUVAddr) -> Self {
        let view = Self {
            image_id: NULL_IMAGE_ID,
            gpu_addr: addr,
            format: info.format,
            view_type: ImageViewType::Buffer,
            range: SubresourceRange::default(),
            size: Extent3D {
                width: info.size.width,
                height: 1,
                depth: 1,
            },
            flags: ImageViewFlagBits::empty(),
            invalidation_tick: 0,
            modification_tick: 0,
        };
        if view_info.view_type != ImageViewType::Buffer {
            fail_soft("Expected texture buffer".to_owned());
        }
        view
    }

    /// Construct a null/sentinel image view.
    ///
    /// Port of `ImageViewBase::ImageViewBase(const NullImageViewParams&)`.
    pub fn null(_: NullImageViewParams) -> Self {
        Self {
            image_id: NULL_IMAGE_ID,
            gpu_addr: 0,
            format: PixelFormat::Invalid,
            view_type: ImageViewType::E1D,
            range: SubresourceRange::default(),
            size: Extent3D {
                width: 0,
                height: 0,
                depth: 0,
            },
            flags: ImageViewFlagBits::empty(),
            invalidation_tick: 0,
            modification_tick: 0,
        }
    }

    /// Whether this view is a buffer view.
    pub fn is_buffer(&self) -> bool {
        self.view_type == ImageViewType::Buffer
    }

    /// Whether this view supports anisotropic filtering.
    ///
    /// Port of `ImageViewBase::SupportsAnisotropy`.
    pub fn supports_anisotropy(&self) -> bool {
        let has_mips = self.range.extent.levels > 1;
        let is_2d =
            self.view_type == ImageViewType::E2D || self.view_type == ImageViewType::E2DArray;
        if !has_mips || !is_2d {
            return false;
        }

        // Formats that do not support anisotropy
        !matches!(
            self.format,
            PixelFormat::R8Unorm
                | PixelFormat::R8Snorm
                | PixelFormat::R8Sint
                | PixelFormat::R8Uint
                | PixelFormat::Bc4Unorm
                | PixelFormat::Bc4Snorm
                | PixelFormat::Bc5Unorm
                | PixelFormat::Bc5Snorm
                | PixelFormat::R32G32Float
                | PixelFormat::R32G32Sint
                | PixelFormat::R32Float
                | PixelFormat::R16Float
                | PixelFormat::R16Unorm
                | PixelFormat::R16Snorm
                | PixelFormat::R16Uint
                | PixelFormat::R16Sint
                | PixelFormat::R16G16Unorm
                | PixelFormat::R16G16Float
                | PixelFormat::R16G16Uint
                | PixelFormat::R16G16Sint
                | PixelFormat::R16G16Snorm
                | PixelFormat::R8G8Unorm
                | PixelFormat::R8G8Snorm
                | PixelFormat::R8G8Sint
                | PixelFormat::R8G8Uint
                | PixelFormat::R32G32Uint
                | PixelFormat::R32Uint
                | PixelFormat::R32Sint
                | PixelFormat::G4R4Unorm
                | PixelFormat::D32Float
                | PixelFormat::D16Unorm
                | PixelFormat::X8D24Unorm
                | PixelFormat::S8Uint
                | PixelFormat::D24UnormS8Uint
                | PixelFormat::S8UintD24Unorm
                | PixelFormat::D32FloatS8Uint
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture_cache::format_lookup_table::PixelFormat;
    use common::slot_vector::SlotId;

    #[test]
    fn image_view_base_keeps_constructing_after_incompatible_format_assert_like_upstream() {
        let image_info = ImageInfo {
            format: PixelFormat::R8Unorm,
            image_type: ImageType::E2D,
            ..ImageInfo::default()
        };
        let view_info = ImageViewInfo {
            format: PixelFormat::R32G32B32A32Float,
            view_type: ImageViewType::E2D,
            ..ImageViewInfo::default()
        };

        let view = ImageViewBase::new(&view_info, &image_info, SlotId { index: 1 }, 0x1000);
        assert_eq!(view.format, PixelFormat::R32G32B32A32Float);
    }

    #[test]
    fn image_view_base_buffer_constructor_keeps_constructing_after_assert_like_upstream() {
        let image_info = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            ..ImageInfo::default()
        };
        let view_info = ImageViewInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            view_type: ImageViewType::E2D,
            ..ImageViewInfo::default()
        };

        let view = ImageViewBase::new_buffer(&image_info, &view_info, 0x1000);
        assert!(view.is_buffer());
    }
}
