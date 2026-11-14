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
    pub indirect_offset: u32,
    pub write_extent: crate::storage_write_extent::StorageWriteExtent,
}

impl Default for StorageBufferDescriptor {
    fn default() -> Self {
        Self { cbuf_index: 0, cbuf_offset: 0, count: 1, is_written: false,
            indirect_offset: u32::MAX, write_extent: Default::default() }
    }
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
    /// Index in Info::handle_programs, or u32::MAX for a direct cbuf handle.
    pub handle_program: u32,
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
    pub handle_program: u32,
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
    pub handle_program: u32,
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
    pub handle_program: u32,
}

/// Task/mesh stage-buffer layout from Eglantine `shader_info.h`, revision
/// 900ab5a8251dcdb5eee42dd2837fa936ade15303 (absent from original Eden-NX2).
/// Each line names the attribute in a 128-byte group of 32 elements.
#[derive(Debug, Clone, Default)]
pub struct MeshInfo {
    pub payload_words: u32,
    pub max_vertices: u32,
    pub max_primitives: u32,
    pub topology: u32,
    // ArrayVec preserves boost::container::static_vector's inline, bounded storage.
    pub vertex_lines: arrayvec::ArrayVec<u32, 136>,
    pub primitive_lines: arrayvec::ArrayVec<u32, 136>,
}

impl MeshInfo {
    /// Vertices per primitive, also its byte count in the stage-buffer map region.
    pub fn primitive_vertices(&self) -> u32 {
        match self.topology {
            0 => 1,
            1 => 2,
            _ => 3,
        }
    }
}

/// Full shader information matching upstream `Info` struct.
#[derive(Debug, Clone, Default)]
pub struct Info {
    /// Upstream guest task/mesh classification, independent of a compute host backend.
    pub is_task: bool,
    pub is_mesh: bool,
    pub uses_workgroup_id: bool,
    pub uses_local_invocation_id: bool,
    pub uses_invocation_id: bool,
    pub uses_invocation_info: bool,
    pub uses_sample_id: bool,
    pub uses_is_helper_invocation: bool,
    pub uses_subgroup_invocation_id: bool,
    pub uses_subgroup_shuffles: bool,
    pub uses_quad_shuffles: bool,
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
    /// Set by ConditionalBarrierPass when a conditional barrier is removed.
    pub removed_conditional_barrier: bool,
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
    pub handle_programs: Vec<Vec<u32>>,
    pub image_descriptors: Vec<ImageDescriptor>,
}

impl Info {
    pub const MAX_INDIRECT_CBUFS: usize = 14;
    pub const MAX_CBUFS: usize = 18;
    pub const MAX_SSBOS: usize = 32;

    /// Whether a storage descriptor names a used entry of the driver table.
    /// The mask indexes guest table slots, not the compacted descriptor vector.
    /// Shared by the backend global-memory helpers to preserve that distinction.
    pub fn uses_nvn_storage_descriptor(&self, descriptor: &StorageBufferDescriptor) -> bool {
        if descriptor.cbuf_index != 0 {
            return false;
        }
        let Some(offset) = descriptor.cbuf_offset.checked_sub(self.nvn_buffer_base) else {
            return false;
        };
        // NVN descriptors contain a 64-bit address, 32-bit size, and padding.
        if offset % 0x10 != 0 {
            return false;
        }
        let slot = offset / 0x10;
        slot < u16::BITS && self.nvn_buffer_used & (1u16 << slot) != 0
    }
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

/// Stack operations used by draw-time texture-handle programs.
/// Upstream: shader_info.h, Fritchii c9ebb5c3af672a0472ca0811392f04fab10203ab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum HandleOp {
    Immediate, Cbuf, CbufDynamic, Storage,
    Add, Sub, Mul, And, Or, Xor, Shl, Shr, Sar, Neg, Not,
    UMin, UMax, SMin, SMax,
    SLt, SLe, SGt, SGe, ULt, ULe, UGt, UGe, Eq, Ne,
    LAnd, LOr, LXor, LNot, Select, Tee, Load,
    /// The element of the descriptor array being computed (see
    /// `TextureDescriptor::handle_program`). Lichii extension:
    /// `shader_recompiler/shader_info.h:438`.
    Index,
}

/// Evaluate a handle against current constant/storage buffer words.
/// Preserves upstream's zero-on-underflow/missing-operand behavior and bounded
/// stack/slots, including ignored pushes at capacity. Unknown opcodes and a
/// final stack size other than one return None. `element` is the descriptor
/// array element computed (`HandleOp::Index`), as Lichii's
/// `EvaluateHandleProgram` (shader_info.h:444) takes it.
pub fn evaluate_handle_program(
    code: &[u32],
    mut read_cbuf: impl FnMut(u32, u32) -> u32,
    mut read_storage: impl FnMut(u32, u32) -> u32,
    element: u32,
) -> Option<u32> {
    let mut stack = [0u32; 64];
    let mut slots = [0u32; 256];
    let mut size = 0usize;
    let pop = |stack: &[u32; 64], size: &mut usize| {
        if *size == 0 { 0 } else { *size -= 1; stack[*size] }
    };
    let push = |stack: &mut [u32; 64], size: &mut usize, value| {
        if *size < stack.len() { stack[*size] = value; *size += 1; }
    };
    let mut code = code.iter().copied();
    while let Some(op) = code.next() {
        let value = match op {
            x if x == HandleOp::Immediate as u32 => code.next().unwrap_or(0),
            x if x == HandleOp::Cbuf as u32 => {
                let index = code.next().unwrap_or(0);
                read_cbuf(index, code.next().unwrap_or(0))
            }
            x if x == HandleOp::CbufDynamic as u32 => {
                let index = code.next().unwrap_or(0);
                read_cbuf(index, pop(&stack, &mut size))
            }
            x if x == HandleOp::Storage as u32 => {
                let binding = code.next().unwrap_or(0);
                read_storage(binding, pop(&stack, &mut size))
            }
            x if x == HandleOp::Neg as u32 => pop(&stack, &mut size).wrapping_neg(),
            x if x == HandleOp::Not as u32 => !pop(&stack, &mut size),
            x if x == HandleOp::LNot as u32 => u32::from(pop(&stack, &mut size) == 0),
            x if x == HandleOp::Tee as u32 => {
                let slot = code.next().unwrap_or(0) as usize;
                if slot < slots.len() && size > 0 { slots[slot] = stack[size - 1]; }
                continue;
            }
            x if x == HandleOp::Load as u32 => {
                slots.get(code.next().unwrap_or(0) as usize).copied().unwrap_or(0)
            }
            x if x == HandleOp::Index as u32 => element,
            x if x == HandleOp::Select as u32 => {
                let false_value = pop(&stack, &mut size);
                let true_value = pop(&stack, &mut size);
                let condition = pop(&stack, &mut size);
                if condition != 0 { true_value } else { false_value }
            }
            _ => {
                let b = pop(&stack, &mut size);
                let a = pop(&stack, &mut size);
                let sa = a as i32;
                let sb = b as i32;
                match op {
                    x if x == HandleOp::Add as u32 => a.wrapping_add(b),
                    x if x == HandleOp::Sub as u32 => a.wrapping_sub(b),
                    x if x == HandleOp::Mul as u32 => a.wrapping_mul(b),
                    x if x == HandleOp::And as u32 => a & b,
                    x if x == HandleOp::Or as u32 => a | b,
                    x if x == HandleOp::Xor as u32 => a ^ b,
                    x if x == HandleOp::Shl as u32 => a.checked_shl(b).unwrap_or(0),
                    x if x == HandleOp::Shr as u32 => a.checked_shr(b).unwrap_or(0),
                    x if x == HandleOp::Sar as u32 => (sa >> b.min(31)) as u32,
                    x if x == HandleOp::UMin as u32 => a.min(b),
                    x if x == HandleOp::UMax as u32 => a.max(b),
                    x if x == HandleOp::SMin as u32 => sa.min(sb) as u32,
                    x if x == HandleOp::SMax as u32 => sa.max(sb) as u32,
                    x if x == HandleOp::SLt as u32 => u32::from(sa < sb),
                    x if x == HandleOp::SLe as u32 => u32::from(sa <= sb),
                    x if x == HandleOp::SGt as u32 => u32::from(sa > sb),
                    x if x == HandleOp::SGe as u32 => u32::from(sa >= sb),
                    x if x == HandleOp::ULt as u32 => u32::from(a < b),
                    x if x == HandleOp::ULe as u32 => u32::from(a <= b),
                    x if x == HandleOp::UGt as u32 => u32::from(a > b),
                    x if x == HandleOp::UGe as u32 => u32::from(a >= b),
                    x if x == HandleOp::Eq as u32 => u32::from(a == b),
                    x if x == HandleOp::Ne as u32 => u32::from(a != b),
                    x if x == HandleOp::LAnd as u32 => u32::from(a != 0 && b != 0),
                    x if x == HandleOp::LOr as u32 => u32::from(a != 0 || b != 0),
                    x if x == HandleOp::LXor as u32 => u32::from((a != 0) != (b != 0)),
                    _ => return None,
                }
            }
        };
        push(&mut stack, &mut size, value);
    }
    (size == 1).then_some(stack[0])
}

#[cfg(test)]
mod tests {
    #[test]
    fn nvn_membership_uses_guest_slots_not_descriptor_order() {
        let info = super::Info { nvn_buffer_base: 0x310, nvn_buffer_used: (1 << 2) | (1 << 5), ..Default::default() };
        for (cbuf_index, cbuf_offset, expected) in [
            (0, 0x330, true), (0, 0x360, true), (1, 0x330, false),
            (0, 0x310, false), (0, 0x331, false), (0, 0x300, false),
            (0, 0x410, false), (0, u32::MAX, false),
        ] {
            let descriptor = super::StorageBufferDescriptor { cbuf_index, cbuf_offset, count: 1, is_written: false, ..Default::default() };
            assert_eq!(info.uses_nvn_storage_descriptor(&descriptor), expected);
        }
    }
    use super::*;

    #[test]
    fn mesh_info_matches_upstream_topology_and_bounded_line_storage() {
        let mut mesh = MeshInfo::default();
        assert_eq!([mesh.payload_words, mesh.max_vertices, mesh.max_primitives, mesh.topology], [0; 4]);
        assert!(mesh.vertex_lines.is_empty());
        assert!(mesh.primitive_lines.is_empty());
        for (topology, vertices) in [(0, 1), (1, 2), (4, 3), (2, 3), (u32::MAX, 3)] {
            mesh.topology = topology;
            assert_eq!(mesh.primitive_vertices(), vertices);
        }
        for lines in [&mut mesh.vertex_lines, &mut mesh.primitive_lines] {
            for attribute in 0..136 { lines.push(attribute); }
            assert!(lines.try_push(136).is_err());
            assert_eq!(lines.len(), 136);
            assert_eq!(lines[135], 135);
        }
    }

    #[test]
    fn num_descriptors_preserves_upstream_u32_wrapping() {
        let descriptors = [
            StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: u32::MAX,
                is_written: false, ..Default::default() },
            StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 2,
                is_written: false, ..Default::default() },
        ];

        assert_eq!(num_descriptors(&descriptors), 1);
    }

    #[test]
    fn handle_program_reads_current_storage_contents_not_its_address() {
        use HandleOp::*;
        let code = [Cbuf as u32, 2, 16, Immediate as u32, 4, Add as u32,
                    Storage as u32, 3];
        for current in [0x1234, 0x5678] {
            assert_eq!(evaluate_handle_program(&code, |index, offset| {
                assert_eq!((index, offset), (2, 16));
                24
            }, |binding, offset| {
                assert_eq!((binding, offset), (3, 28));
                current
            }, 0), Some(current));
        }
        assert_eq!(evaluate_handle_program(&[Immediate as u32, 12, CbufDynamic as u32, 4],
            |index, offset| { assert_eq!((index, offset), (4, 12)); 7 },
            |_, _| panic!("unexpected storage read"), 0), Some(7));
    }

    #[test]
    fn handle_program_arithmetic_bit_patterns_and_operand_order() {
        use HandleOp::*;
        for (op, a, b, expected) in [
            (Add, u32::MAX, 2, 1), (Sub, 2, 3, u32::MAX),
            (Mul, u32::MAX, 2, u32::MAX - 1), (And, 5, 3, 1),
            (Or, 5, 3, 7), (Xor, 5, 3, 6), (Shl, 1, 31, 0x80000000),
            (Shl, 1, 32, 0), (Shr, u32::MAX, 32, 0),
            (Shr, 0x80000000, 31, 1), (Sar, 0x80000000, 32, u32::MAX),
            (UMin, u32::MAX, 1, 1), (UMax, u32::MAX, 1, u32::MAX),
            (SMin, u32::MAX, 1, u32::MAX), (SMax, u32::MAX, 1, 1),
            (SLt, u32::MAX, 1, 1), (SLe, 1, 1, 1), (SGt, 1, u32::MAX, 1),
            (SGe, u32::MAX, 1, 0), (ULt, u32::MAX, 1, 0), (ULe, 1, 1, 1),
            (UGt, u32::MAX, 1, 1), (UGe, 1, u32::MAX, 0),
            (Eq, 2, 2, 1), (Ne, 2, 3, 1), (LAnd, 4, 2, 1),
            (LOr, 0, 2, 1), (LXor, 4, 2, 0),
        ] {
            let code = [Immediate as u32, a, Immediate as u32, b, op as u32];
            assert_eq!(evaluate_handle_program(&code, |_, _| 0, |_, _| 0, 0),
                       Some(expected), "{op:?}");
        }
        for (op, value, expected) in [(Neg, 1, u32::MAX), (Not, 1, !1),
                                      (LNot, 0, 1), (LNot, 5, 0)] {
            assert_eq!(evaluate_handle_program(&[Immediate as u32, value, op as u32],
                |_, _| 0, |_, _| 0, 0), Some(expected));
        }
    }

    #[test]
    fn handle_program_index_pushes_the_descriptor_array_element() {
        use HandleOp::*;
        // A light table read at element * 0x60 + 0x10: one handle per element.
        let code = [Index as u32, Immediate as u32, 0x60, Mul as u32, Immediate as u32, 0x10,
                    Add as u32, Storage as u32, 1];
        for element in [0u32, 1, 63] {
            assert_eq!(evaluate_handle_program(&code, |_, _| panic!("no cbuf read"),
                |binding, offset| { assert_eq!(binding, 1); offset }, element),
                Some(element * 0x60 + 0x10));
        }
    }

    #[test]
    fn handle_program_slots_select_and_upstream_bounds() {
        use HandleOp::*;
        let eval = |code: &[u32]| evaluate_handle_program(code, |_, _| 0, |_, _| 0, 0);
        assert_eq!(eval(&[Immediate as u32, 3, Tee as u32, 255, Load as u32, 255,
                         Add as u32]), Some(6));
        for (condition, expected) in [(0, 9), (7, 5)] {
            assert_eq!(eval(&[Immediate as u32, condition, Immediate as u32, 5,
                             Immediate as u32, 9, Select as u32]), Some(expected));
        }
        assert_eq!(eval(&[Load as u32, 256]), Some(0));
        assert_eq!(eval(&[Immediate as u32, 5, Tee as u32, 256]), Some(5));
        assert_eq!(eval(&[Immediate as u32]), Some(0));
        assert_eq!(eval(&[Sub as u32]), Some(0));
        assert_eq!(eval(&[]), None);
        assert_eq!(eval(&[u32::MAX]), None);
        assert_eq!(eval(&[Immediate as u32, 1, Immediate as u32, 2]), None);
        let mut code = Vec::new();
        for _ in 0..65 { code.extend([Immediate as u32, 1]); }
        code.extend(std::iter::repeat_n(Add as u32, 63));
        assert_eq!(eval(&code), Some(64));
        assert_eq!(HandleOp::Load as u32, 35);
    }
}
