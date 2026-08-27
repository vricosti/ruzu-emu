// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/texture_cache/types.h
//!
//! Fundamental types, constants, and data structures used throughout the
//! texture cache subsystem.

use common::slot_vector::SlotId;

// ── Constants ──────────────────────────────────────────────────────────

pub const NUM_RT: usize = 8;
pub const MAX_MIP_LEVELS: usize = 16;

pub const CORRUPT_ID: SlotId = SlotId { index: 0xffff_fffe };

// ── Slot‑ID type aliases ───────────────────────────────────────────────

pub type ImageId = SlotId;
pub type ImageMapId = SlotId;
pub type ImageViewId = SlotId;
pub type ImageAllocId = SlotId;
pub type SamplerId = SlotId;
pub type FramebufferId = SlotId;

/// Fake image ID for null image views
pub const NULL_IMAGE_ID: ImageId = SlotId { index: 0 };
/// Image view ID for null descriptors
pub const NULL_IMAGE_VIEW_ID: ImageViewId = SlotId { index: 0 };
/// Sampler ID for bugged sampler ids
pub const NULL_SAMPLER_ID: SamplerId = SlotId { index: 0 };

// ── Enumerations ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum ImageType {
    E1D = 0,
    E2D = 1,
    E3D = 2,
    Linear = 3,
    Buffer = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum ImageViewType {
    E1D = 0,
    E2D = 1,
    Cube = 2,
    E3D = 3,
    E1DArray = 4,
    E2DArray = 5,
    CubeArray = 6,
    Rect = 7,
    Buffer = 8,
}

pub const NUM_IMAGE_VIEW_TYPES: usize = 9;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct RelaxedOptions: u32 {
        const SIZE              = 1 << 0;
        const FORMAT            = 1 << 1;
        const SAMPLES           = 1 << 2;
        const FORCE_BROKEN_VIEWS = 1 << 3;
    }
}

// ── Geometry primitives ────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub struct Offset2D {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub struct Offset3D {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub struct Region2D {
    pub start: Offset2D,
    pub end: Offset2D,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub struct Extent2D {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub struct Extent3D {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
}

// ── Subresource descriptors ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct SubresourceLayers {
    pub base_level: i32,
    pub base_layer: i32,
    pub num_layers: i32,
}

impl Default for SubresourceLayers {
    fn default() -> Self {
        Self {
            base_level: 0,
            base_layer: 0,
            num_layers: 1,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub struct SubresourceBase {
    pub level: i32,
    pub layer: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub struct SubresourceExtent {
    pub levels: i32,
    pub layers: i32,
}

impl Default for SubresourceExtent {
    fn default() -> Self {
        Self {
            levels: 1,
            layers: 1,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub struct SubresourceRange {
    pub base: SubresourceBase,
    pub extent: SubresourceExtent,
}

// ── Copy / transfer descriptors ────────────────────────────────────────

#[derive(Debug, Default, Clone, Copy)]
#[repr(C)]
pub struct ImageCopy {
    pub src_subresource: SubresourceLayers,
    pub dst_subresource: SubresourceLayers,
    pub src_offset: Offset3D,
    pub dst_offset: Offset3D,
    pub extent: Extent3D,
}

#[derive(Debug, Default, Clone, Copy)]
#[repr(C)]
pub struct BufferImageCopy {
    pub buffer_offset: usize,
    pub buffer_size: usize,
    pub buffer_row_length: u32,
    pub buffer_image_height: u32,
    pub image_subresource: SubresourceLayers,
    pub image_offset: Offset3D,
    pub image_extent: Extent3D,
}

#[derive(Debug, Default, Clone, Copy)]
#[repr(C)]
pub struct BufferCopy {
    pub src_offset: u64,
    pub dst_offset: u64,
    pub size: usize,
}

#[derive(Debug, Default, Clone, Copy)]
#[repr(C)]
pub struct SwizzleParameters {
    pub num_tiles: Extent3D,
    pub block: Extent3D,
    pub buffer_offset: usize,
    pub level: i32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    #[test]
    fn constants_and_discriminants_match_upstream() {
        assert_eq!(NUM_RT, 8);
        assert_eq!(MAX_MIP_LEVELS, 16);
        assert_eq!(CORRUPT_ID.index, 0xffff_fffe);
        assert_eq!(NUM_IMAGE_VIEW_TYPES, 9);
        assert_eq!(ImageType::E1D as u32, 0);
        assert_eq!(ImageType::Buffer as u32, 4);
        assert_eq!(ImageViewType::E1D as u32, 0);
        assert_eq!(ImageViewType::Buffer as u32, 8);
        assert_eq!(RelaxedOptions::all().bits(), 0x0f);
    }

    #[test]
    fn default_subresources_match_upstream_member_initializers() {
        assert_eq!(
            SubresourceLayers::default(),
            SubresourceLayers {
                base_level: 0,
                base_layer: 0,
                num_layers: 1,
            }
        );
        assert_eq!(
            SubresourceBase::default(),
            SubresourceBase { level: 0, layer: 0 }
        );
        assert_eq!(
            SubresourceExtent::default(),
            SubresourceExtent {
                levels: 1,
                layers: 1,
            }
        );
    }

    #[test]
    fn fixed_copy_and_geometry_layouts_match_upstream() {
        assert_eq!(size_of::<Offset2D>(), 8);
        assert_eq!(size_of::<Offset3D>(), 12);
        assert_eq!(size_of::<Region2D>(), 16);
        assert_eq!(size_of::<Extent2D>(), 8);
        assert_eq!(size_of::<Extent3D>(), 12);
        assert_eq!(size_of::<SubresourceLayers>(), 12);
        assert_eq!(size_of::<SubresourceBase>(), 8);
        assert_eq!(size_of::<SubresourceExtent>(), 8);
        assert_eq!(size_of::<SubresourceRange>(), 16);
        assert_eq!(size_of::<ImageCopy>(), 60);
        assert_eq!(align_of::<ImageCopy>(), 4);
        assert_eq!(offset_of!(ImageCopy, src_subresource), 0);
        assert_eq!(offset_of!(ImageCopy, dst_subresource), 12);
        assert_eq!(offset_of!(ImageCopy, src_offset), 24);
        assert_eq!(offset_of!(ImageCopy, dst_offset), 36);
        assert_eq!(offset_of!(ImageCopy, extent), 48);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn pointer_sized_copy_layouts_match_upstream_64_bit_abi() {
        assert_eq!(size_of::<BufferImageCopy>(), 64);
        assert_eq!(align_of::<BufferImageCopy>(), 8);
        assert_eq!(offset_of!(BufferImageCopy, buffer_offset), 0);
        assert_eq!(offset_of!(BufferImageCopy, buffer_size), 8);
        assert_eq!(offset_of!(BufferImageCopy, buffer_row_length), 16);
        assert_eq!(offset_of!(BufferImageCopy, buffer_image_height), 20);
        assert_eq!(offset_of!(BufferImageCopy, image_subresource), 24);
        assert_eq!(offset_of!(BufferImageCopy, image_offset), 36);
        assert_eq!(offset_of!(BufferImageCopy, image_extent), 48);

        assert_eq!(size_of::<BufferCopy>(), 24);
        assert_eq!(offset_of!(BufferCopy, src_offset), 0);
        assert_eq!(offset_of!(BufferCopy, dst_offset), 8);
        assert_eq!(offset_of!(BufferCopy, size), 16);

        assert_eq!(size_of::<SwizzleParameters>(), 40);
        assert_eq!(offset_of!(SwizzleParameters, num_tiles), 0);
        assert_eq!(offset_of!(SwizzleParameters, block), 12);
        assert_eq!(offset_of!(SwizzleParameters, buffer_offset), 24);
        assert_eq!(offset_of!(SwizzleParameters, level), 32);
    }

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn pointer_sized_copy_layouts_match_upstream_32_bit_abi() {
        assert_eq!(size_of::<BufferImageCopy>(), 52);
        assert_eq!(size_of::<BufferCopy>(), 20);
        assert_eq!(size_of::<SwizzleParameters>(), 32);
    }
}
