// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/texture_cache/image_base.h and image_base.cpp
//!
//! Defines `ImageBase`, `ImageMapView`, `ImageAllocBase`, and the
//! `add_image_alias` helper.

use super::image_info::ImageInfo;
use super::image_view_info::ImageViewInfo;
use super::types::*;
use super::util;
use crate::surface;
use common::div_ceil::div_ceil_u32;
use smallvec::SmallVec;

// ── Type aliases matching upstream ─────────────────────────────────────

/// GPU virtual address (u64).
pub type GPUVAddr = u64;
/// CPU virtual address (u64).
pub type VAddr = u64;

// ── ImageFlagBits ──────────────────────────────────────────────────────

bitflags::bitflags! {
    /// Flags associated with a cached image.
    ///
    /// Port of `VideoCommon::ImageFlagBits`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ImageFlagBits: u32 {
        const ACCELERATED_UPLOAD   = 1 << 0;
        const CONVERTED            = 1 << 1;
        const CPU_MODIFIED         = 1 << 2;
        const GPU_MODIFIED         = 1 << 3;
        const TRACKED              = 1 << 4;
        const STRONG               = 1 << 5;
        const REGISTERED           = 1 << 6;
        const PICKED               = 1 << 7;
        const REMAPPED             = 1 << 8;
        const SPARSE               = 1 << 9;
        const BAD_OVERLAP          = 1 << 10;
        const ALIAS                = 1 << 11;
        const COSTLY_LOAD          = 1 << 12;
        const RESCALED             = 1 << 13;
        const CHECKING_RESCALABLE  = 1 << 14;
        const IS_RESCALABLE        = 1 << 15;
        const ASYNCHRONOUS_DECODE  = 1 << 16;
        const IS_DECODING          = 1 << 17;
    }
}

// ── AliasedImage ───────────────────────────────────────────────────────

/// An aliased image with associated copy descriptors.
///
/// Port of `VideoCommon::AliasedImage`.
#[derive(Debug, Clone)]
pub struct AliasedImage {
    pub copies: Vec<ImageCopy>,
    pub id: ImageId,
}

// ── NullImageParams ────────────────────────────────────────────────────

/// Tag type for constructing a null/sentinel image.
pub struct NullImageParams;

// ── ImageBase ──────────────────────────────────────────────────────────

/// Base data for a cached GPU image.
///
/// Port of `VideoCommon::ImageBase`.
#[derive(Debug, Clone)]
pub struct ImageBase {
    pub info: ImageInfo,

    pub guest_size_bytes: u32,
    pub unswizzled_size_bytes: u32,
    pub converted_size_bytes: u32,
    pub scale_rating: u32,
    pub scale_tick: u64,
    pub has_scaled: bool,

    pub channel: usize,

    pub flags: ImageFlagBits,

    pub gpu_addr: GPUVAddr,
    pub cpu_addr: VAddr,
    pub cpu_addr_end: VAddr,

    pub modification_tick: u64,
    pub lru_index: usize,

    pub mip_level_offsets: [u32; MAX_MIP_LEVELS],

    pub image_view_infos: Vec<ImageViewInfo>,
    pub image_view_ids: Vec<ImageViewId>,

    pub slice_offsets: SmallVec<[u32; 16]>,
    pub slice_subresources: SmallVec<[SubresourceBase; 16]>,

    pub aliased_images: Vec<AliasedImage>,
    pub overlapping_images: Vec<ImageId>,
    pub map_view_id: ImageMapId,
}

impl ImageBase {
    /// Construct from image info and addresses.
    ///
    /// Port of `ImageBase::ImageBase(const ImageInfo&, GPUVAddr, VAddr)`.
    pub fn new(info: ImageInfo, gpu_addr: GPUVAddr, cpu_addr: VAddr) -> Self {
        let guest_size_bytes = util::calculate_guest_size_in_bytes(&info);
        let unswizzled_size_bytes = util::calculate_unswizzled_size_bytes(&info);
        let converted_size_bytes = util::calculate_converted_size_bytes(&info);
        let mip_level_offsets = util::calculate_mip_level_offsets(&info);
        let slice_offsets = if info.image_type == ImageType::E3D {
            util::calculate_slice_offsets(&info)
        } else {
            SmallVec::new()
        };
        let slice_subresources = if info.image_type == ImageType::E3D {
            util::calculate_slice_subresources(&info)
        } else {
            SmallVec::new()
        };
        Self {
            guest_size_bytes,
            unswizzled_size_bytes,
            converted_size_bytes,
            scale_rating: 0,
            scale_tick: 0,
            has_scaled: false,
            channel: 0,
            flags: ImageFlagBits::CPU_MODIFIED,
            gpu_addr,
            cpu_addr,
            cpu_addr_end: cpu_addr.wrapping_add(guest_size_bytes as u64),
            modification_tick: 0,
            lru_index: usize::MAX,
            mip_level_offsets,
            image_view_infos: Vec::new(),
            image_view_ids: Vec::new(),
            slice_offsets,
            slice_subresources,
            aliased_images: Vec::new(),
            overlapping_images: Vec::new(),
            map_view_id: ImageMapId::default(),
            info,
        }
    }

    /// Construct a null/sentinel image.
    ///
    /// Port of `ImageBase::ImageBase(const NullImageParams&)`.
    pub fn null(_: NullImageParams) -> Self {
        Self {
            info: ImageInfo::default(),
            guest_size_bytes: 0,
            unswizzled_size_bytes: 0,
            converted_size_bytes: 0,
            scale_rating: 0,
            scale_tick: 0,
            has_scaled: false,
            channel: 0,
            flags: ImageFlagBits::CPU_MODIFIED,
            gpu_addr: 0,
            cpu_addr: 0,
            cpu_addr_end: 0,
            modification_tick: 0,
            lru_index: usize::MAX,
            mip_level_offsets: [0; MAX_MIP_LEVELS],
            image_view_infos: Vec::new(),
            image_view_ids: Vec::new(),
            slice_offsets: SmallVec::new(),
            slice_subresources: SmallVec::new(),
            aliased_images: Vec::new(),
            overlapping_images: Vec::new(),
            map_view_id: ImageMapId::default(),
        }
    }

    /// Try to find the base subresource at `other_addr`.
    ///
    /// Port of `ImageBase::TryFindBase`.
    pub fn try_find_base(&self, other_addr: GPUVAddr) -> Option<SubresourceBase> {
        if other_addr < self.gpu_addr {
            return None;
        }
        let diff = (other_addr - self.gpu_addr) as u32;
        if diff > self.guest_size_bytes {
            return None;
        }
        if self.info.image_type != ImageType::E3D {
            let (layer, mip_offset) = layer_mip_offset(diff as i32, self.info.layer_stride);
            let levels = self.info.resources.levels as usize;
            let it = self.mip_level_offsets[..levels]
                .iter()
                .position(|&o| o == mip_offset as u32);
            match it {
                Some(level) if layer <= self.info.resources.layers => Some(SubresourceBase {
                    level: level as i32,
                    layer,
                }),
                _ => None,
            }
        } else {
            let it = self.slice_offsets.iter().position(|&o| o == diff);
            it.map(|idx| self.slice_subresources[idx])
        }
    }

    /// Find an existing view matching `view_info`.
    ///
    /// Port of `ImageBase::FindView`.
    pub fn find_view(&self, view_info: &ImageViewInfo) -> ImageViewId {
        self.image_view_infos
            .iter()
            .position(|i| i == view_info)
            .map(|idx| self.image_view_ids[idx])
            .unwrap_or_default()
    }

    /// Insert a new view.
    ///
    /// Port of `ImageBase::InsertView`.
    pub fn insert_view(&mut self, view_info: ImageViewInfo, image_view_id: ImageViewId) {
        self.image_view_infos.push(view_info);
        self.image_view_ids.push(image_view_id);
    }

    /// Whether it is safe to download this image to the CPU.
    ///
    /// Port of `ImageBase::IsSafeDownload`.
    pub fn is_safe_download(&self) -> bool {
        if !self.flags.contains(ImageFlagBits::GPU_MODIFIED) {
            return false;
        }
        if self.flags.contains(ImageFlagBits::CPU_MODIFIED) {
            return false;
        }
        if self.info.num_samples > 1 {
            log::warn!("MSAA image downloads are not implemented");
            return false;
        }
        true
    }

    /// Whether this image has ever been scaled.
    ///
    /// Port of `ImageBase::HasScaled`.
    pub fn has_scaled(&self) -> bool {
        self.has_scaled
    }

    /// Whether this image overlaps a CPU address range.
    pub fn overlaps(&self, overlap_cpu_addr: VAddr, overlap_size: usize) -> bool {
        let overlap_end = overlap_cpu_addr.wrapping_add(overlap_size as u64);
        self.cpu_addr < overlap_end && overlap_cpu_addr < self.cpu_addr_end
    }

    /// Whether this image overlaps a GPU address range.
    pub fn overlaps_gpu(&self, overlap_gpu_addr: GPUVAddr, overlap_size: usize) -> bool {
        let overlap_end = overlap_gpu_addr.wrapping_add(overlap_size as u64);
        let gpu_addr_end = self.gpu_addr.wrapping_add(self.guest_size_bytes as u64);
        self.gpu_addr < overlap_end && overlap_gpu_addr < gpu_addr_end
    }

    /// Port of `ImageBase::CheckBadOverlapState`.
    pub fn check_bad_overlap_state(&mut self) {
        if !self.flags.contains(ImageFlagBits::BAD_OVERLAP) {
            return;
        }
        if !self.overlapping_images.is_empty() {
            return;
        }
        self.flags.remove(ImageFlagBits::BAD_OVERLAP);
    }

    /// Port of `ImageBase::CheckAliasState`.
    pub fn check_alias_state(&mut self) {
        if !self.flags.contains(ImageFlagBits::ALIAS) {
            return;
        }
        if !self.aliased_images.is_empty() {
            return;
        }
        self.flags.remove(ImageFlagBits::ALIAS);
    }
}

// ── ImageMapView ───────────────────────────────────────────────────────

/// Describes a mapped region of an image in the GPU address space.
///
/// Port of `VideoCommon::ImageMapView`.
#[derive(Debug, Clone)]
pub struct ImageMapView {
    pub gpu_addr: GPUVAddr,
    pub cpu_addr: VAddr,
    pub size: usize,
    pub image_id: ImageId,
    pub picked: bool,
}

impl ImageMapView {
    pub fn new(gpu_addr: GPUVAddr, cpu_addr: VAddr, size: usize, image_id: ImageId) -> Self {
        Self {
            gpu_addr,
            cpu_addr,
            size,
            image_id,
            picked: false,
        }
    }

    pub fn overlaps(&self, overlap_cpu_addr: VAddr, overlap_size: usize) -> bool {
        let overlap_end = overlap_cpu_addr.wrapping_add(overlap_size as u64);
        let cpu_addr_end = self.cpu_addr.wrapping_add(self.size as u64);
        self.cpu_addr < overlap_end && overlap_cpu_addr < cpu_addr_end
    }

    pub fn overlaps_gpu(&self, overlap_gpu_addr: GPUVAddr, overlap_size: usize) -> bool {
        let overlap_end = overlap_gpu_addr.wrapping_add(overlap_size as u64);
        let gpu_addr_end = self.gpu_addr.wrapping_add(self.size as u64);
        self.gpu_addr < overlap_end && overlap_gpu_addr < gpu_addr_end
    }
}

// ── ImageAllocBase ─────────────────────────────────────────────────────

/// Base allocation record for an image.
///
/// Port of `VideoCommon::ImageAllocBase`.
#[derive(Debug, Clone, Default)]
pub struct ImageAllocBase {
    pub images: Vec<ImageId>,
}

// ── Private helpers ────────────────────────────────────────────────────

/// Returns (base_layer, mip_offset) from a byte offset and layer stride.
fn layer_mip_offset(diff: i32, layer_stride: u32) -> (i32, i32) {
    if layer_stride == 0 {
        (0, diff)
    } else {
        let diff = diff as u32;
        ((diff / layer_stride) as i32, (diff % layer_stride) as i32)
    }
}

// ── add_image_alias ────────────────────────────────────────────────────

/// Validate subresource layers against image info.
fn validate_layers(layers: &SubresourceLayers, info: &ImageInfo) -> bool {
    layers.base_level < info.resources.levels
        && layers.base_layer + layers.num_layers <= info.resources.layers
}

/// Validate a copy against source and destination image info.
fn validate_copy(copy: &ImageCopy, dst: &ImageInfo, src: &ImageInfo) -> bool {
    let src_size = util::mip_size(src.size, copy.src_subresource.base_level as u32);
    let dst_size = util::mip_size(dst.size, copy.dst_subresource.base_level as u32);
    if !validate_layers(&copy.src_subresource, src) {
        return false;
    }
    if !validate_layers(&copy.dst_subresource, dst) {
        return false;
    }
    if (copy.src_offset.x as u32).wrapping_add(copy.extent.width) > src_size.width
        || (copy.src_offset.y as u32).wrapping_add(copy.extent.height) > src_size.height
        || (copy.src_offset.z as u32).wrapping_add(copy.extent.depth) > src_size.depth
    {
        return false;
    }
    if (copy.dst_offset.x as u32).wrapping_add(copy.extent.width) > dst_size.width
        || (copy.dst_offset.y as u32).wrapping_add(copy.extent.height) > dst_size.height
        || (copy.dst_offset.z as u32).wrapping_add(copy.extent.depth) > dst_size.depth
    {
        return false;
    }
    true
}

/// Create bidirectional alias records between two images.
///
/// Port of `VideoCommon::AddImageAlias`.
pub fn add_image_alias(
    lhs: &mut ImageBase,
    rhs: &mut ImageBase,
    lhs_id: ImageId,
    rhs_id: ImageId,
) -> bool {
    let options = RelaxedOptions::SIZE | RelaxedOptions::FORMAT;
    assert!(lhs.info.image_type == rhs.info.image_type);

    let base = if lhs.info.image_type == ImageType::Linear {
        Some(SubresourceBase { level: 0, layer: 0 })
    } else {
        // Relaxed formats as option, broken_views/bgr won't matter
        util::find_subresource(&rhs.info, lhs, rhs.gpu_addr, options, false, true)
    };

    let base = match base {
        Some(b) => b,
        None => {
            log::error!("Image alias should have been flipped");
            return false;
        }
    };

    let lhs_format = lhs.info.format;
    let rhs_format = rhs.info.format;
    let lhs_block = Extent2D {
        width: surface::default_block_width(lhs_format),
        height: surface::default_block_height(lhs_format),
    };
    let rhs_block = Extent2D {
        width: surface::default_block_width(rhs_format),
        height: surface::default_block_height(rhs_format),
    };
    let is_lhs_compressed = lhs_block.width > 1 || lhs_block.height > 1;
    let is_rhs_compressed = rhs_block.width > 1 || rhs_block.height > 1;
    let lhs_mips = lhs.info.resources.levels;
    let rhs_mips = rhs.info.resources.levels;
    let num_mips = (lhs_mips - base.level).min(rhs_mips);

    let mut lhs_alias = AliasedImage {
        id: rhs_id,
        copies: Vec::with_capacity(num_mips as usize),
    };
    let mut rhs_alias = AliasedImage {
        id: lhs_id,
        copies: Vec::with_capacity(num_mips as usize),
    };

    for mip_level in 0..num_mips {
        let mut lhs_size = util::mip_size(lhs.info.size, (base.level + mip_level) as u32);
        let mut rhs_size = util::mip_size(rhs.info.size, mip_level as u32);
        if is_lhs_compressed {
            lhs_size.width = div_ceil_u32(lhs_size.width, lhs_block.width);
            lhs_size.height = div_ceil_u32(lhs_size.height, lhs_block.height);
        }
        if is_rhs_compressed {
            rhs_size.width = div_ceil_u32(rhs_size.width, rhs_block.width);
            rhs_size.height = div_ceil_u32(rhs_size.height, rhs_block.height);
        }
        let copy_size = Extent3D {
            width: lhs_size.width.min(rhs_size.width),
            height: lhs_size.height.min(rhs_size.height),
            depth: lhs_size.depth.min(rhs_size.depth),
        };
        if copy_size.width == 0 || copy_size.height == 0 {
            log::warn!("Copy size is smaller than block size. Mip cannot be aliased.");
            continue;
        }
        let is_lhs_3d = lhs.info.image_type == ImageType::E3D;
        let is_rhs_3d = rhs.info.image_type == ImageType::E3D;
        let lhs_offset = Offset3D { x: 0, y: 0, z: 0 };
        let rhs_offset = Offset3D {
            x: 0,
            y: 0,
            z: if is_rhs_3d { base.layer } else { 0 },
        };
        let lhs_layers = if is_lhs_3d {
            1
        } else {
            lhs.info.resources.layers - base.layer
        };
        let rhs_layers = if is_rhs_3d {
            1
        } else {
            rhs.info.resources.layers
        };
        let num_layers = lhs_layers.min(rhs_layers);

        let lhs_subresource = SubresourceLayers {
            base_level: mip_level,
            base_layer: 0,
            num_layers,
        };
        let rhs_subresource = SubresourceLayers {
            base_level: base.level + mip_level,
            base_layer: if is_rhs_3d { 0 } else { base.layer },
            num_layers,
        };

        let to_lhs_copy = ImageCopy {
            src_subresource: lhs_subresource,
            dst_subresource: rhs_subresource,
            src_offset: lhs_offset,
            dst_offset: rhs_offset,
            extent: copy_size,
        };
        let to_rhs_copy = ImageCopy {
            src_subresource: rhs_subresource,
            dst_subresource: lhs_subresource,
            src_offset: rhs_offset,
            dst_offset: lhs_offset,
            extent: copy_size,
        };

        debug_assert!(
            validate_copy(&to_lhs_copy, &lhs.info, &rhs.info),
            "Invalid RHS to LHS copy"
        );
        debug_assert!(
            validate_copy(&to_rhs_copy, &rhs.info, &lhs.info),
            "Invalid LHS to RHS copy"
        );

        lhs_alias.copies.push(to_lhs_copy);
        rhs_alias.copies.push(to_rhs_copy);
    }

    debug_assert!(lhs_alias.copies.is_empty() == rhs_alias.copies.is_empty());
    if lhs_alias.copies.is_empty() {
        return false;
    }
    lhs.aliased_images.push(lhs_alias);
    rhs.aliased_images.push(rhs_alias);
    lhs.flags.remove(ImageFlagBits::IS_RESCALABLE);
    rhs.flags.remove(ImageFlagBits::IS_RESCALABLE);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture_cache::format_lookup_table::PixelFormat;

    #[test]
    fn null_image_keeps_upstream_default_flags() {
        let mut image = ImageBase::null(NullImageParams);
        assert_eq!(image.flags, ImageFlagBits::CPU_MODIFIED);
        assert!(!image.has_scaled());

        image.has_scaled = true;
        assert!(image.has_scaled());
    }

    #[test]
    fn layer_mip_offset_uses_upstream_unsigned_arithmetic() {
        assert_eq!(layer_mip_offset(-1, 0x1000), (0x000f_ffff, 0x0fff));
        assert_eq!(layer_mip_offset(-1, 0), (0, -1));
    }

    #[test]
    fn msaa_download_is_not_safe() {
        let info = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            size: Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            },
            num_samples: 4,
            ..ImageInfo::default()
        };
        let mut image = ImageBase::new(info, 0x4000_0000, 0x5000_0000);
        image.flags.remove(ImageFlagBits::CPU_MODIFIED);
        image.flags.insert(ImageFlagBits::GPU_MODIFIED);

        assert!(!image.is_safe_download());
    }
}
