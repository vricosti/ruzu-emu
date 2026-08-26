// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `shader_recompiler/shader_info.h`
//!
//! Full shader information collected during translation passes.
//! The existing `ir::program::ShaderInfo` is a simplified version;
//! this module provides the upstream-faithful `Info` struct with all
//! descriptor types and usage flags.

use super::varying_state::VaryingState;
use std::collections::BTreeMap;

/// Constant that can replace a cbuf access (for HLE macro state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum ReplaceConstant {
    BaseInstance = 0,
    BaseVertex = 1,
    DrawID = 2,
}

/// Texture type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum TextureType {
    Color1D = 0,
    ColorArray1D = 1,
    Color2D = 2,
    ColorArray2D = 3,
    Color3D = 4,
    ColorCube = 5,
    ColorArrayCube = 6,
    Buffer = 7,
    Color2DRect = 8,
}

/// Number of texture types.
pub const NUM_TEXTURE_TYPES: u32 = 9;

impl TextureType {
    /// Decode the 3-bit `texture_type` field of `TextureInstInfo`. Values
    /// > 8 are clamped to `Color2D` (the safest default), matching upstream's
    /// behaviour when a malformed shader produces an out-of-range field.
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => TextureType::Color1D,
            1 => TextureType::ColorArray1D,
            2 => TextureType::Color2D,
            3 => TextureType::ColorArray2D,
            4 => TextureType::Color3D,
            5 => TextureType::ColorCube,
            6 => TextureType::ColorArrayCube,
            7 => TextureType::Buffer,
            8 => TextureType::Color2DRect,
            _ => TextureType::Color2D,
        }
    }
}

/// Texture pixel format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TexturePixelFormat {
    A8B8G8R8Unorm,
    A8B8G8R8Snorm,
    A8B8G8R8Sint,
    A8B8G8R8Uint,
    R5G6B5Unorm,
    B5G6R5Unorm,
    A1R5G5B5Unorm,
    A2B10G10R10Unorm,
    A2B10G10R10Uint,
    A2R10G10B10Unorm,
    A1B5G5R5Unorm,
    A5B5G5R1Unorm,
    R8Unorm,
    R8Snorm,
    R8Sint,
    R8Uint,
    R16G16B16A16Float,
    R16G16B16A16Unorm,
    R16G16B16A16Snorm,
    R16G16B16A16Sint,
    R16G16B16A16Uint,
    B10G11R11Float,
    R32G32B32A32Uint,
    Bc1RgbaUnorm,
    Bc2Unorm,
    Bc3Unorm,
    Bc4Unorm,
    Bc4Snorm,
    Bc5Unorm,
    Bc5Snorm,
    Bc7Unorm,
    Bc6hUfloat,
    Bc6hSfloat,
    Astc2d4x4Unorm,
    B8G8R8A8Unorm,
    R32G32B32A32Float,
    R32G32B32A32Sint,
    R32G32Float,
    R32G32Sint,
    R32Float,
    R16Float,
    R16Unorm,
    R16Snorm,
    R16Uint,
    R16Sint,
    R16G16Unorm,
    R16G16Float,
    R16G16Uint,
    R16G16Sint,
    R16G16Snorm,
    R32G32B32Float,
    A8B8G8R8Srgb,
    R8G8Unorm,
    R8G8Snorm,
    R8G8Sint,
    R8G8Uint,
    R32G32Uint,
    R16G16B16X16Float,
    R32Uint,
    R32Sint,
    Astc2d8x8Unorm,
    Astc2d8x5Unorm,
    Astc2d5x4Unorm,
    B8G8R8A8Srgb,
    Bc1RgbaSrgb,
    Bc2Srgb,
    Bc3Srgb,
    Bc7Srgb,
    A4B4G4R4Unorm,
    G4R4Unorm,
    Astc2d4x4Srgb,
    Astc2d8x8Srgb,
    Astc2d8x5Srgb,
    Astc2d5x4Srgb,
    Astc2d5x5Unorm,
    Astc2d5x5Srgb,
    Astc2d10x8Unorm,
    Astc2d10x8Srgb,
    Astc2d6x6Unorm,
    Astc2d6x6Srgb,
    Astc2d10x6Unorm,
    Astc2d10x6Srgb,
    Astc2d10x5Unorm,
    Astc2d10x5Srgb,
    Astc2d10x10Unorm,
    Astc2d10x10Srgb,
    Astc2d12x10Unorm,
    Astc2d12x10Srgb,
    Astc2d12x12Unorm,
    Astc2d12x12Srgb,
    Astc2d8x6Unorm,
    Astc2d8x6Srgb,
    Astc2d6x5Unorm,
    Astc2d6x5Srgb,
    E5B9G9R9Float,
    // ETC2 / EAC. These carry no meaning for the shader recompiler, but the
    // discriminants of this enum must stay identical to
    // `VideoCore::Surface::PixelFormat`: `convert_texture_pixel_format`
    // transmutes one into the other, so a variant missing here shifts every
    // depth format and yields an invalid discriminant.
    Etc2RgbUnorm,
    Etc2RgbaUnorm,
    Etc2RgbPtaUnorm,
    Etc2RgbSrgb,
    Etc2RgbaSrgb,
    Etc2RgbPtaSrgb,
    EacR11Unorm,
    EacR11Snorm,
    EacR11G11Unorm,
    EacR11G11Snorm,
    D32Float,
    D16Unorm,
    X8D24Unorm,
    S8Uint,
    D24UnormS8Uint,
    S8UintD24Unorm,
    D32FloatS8Uint,
}

/// Image format for typed image accesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum ImageFormat {
    Typeless = 0,
    R8Uint = 1,
    R8Sint = 2,
    R16Uint = 3,
    R16Sint = 4,
    R32Uint = 5,
    R32G32Uint = 6,
    R32G32B32A32Uint = 7,
}

impl Default for ImageFormat {
    fn default() -> Self {
        ImageFormat::Typeless
    }
}

impl ImageFormat {
    /// Decode the 3-bit `image_format` field of `TextureInstInfo`.
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => ImageFormat::Typeless,
            1 => ImageFormat::R8Uint,
            2 => ImageFormat::R8Sint,
            3 => ImageFormat::R16Uint,
            4 => ImageFormat::R16Sint,
            5 => ImageFormat::R32Uint,
            6 => ImageFormat::R32G32Uint,
            7 => ImageFormat::R32G32B32A32Uint,
            _ => ImageFormat::Typeless,
        }
    }
}

/// Interpolation mode for fragment shader inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Interpolation {
    Smooth,
    Flat,
    NoPerspective,
}

impl Default for Interpolation {
    fn default() -> Self {
        Interpolation::Smooth
    }
}

/// Constant buffer descriptor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConstantBufferDescriptor {
    pub index: u32,
    pub count: u32,
}

/// Storage buffer descriptor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct StorageBufferDescriptor {
    pub cbuf_index: u32,
    pub cbuf_offset: u32,
    pub count: u32,
    pub is_written: bool,
}

/// Texture buffer descriptor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextureBufferDescriptor {
    pub has_secondary: bool,
    pub cbuf_index: u32,
    pub cbuf_offset: u32,
    pub shift_left: u32,
    pub secondary_cbuf_index: u32,
    pub secondary_cbuf_offset: u32,
    pub secondary_shift_left: u32,
    pub count: u32,
    pub size_shift: u32,
}

/// Image buffer descriptor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ImageBufferDescriptor {
    pub format: ImageFormat,
    pub is_written: bool,
    pub is_read: bool,
    pub is_integer: bool,
    pub cbuf_index: u32,
    pub cbuf_offset: u32,
    pub count: u32,
    pub size_shift: u32,
}

/// Texture descriptor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextureDescriptor {
    pub texture_type: TextureType,
    pub is_depth: bool,
    pub is_multisample: bool,
    pub is_integer: bool,
    pub has_secondary: bool,
    pub cbuf_index: u32,
    pub cbuf_offset: u32,
    pub shift_left: u32,
    pub secondary_cbuf_index: u32,
    pub secondary_cbuf_offset: u32,
    pub secondary_shift_left: u32,
    pub count: u32,
    pub size_shift: u32,
}

/// Image descriptor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ImageDescriptor {
    pub texture_type: TextureType,
    pub format: ImageFormat,
    pub is_written: bool,
    pub is_read: bool,
    pub is_integer: bool,
    pub cbuf_index: u32,
    pub cbuf_offset: u32,
    pub count: u32,
    pub size_shift: u32,
}

/// Full shader information matching upstream `Info` struct.
#[derive(Debug, Clone, Default)]
pub struct Info {
    pub uses_workgroup_id: bool,
    pub uses_local_invocation_id: bool,
    pub uses_invocation_id: bool,
    pub uses_invocation_info: bool,
    pub uses_sample_id: bool,
    pub uses_is_helper_invocation: bool,
    pub uses_subgroup_invocation_id: bool,
    pub uses_subgroup_shuffles: bool,
    pub uses_patches: [bool; 30],

    pub interpolation: [Interpolation; 32],
    pub loads: VaryingState,
    pub stores: VaryingState,
    pub passthrough: VaryingState,

    pub legacy_stores_mapping: BTreeMap<u64, u64>,

    pub loads_indexed_attributes: bool,

    pub stores_frag_color: [bool; 8],
    pub stores_sample_mask: bool,
    pub stores_frag_depth: bool,

    pub stores_tess_level_outer: bool,
    pub stores_tess_level_inner: bool,

    pub stores_indexed_attributes: bool,

    pub stores_global_memory: bool,
    pub uses_local_memory: bool,

    pub uses_fp16: bool,
    pub uses_fp64: bool,
    pub uses_fp16_denorms_flush: bool,
    pub uses_fp16_denorms_preserve: bool,
    pub uses_fp32_denorms_flush: bool,
    pub uses_fp32_denorms_preserve: bool,
    pub uses_int8: bool,
    pub uses_int16: bool,
    pub uses_int64: bool,
    pub uses_image_1d: bool,
    pub uses_sampled_1d: bool,
    pub uses_sparse_residency: bool,
    pub uses_demote_to_helper_invocation: bool,
    pub uses_subgroup_vote: bool,
    pub uses_subgroup_mask: bool,
    pub uses_fswzadd: bool,
    pub uses_derivatives: bool,
    pub uses_typeless_image_reads: bool,
    pub uses_typeless_image_writes: bool,
    pub uses_image_buffers: bool,
    pub uses_shared_increment: bool,
    pub uses_shared_decrement: bool,
    pub uses_global_increment: bool,
    pub uses_global_decrement: bool,
    pub uses_atomic_f32_add: bool,
    pub uses_atomic_f16x2_add: bool,
    pub uses_atomic_f16x2_min: bool,
    pub uses_atomic_f16x2_max: bool,
    pub uses_atomic_f32x2_add: bool,
    pub uses_atomic_f32x2_min: bool,
    pub uses_atomic_f32x2_max: bool,
    pub uses_atomic_s32_min: bool,
    pub uses_atomic_s32_max: bool,
    pub uses_int64_bit_atomics: bool,
    pub uses_global_memory: bool,
    pub uses_atomic_image_u32: bool,
    pub uses_shadow_lod: bool,
    pub uses_rescaling_uniform: bool,
    pub uses_cbuf_indirect: bool,
    pub uses_render_area: bool,

    /// Bitmask of IR::Type values used for constant buffer accesses.
    pub used_constant_buffer_types: u32,
    /// Bitmask of IR::Type values used for storage buffer accesses.
    pub used_storage_buffer_types: u32,
    /// Bitmask of IR::Type values used for indirect cbuf accesses.
    pub used_indirect_cbuf_types: u32,

    pub constant_buffer_mask: u32,
    pub constant_buffer_used_sizes: [u32; Self::MAX_CBUFS],

    // The previous Rust-port-only compatibility fields
    // (loads_generics, stores_generics, loads_position, stores_position,
    // local_memory_size, shared_memory_size) have been removed. The
    // translator and backends now use `VaryingState`-based loads/stores
    // and `Program::local_memory_size` / `Program::shared_memory_size`
    // matching upstream.
    pub nvn_buffer_base: u32,
    pub nvn_buffer_used: u16,

    pub requires_layer_emulation: bool,
    /// The attribute used to emulate gl_Layer when not natively supported.
    pub emulated_layer: u64,

    pub used_clip_distances: u32,

    pub constant_buffer_descriptors: Vec<ConstantBufferDescriptor>,
    pub storage_buffers_descriptors: Vec<StorageBufferDescriptor>,
    pub texture_buffer_descriptors: Vec<TextureBufferDescriptor>,
    pub image_buffer_descriptors: Vec<ImageBufferDescriptor>,
    pub texture_descriptors: Vec<TextureDescriptor>,
    pub image_descriptors: Vec<ImageDescriptor>,
}

impl Info {
    pub const MAX_INDIRECT_CBUFS: usize = 14;
    pub const MAX_CBUFS: usize = 18;
    pub const MAX_SSBOS: usize = 32;
}

/// Count total descriptors (sum of all `count` fields).
pub fn num_descriptors<T: HasCount>(descriptors: &[T]) -> u32 {
    descriptors.iter().fold(0u32, |num, descriptor| {
        num.wrapping_add(descriptor.descriptor_count())
    })
}

/// Trait for descriptors that have a `count` field.
pub trait HasCount {
    fn descriptor_count(&self) -> u32;
}

impl HasCount for ConstantBufferDescriptor {
    fn descriptor_count(&self) -> u32 {
        self.count
    }
}

impl HasCount for StorageBufferDescriptor {
    fn descriptor_count(&self) -> u32 {
        self.count
    }
}

impl HasCount for TextureBufferDescriptor {
    fn descriptor_count(&self) -> u32 {
        self.count
    }
}

impl HasCount for ImageBufferDescriptor {
    fn descriptor_count(&self) -> u32 {
        self.count
    }
}

impl HasCount for TextureDescriptor {
    fn descriptor_count(&self) -> u32 {
        self.count
    }
}

impl HasCount for ImageDescriptor {
    fn descriptor_count(&self) -> u32 {
        self.count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn num_descriptors_preserves_upstream_u32_wrapping() {
        let descriptors = [
            StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: u32::MAX,
                is_written: false,
            },
            StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 2,
                is_written: false,
            },
        ];

        assert_eq!(num_descriptors(&descriptors), 1);
    }
}
