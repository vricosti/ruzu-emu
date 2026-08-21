// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SPIR-V emission context — maps to upstream
//! `backend/spirv/spirv_emit_context.h` and `spirv_emit_context.cpp`.
//!
//! Wraps rspirv builder with cached types, constants, and resource variables.

use rspirv::binary::Assemble;
use rspirv::dr::{Builder, Operand};
use rspirv::spirv;
use std::collections::HashMap;

use crate::backend::bindings::Bindings;
use crate::ir;
use crate::ir::program::ShaderInfo;
use crate::ir::types::{ShaderStage, Type};
use crate::profile::Profile;
use crate::runtime_info::{AttributeType, InputTopology, RuntimeInfo};
use crate::shader_info::{
    ConstantBufferDescriptor, ImageBufferDescriptor, ImageDescriptor, ImageFormat,
    TextureBufferDescriptor, TextureDescriptor, TextureType,
};

struct DeferredPhi {
    result_id: spirv::Word,
    values: Vec<ir::Value>,
}

#[derive(Clone, Copy)]
enum StorageCasOperation {
    Increment,
    Decrement,
    FpAdd,
    FpMin,
    FpMax,
}

#[derive(Clone, Copy, Default)]
pub(crate) enum InputGenericLoadOp {
    #[default]
    None,
    Bitcast,
    SToF,
    UToF,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct InputGenericInfo {
    pub id: spirv::Word,
    pub pointer_type: spirv::Word,
    pub component_type: spirv::Word,
    pub load_op: InputGenericLoadOp,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct GenericElementInfo {
    pub id: spirv::Word,
    pub first_element: u32,
    pub num_components: u32,
}

/// Port of upstream `TextureDefinition` in `spirv_emit_context.h`.
#[derive(Clone, Copy)]
pub(crate) struct TextureDefinition {
    pub id: spirv::Word,
    pub sampled_type: spirv::Word,
    pub pointer_type: spirv::Word,
    pub image_type: spirv::Word,
    pub count: u32,
    pub is_multisample: bool,
    pub is_integer: bool,
}

/// Port of upstream `TextureBufferDefinition` in `spirv_emit_context.h`.
#[derive(Clone, Copy)]
pub(crate) struct TextureBufferDefinition {
    pub id: spirv::Word,
    pub count: u32,
}

/// Port of upstream `ImageBufferDefinition` in `spirv_emit_context.h`.
#[derive(Clone, Copy)]
pub(crate) struct ImageBufferDefinition {
    pub id: spirv::Word,
    pub image_type: spirv::Word,
    pub pointer_type: spirv::Word,
    pub count: u32,
    pub is_integer: bool,
}

/// Port of upstream `ImageDefinition` in `spirv_emit_context.h`.
#[derive(Clone, Copy)]
pub(crate) struct ImageDefinition {
    pub id: spirv::Word,
    pub image_type: spirv::Word,
    pub pointer_type: spirv::Word,
    pub count: u32,
    pub is_integer: bool,
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct UniformDefinitions {
    pub u8_scalar: spirv::Word,
    pub i8_scalar: spirv::Word,
    pub u16_scalar: spirv::Word,
    pub i16_scalar: spirv::Word,
    pub u32_scalar: spirv::Word,
    pub f32_scalar: spirv::Word,
    pub u32x2: spirv::Word,
    pub u32x4: spirv::Word,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UniformDefinitionKind {
    U8,
    I8,
    U16,
    I16,
    U32,
    F32,
    U32x2,
    U32x4,
}

impl UniformDefinitions {
    pub(crate) fn get(self, kind: UniformDefinitionKind) -> spirv::Word {
        match kind {
            UniformDefinitionKind::U8 => self.u8_scalar,
            UniformDefinitionKind::I8 => self.i8_scalar,
            UniformDefinitionKind::U16 => self.u16_scalar,
            UniformDefinitionKind::I16 => self.i16_scalar,
            UniformDefinitionKind::U32 => self.u32_scalar,
            UniformDefinitionKind::F32 => self.f32_scalar,
            UniformDefinitionKind::U32x2 => self.u32x2,
            UniformDefinitionKind::U32x4 => self.u32x4,
        }
    }

    fn set(&mut self, kind: UniformDefinitionKind, value: spirv::Word) {
        match kind {
            UniformDefinitionKind::U8 => self.u8_scalar = value,
            UniformDefinitionKind::I8 => self.i8_scalar = value,
            UniformDefinitionKind::U16 => self.u16_scalar = value,
            UniformDefinitionKind::I16 => self.i16_scalar = value,
            UniformDefinitionKind::U32 => self.u32_scalar = value,
            UniformDefinitionKind::F32 => self.f32_scalar = value,
            UniformDefinitionKind::U32x2 => self.u32x2 = value,
            UniformDefinitionKind::U32x4 => self.u32x4 = value,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct StorageTypeDefinition {
    pub array: spirv::Word,
    pub element: spirv::Word,
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct StorageTypeDefinitions {
    pub u8_scalar: StorageTypeDefinition,
    pub i8_scalar: StorageTypeDefinition,
    pub u16_scalar: StorageTypeDefinition,
    pub i16_scalar: StorageTypeDefinition,
    pub u32_scalar: StorageTypeDefinition,
    pub f32_scalar: StorageTypeDefinition,
    pub u64_scalar: StorageTypeDefinition,
    pub u32x2: StorageTypeDefinition,
    pub u32x4: StorageTypeDefinition,
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct StorageDefinitions {
    pub u8_scalar: spirv::Word,
    pub i8_scalar: spirv::Word,
    pub u16_scalar: spirv::Word,
    pub i16_scalar: spirv::Word,
    pub u32_scalar: spirv::Word,
    pub f32_scalar: spirv::Word,
    pub u64_scalar: spirv::Word,
    pub u32x2: spirv::Word,
    pub u32x4: spirv::Word,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StorageDefinitionKind {
    U8,
    I8,
    U16,
    I16,
    U32,
    F32,
    U64,
    U32x2,
    U32x4,
}

impl StorageTypeDefinitions {
    pub(crate) fn get(self, kind: StorageDefinitionKind) -> StorageTypeDefinition {
        match kind {
            StorageDefinitionKind::U8 => self.u8_scalar,
            StorageDefinitionKind::I8 => self.i8_scalar,
            StorageDefinitionKind::U16 => self.u16_scalar,
            StorageDefinitionKind::I16 => self.i16_scalar,
            StorageDefinitionKind::U32 => self.u32_scalar,
            StorageDefinitionKind::F32 => self.f32_scalar,
            StorageDefinitionKind::U64 => self.u64_scalar,
            StorageDefinitionKind::U32x2 => self.u32x2,
            StorageDefinitionKind::U32x4 => self.u32x4,
        }
    }

    fn set(&mut self, kind: StorageDefinitionKind, value: StorageTypeDefinition) {
        match kind {
            StorageDefinitionKind::U8 => self.u8_scalar = value,
            StorageDefinitionKind::I8 => self.i8_scalar = value,
            StorageDefinitionKind::U16 => self.u16_scalar = value,
            StorageDefinitionKind::I16 => self.i16_scalar = value,
            StorageDefinitionKind::U32 => self.u32_scalar = value,
            StorageDefinitionKind::F32 => self.f32_scalar = value,
            StorageDefinitionKind::U64 => self.u64_scalar = value,
            StorageDefinitionKind::U32x2 => self.u32x2 = value,
            StorageDefinitionKind::U32x4 => self.u32x4 = value,
        }
    }
}

impl StorageDefinitions {
    pub(crate) fn get(self, kind: StorageDefinitionKind) -> spirv::Word {
        match kind {
            StorageDefinitionKind::U8 => self.u8_scalar,
            StorageDefinitionKind::I8 => self.i8_scalar,
            StorageDefinitionKind::U16 => self.u16_scalar,
            StorageDefinitionKind::I16 => self.i16_scalar,
            StorageDefinitionKind::U32 => self.u32_scalar,
            StorageDefinitionKind::F32 => self.f32_scalar,
            StorageDefinitionKind::U64 => self.u64_scalar,
            StorageDefinitionKind::U32x2 => self.u32x2,
            StorageDefinitionKind::U32x4 => self.u32x4,
        }
    }

    fn set(&mut self, kind: StorageDefinitionKind, value: spirv::Word) {
        match kind {
            StorageDefinitionKind::U8 => self.u8_scalar = value,
            StorageDefinitionKind::I8 => self.i8_scalar = value,
            StorageDefinitionKind::U16 => self.u16_scalar = value,
            StorageDefinitionKind::I16 => self.i16_scalar = value,
            StorageDefinitionKind::U32 => self.u32_scalar = value,
            StorageDefinitionKind::F32 => self.f32_scalar = value,
            StorageDefinitionKind::U64 => self.u64_scalar = value,
            StorageDefinitionKind::U32x2 => self.u32x2 = value,
            StorageDefinitionKind::U32x4 => self.u32x4 = value,
        }
    }
}

/// Port of upstream `ImageType(EmitContext&, const TextureDescriptor&)`.
fn texture_image_type(ctx: &mut SpirvEmitContext, desc: &TextureDescriptor) -> spirv::Word {
    let depth = u32::from(desc.is_depth);
    let multisampled = u32::from(desc.is_multisample);
    let (dim, arrayed, ms) = match desc.texture_type {
        TextureType::Color1D => (spirv::Dim::Dim1D, 0, 0),
        TextureType::ColorArray1D => (spirv::Dim::Dim1D, 1, 0),
        TextureType::Color2D | TextureType::Color2DRect => (spirv::Dim::Dim2D, 0, multisampled),
        TextureType::ColorArray2D => (spirv::Dim::Dim2D, 1, multisampled),
        TextureType::Color3D => (spirv::Dim::Dim3D, 0, 0),
        TextureType::ColorCube => (spirv::Dim::DimCube, 0, 0),
        TextureType::ColorArrayCube => (spirv::Dim::DimCube, 1, 0),
        TextureType::Buffer => panic!("SPIR-V: buffer texture in sampled texture descriptors"),
    };
    let sampled_type = if desc.is_integer {
        ctx.u32_type
    } else {
        ctx.f32_type
    };
    ctx.builder.type_image(
        sampled_type,
        dim,
        depth,
        arrayed,
        ms,
        1,
        spirv::ImageFormat::Unknown,
        None,
    )
}

/// Port of upstream `GetImageFormat`.
fn image_format(format: ImageFormat) -> spirv::ImageFormat {
    match format {
        ImageFormat::Typeless => spirv::ImageFormat::Unknown,
        ImageFormat::R8Uint => spirv::ImageFormat::R8ui,
        ImageFormat::R8Sint => spirv::ImageFormat::R8i,
        ImageFormat::R16Uint => spirv::ImageFormat::R16ui,
        ImageFormat::R16Sint => spirv::ImageFormat::R16i,
        ImageFormat::R32Uint => spirv::ImageFormat::R32ui,
        ImageFormat::R32G32Uint => spirv::ImageFormat::Rg32ui,
        ImageFormat::R32G32B32A32Uint => spirv::ImageFormat::Rgba32ui,
    }
}

/// Port of upstream `ImageType(EmitContext&, const ImageDescriptor&, Id)`.
fn storage_image_type(
    ctx: &mut SpirvEmitContext,
    desc: &ImageDescriptor,
    sampled_type: spirv::Word,
) -> spirv::Word {
    let (dim, arrayed) = match desc.texture_type {
        TextureType::Color1D => (spirv::Dim::Dim1D, 0),
        TextureType::ColorArray1D => (spirv::Dim::Dim1D, 1),
        TextureType::Color2D => (spirv::Dim::Dim2D, 0),
        TextureType::ColorArray2D => (spirv::Dim::Dim2D, 1),
        TextureType::Color3D => (spirv::Dim::Dim3D, 0),
        TextureType::Buffer => panic!("SPIR-V: image buffer in image descriptors"),
        TextureType::ColorCube | TextureType::ColorArrayCube | TextureType::Color2DRect => {
            panic!(
                "SPIR-V: invalid storage image texture type {:?}",
                desc.texture_type
            )
        }
    };
    ctx.builder.type_image(
        sampled_type,
        dim,
        0,
        arrayed,
        0,
        2,
        image_format(desc.format),
        None,
    )
}

/// SPIR-V emission context.
///
/// Matches upstream `EmitContext` class that inherits from `Sirit::Module`.
pub struct SpirvEmitContext {
    pub builder: Builder,
    pub profile: Profile,
    pub stage: ShaderStage,

    // ── Cached SPIR-V type IDs ────────────────────────────────────────
    pub void_type: spirv::Word,
    pub bool_type: spirv::Word,
    pub u8_type: spirv::Word,
    pub i8_type: spirv::Word,
    pub u16_type: spirv::Word,
    pub i16_type: spirv::Word,
    pub u32_type: spirv::Word,
    pub i32_type: spirv::Word,
    pub f16_type: spirv::Word,
    pub f32_type: spirv::Word,
    pub f16_vec2_type: spirv::Word,
    pub f16_vec3_type: spirv::Word,
    pub f16_vec4_type: spirv::Word,
    pub u32_vec2_type: spirv::Word,
    pub u32_vec3_type: spirv::Word,
    pub u32_vec4_type: spirv::Word,
    pub i32_vec4_type: spirv::Word,
    pub f32_vec2_type: spirv::Word,
    pub f32_vec3_type: spirv::Word,
    pub f32_vec4_type: spirv::Word,
    pub u64_type: spirv::Word,
    pub f64_type: spirv::Word,
    pub f64_vec2_type: spirv::Word,
    pub f64_vec3_type: spirv::Word,
    pub f64_vec4_type: spirv::Word,
    pub void_fn_type: spirv::Word,

    // ── Pointer types ─────────────────────────────────────────────────
    pub input_f32_ptr: spirv::Word,
    pub output_f32_ptr: spirv::Word,
    pub uniform_u32_ptr: spirv::Word,
    pub input_u32_ptr: spirv::Word,
    pub input_i32_ptr: spirv::Word,
    pub output_u32_ptr: spirv::Word,
    pub output_i32_ptr: spirv::Word,
    pub uniform_f32_ptr: spirv::Word,
    pub uniform_u32_vec2_ptr: spirv::Word,
    pub uniform_u32_vec4_ptr: spirv::Word,
    pub private_u32_ptr: spirv::Word,
    /// Upstream `EmitContext::image_u32`, used by `OpImageTexelPointer`.
    pub(crate) image_u32: spirv::Word,

    // ── Cached constants ──────────────────────────────────────────────
    pub const_zero_u32: spirv::Word,
    pub const_one_u32: spirv::Word,
    pub const_zero_f32: spirv::Word,
    pub const_one_f32: spirv::Word,
    pub const_true: spirv::Word,
    pub const_false: spirv::Word,

    // ── GLSL.std.450 extended instruction set ─────────────────────────
    pub glsl_ext: spirv::Word,

    // ── Runtime info ──────────────────────────────────────────────────
    pub runtime_info: RuntimeInfo,

    // ── System value input variables ─────────────────────────────────
    pub workgroup_id: spirv::Word,
    pub local_invocation_id: spirv::Word,
    pub invocation_id: spirv::Word,
    pub patch_vertices_in: spirv::Word,
    pub sample_id: spirv::Word,
    pub is_helper_invocation: spirv::Word,
    pub subgroup_local_invocation_id: spirv::Word,
    pub subgroup_mask_eq: spirv::Word,
    pub subgroup_mask_lt: spirv::Word,
    pub subgroup_mask_le: spirv::Word,
    pub subgroup_mask_gt: spirv::Word,
    pub subgroup_mask_ge: spirv::Word,
    pub primitive_id: spirv::Word,
    pub layer: spirv::Word,
    pub viewport_index: spirv::Word,
    pub viewport_mask: spirv::Word,
    pub instance_id: spirv::Word,
    pub instance_index: spirv::Word,
    pub base_instance: spirv::Word,
    pub vertex_id: spirv::Word,
    pub vertex_index: spirv::Word,
    pub base_vertex: spirv::Word,
    pub draw_index: spirv::Word,
    pub front_face: spirv::Word,
    pub point_coord: spirv::Word,
    pub tess_coord: spirv::Word,
    pub clip_distances: spirv::Word,
    /// Clip-distance components written by the IR program. This is kept per
    /// compilation context instead of Eden's header-level bitset so parallel
    /// shader compilation cannot leak state between programs.
    pub(crate) clip_distance_written: [bool; 8],
    pub need_input_position_indirect: bool,
    pub input_position: spirv::Word,
    pub output_point_size: spirv::Word,
    pub output_position: spirv::Word,
    pub output_tess_level_outer: spirv::Word,
    pub output_tess_level_inner: spirv::Word,
    pub patches: [spirv::Word; 30],
    pub frag_color: [spirv::Word; 8],
    pub sample_mask: spirv::Word,
    pub frag_depth: spirv::Word,
    pub fswzadd_lut_a: spirv::Word,
    pub fswzadd_lut_b: spirv::Word,

    // ── Rescaling / render area push constants ───────────────────────
    pub rescaling_uniform_constant: spirv::Word,
    pub rescaling_push_constants: spirv::Word,
    pub rescaling_downfactor_member_index: u32,
    pub render_area_push_constant: spirv::Word,
    pub render_are_member_index: u32,

    // ── Local / shared memory ─────────────────────────────────────────
    pub local_memory: spirv::Word,
    pub shared_u8: spirv::Word,
    pub shared_u16: spirv::Word,
    pub shared_u32: spirv::Word,
    pub shared_u64: spirv::Word,
    pub shared_u32x2: spirv::Word,
    pub shared_u32x4: spirv::Word,
    pub shared_memory_u8: spirv::Word,
    pub shared_memory_u16: spirv::Word,
    pub shared_memory_u32: spirv::Word,
    pub shared_memory_u64: spirv::Word,
    pub shared_memory_u32x2: spirv::Word,
    pub shared_memory_u32x4: spirv::Word,
    pub shared_memory_u32_type: spirv::Word,
    pub shared_store_u8_func: spirv::Word,
    pub shared_store_u16_func: spirv::Word,
    pub increment_cas_shared: spirv::Word,
    pub decrement_cas_shared: spirv::Word,
    /// Set by `define_shared_memory`; mirrors upstream
    /// `EmitContext::uses_explicit_workgroup_layout`.
    pub uses_explicit_workgroup_layout: bool,
    /// Ids already carrying `OpDecorate … NonUniform`, so the decoration is
    /// emitted at most once per id. Mirrors upstream
    /// `EmitContext::non_uniform_ids`.
    pub non_uniform_ids: std::collections::HashSet<spirv::Word>,
    pub uses_nonuniform_sampled_image: bool,
    pub uses_nonuniform_storage_image: bool,
    pub uses_nonuniform_uniform_texel_buffer: bool,
    pub uses_nonuniform_storage_texel_buffer: bool,

    // ── Resources ─────────────────────────────────────────────────────
    /// Typed constant-buffer views indexed by CB index. The views alias the
    /// same descriptor bindings when the device supports descriptor aliasing.
    pub(crate) cbufs: HashMap<u32, UniformDefinitions>,
    pub(crate) uniform_types: UniformDefinitions,
    pub(crate) load_const_func_u8: spirv::Word,
    pub(crate) load_const_func_u16: spirv::Word,
    pub(crate) load_const_func_u32: spirv::Word,
    pub(crate) load_const_func_f32: spirv::Word,
    pub(crate) load_const_func_u32x2: spirv::Word,
    pub(crate) load_const_func_u32x4: spirv::Word,
    pub(crate) storage_types: StorageTypeDefinitions,
    pub(crate) ssbos: HashMap<u32, StorageDefinitions>,
    pub(crate) increment_cas_ssbo: spirv::Word,
    pub(crate) decrement_cas_ssbo: spirv::Word,
    pub(crate) f32_add_cas: spirv::Word,
    pub(crate) f16x2_add_cas: spirv::Word,
    pub(crate) f16x2_min_cas: spirv::Word,
    pub(crate) f16x2_max_cas: spirv::Word,
    pub(crate) f32x2_add_cas: spirv::Word,
    pub(crate) f32x2_min_cas: spirv::Word,
    pub(crate) f32x2_max_cas: spirv::Word,
    pub(crate) write_storage_cas_loop_func: spirv::Word,
    pub(crate) load_global_func_u32: spirv::Word,
    pub(crate) load_global_func_u32x2: spirv::Word,
    pub(crate) load_global_func_u32x4: spirv::Word,
    pub(crate) write_global_func_u32: spirv::Word,
    pub(crate) write_global_func_u32x2: spirv::Word,
    pub(crate) write_global_func_u32x4: spirv::Word,
    pub(crate) indexed_load_func: spirv::Word,
    pub(crate) indexed_store_func: spirv::Word,
    /// Texture combined image sampler variables, indexed by descriptor index.
    pub(crate) textures: Vec<TextureDefinition>,
    /// Uniform texel-buffer variables, indexed by descriptor index.
    pub(crate) texture_buffers: Vec<TextureBufferDefinition>,
    /// Storage texel-buffer variables, indexed by descriptor index.
    pub(crate) image_buffers: Vec<ImageBufferDefinition>,
    /// Storage image variables, indexed by descriptor index.
    pub(crate) images: Vec<ImageDefinition>,
    /// Shared sampled image type for uniform texel buffers.
    pub(crate) image_buffer_type: spirv::Word,
    /// Input variables (vertex attributes / fragment varyings).
    pub input_vars: HashMap<u32, spirv::Word>,
    pub(crate) input_generics: [InputGenericInfo; 32],
    /// Output variables (vertex outputs / fragment colors).
    pub output_vars: HashMap<u32, spirv::Word>,
    pub(crate) output_generics: [[GenericElementInfo; 4]; 32],
    /// Entry-point interface variables.
    ///
    /// Upstream keeps this list on `EmitContext` and appends variables as they
    /// are defined. Do not reconstruct it later, because resources are part of
    /// the interface for SPIR-V 1.4+.
    pub interfaces: Vec<spirv::Word>,

    // ── Value mapping ─────────────────────────────────────────────────
    /// Maps IR instruction references (block, inst) to SPIR-V result IDs.
    pub values: HashMap<(u32, u32), spirv::Word>,
    /// Maps IR block indices to SPIR-V label IDs.
    pub block_labels: Vec<spirv::Word>,
    /// Phi values are patched after all blocks have been emitted, matching
    /// upstream Sirit's `DeferredOpPhi` / `PatchDeferredPhi` lifecycle.
    deferred_phis: Vec<DeferredPhi>,
}

impl SpirvEmitContext {
    /// Create a new SPIR-V emission context.
    pub fn new(program: &ir::Program, profile: &Profile, runtime_info: &RuntimeInfo) -> Self {
        let mut builder = Builder::new();
        builder.set_version(
            (profile.supported_spirv >> 16) as u8,
            ((profile.supported_spirv >> 8) & 0xff) as u8,
        );
        builder.capability(spirv::Capability::Shader);

        // Upstream gates Float16/Float64/Int* capabilities on
        // `program.info.uses_fp16/fp64/int8/int16/int64` — the program-
        // info flags populated during translation.
        if program.info.uses_fp16 {
            builder.capability(spirv::Capability::Float16);
        }
        if program.info.uses_fp64 {
            builder.capability(spirv::Capability::Float64);
        }
        if program.info.uses_int8 && profile.support_int8 {
            builder.capability(spirv::Capability::Int8);
        }
        if program.info.uses_int16 && profile.support_int16 {
            builder.capability(spirv::Capability::Int16);
        }
        if program.info.uses_int64 && profile.support_int64 {
            builder.capability(spirv::Capability::Int64);
        }
        builder.memory_model(spirv::AddressingModel::Logical, spirv::MemoryModel::GLSL450);

        let glsl_ext = builder.ext_inst_import("GLSL.std.450");

        // Define scalar types
        let void_type = builder.type_void();
        let bool_type = builder.type_bool();
        let u8_type = if program.info.uses_int8 && profile.support_int8 {
            builder.type_int(8, 0)
        } else {
            0
        };
        let i8_type = if program.info.uses_int8 && profile.support_int8 {
            builder.type_int(8, 1)
        } else {
            0
        };
        let u16_type = if program.info.uses_int16 && profile.support_int16 {
            builder.type_int(16, 0)
        } else {
            0
        };
        let i16_type = if program.info.uses_int16 && profile.support_int16 {
            builder.type_int(16, 1)
        } else {
            0
        };
        let u32_type = builder.type_int(32, 0);
        let i32_type = builder.type_int(32, 1);
        let f32_type = builder.type_float(32);
        let f16_type = if program.info.uses_fp16 {
            builder.type_float(16)
        } else {
            f32_type
        };

        // Define vector types
        let u32_vec2_type = builder.type_vector(u32_type, 2);
        let u32_vec3_type = builder.type_vector(u32_type, 3);
        let u32_vec4_type = builder.type_vector(u32_type, 4);
        let i32_vec4_type = builder.type_vector(i32_type, 4);
        let f32_vec2_type = builder.type_vector(f32_type, 2);
        let f32_vec3_type = builder.type_vector(f32_type, 3);
        let f32_vec4_type = builder.type_vector(f32_type, 4);
        let f16_vec2_type = if program.info.uses_fp16 {
            builder.type_vector(f16_type, 2)
        } else {
            f32_vec2_type
        };
        let f16_vec3_type = if program.info.uses_fp16 {
            builder.type_vector(f16_type, 3)
        } else {
            f32_vec3_type
        };
        let f16_vec4_type = if program.info.uses_fp16 {
            builder.type_vector(f16_type, 4)
        } else {
            f32_vec4_type
        };

        // Upstream only defines 64-bit scalar types when the program uses
        // them. Declaring OpTypeInt/OpTypeFloat 64 without the corresponding
        // capability makes otherwise 32-bit shaders invalid SPIR-V.
        let u64_type = if program.info.uses_int64 && profile.support_int64 {
            builder.type_int(64, 0)
        } else {
            u32_type
        };
        let f64_type = if program.info.uses_fp64 {
            builder.type_float(64)
        } else {
            f32_type
        };
        let f64_vec2_type = if program.info.uses_fp64 {
            builder.type_vector(f64_type, 2)
        } else {
            f32_vec2_type
        };
        let f64_vec3_type = if program.info.uses_fp64 {
            builder.type_vector(f64_type, 3)
        } else {
            f32_vec3_type
        };
        let f64_vec4_type = if program.info.uses_fp64 {
            builder.type_vector(f64_type, 4)
        } else {
            f32_vec4_type
        };

        // Function type: void(void)
        let void_fn_type = builder.type_function(void_type, vec![]);

        // Pointer types
        let input_f32_ptr = builder.type_pointer(None, spirv::StorageClass::Input, f32_type);
        let output_f32_ptr = builder.type_pointer(None, spirv::StorageClass::Output, f32_type);
        let input_u32_ptr = builder.type_pointer(None, spirv::StorageClass::Input, u32_type);
        let input_i32_ptr = builder.type_pointer(None, spirv::StorageClass::Input, i32_type);
        let output_u32_ptr = builder.type_pointer(None, spirv::StorageClass::Output, u32_type);
        let output_i32_ptr = builder.type_pointer(None, spirv::StorageClass::Output, i32_type);
        let uniform_u32_ptr = builder.type_pointer(None, spirv::StorageClass::Uniform, u32_type);
        let uniform_f32_ptr = builder.type_pointer(None, spirv::StorageClass::Uniform, f32_type);
        let uniform_u32_vec2_ptr =
            builder.type_pointer(None, spirv::StorageClass::Uniform, u32_vec2_type);
        let uniform_u32_vec4_ptr =
            builder.type_pointer(None, spirv::StorageClass::Uniform, u32_vec4_type);
        let private_u32_ptr = builder.type_pointer(None, spirv::StorageClass::Private, u32_type);

        // Define constants
        let const_zero_u32 = builder.constant_bit32(u32_type, 0);
        let const_one_u32 = builder.constant_bit32(u32_type, 1);
        let const_zero_f32 = builder.constant_bit32(f32_type, 0.0f32.to_bits());
        let const_one_f32 = builder.constant_bit32(f32_type, 1.0f32.to_bits());
        let const_true = builder.constant_true(bool_type);
        let const_false = builder.constant_false(bool_type);

        Self {
            builder,
            profile: profile.clone(),
            stage: program.stage,
            runtime_info: runtime_info.clone(),
            workgroup_id: 0,
            local_invocation_id: 0,
            invocation_id: 0,
            patch_vertices_in: 0,
            sample_id: 0,
            is_helper_invocation: 0,
            subgroup_local_invocation_id: 0,
            subgroup_mask_eq: 0,
            subgroup_mask_lt: 0,
            subgroup_mask_le: 0,
            subgroup_mask_gt: 0,
            subgroup_mask_ge: 0,
            primitive_id: 0,
            layer: 0,
            viewport_index: 0,
            viewport_mask: 0,
            instance_id: 0,
            instance_index: 0,
            base_instance: 0,
            vertex_id: 0,
            vertex_index: 0,
            base_vertex: 0,
            draw_index: 0,
            front_face: 0,
            point_coord: 0,
            tess_coord: 0,
            clip_distances: 0,
            clip_distance_written: std::array::from_fn(|index| {
                program
                    .info
                    .stores
                    .get((ir::value::Attribute::CLIP_DISTANCE_0.0 + index as u32) as usize)
            }),
            need_input_position_indirect: false,
            input_position: 0,
            output_point_size: 0,
            output_position: 0,
            output_tess_level_outer: 0,
            output_tess_level_inner: 0,
            patches: [0; 30],
            frag_color: [0; 8],
            sample_mask: 0,
            frag_depth: 0,
            fswzadd_lut_a: 0,
            fswzadd_lut_b: 0,
            rescaling_uniform_constant: 0,
            rescaling_push_constants: 0,
            rescaling_downfactor_member_index: 0,
            render_area_push_constant: 0,
            render_are_member_index: 0,
            local_memory: 0,
            shared_u8: 0,
            shared_u16: 0,
            shared_u32: 0,
            shared_u64: 0,
            shared_u32x2: 0,
            shared_u32x4: 0,
            shared_memory_u8: 0,
            shared_memory_u16: 0,
            shared_memory_u32: 0,
            shared_memory_u64: 0,
            shared_memory_u32x2: 0,
            shared_memory_u32x4: 0,
            shared_memory_u32_type: 0,
            shared_store_u8_func: 0,
            shared_store_u16_func: 0,
            increment_cas_shared: 0,
            decrement_cas_shared: 0,
            uses_explicit_workgroup_layout: false,
            non_uniform_ids: std::collections::HashSet::new(),
            uses_nonuniform_sampled_image: false,
            uses_nonuniform_storage_image: false,
            uses_nonuniform_uniform_texel_buffer: false,
            uses_nonuniform_storage_texel_buffer: false,
            void_type,
            bool_type,
            u8_type,
            i8_type,
            u16_type,
            i16_type,
            u32_type,
            i32_type,
            f16_type,
            f32_type,
            f16_vec2_type,
            f16_vec3_type,
            f16_vec4_type,
            u32_vec2_type,
            u32_vec3_type,
            u32_vec4_type,
            i32_vec4_type,
            f32_vec2_type,
            f32_vec3_type,
            f32_vec4_type,
            u64_type,
            f64_type,
            f64_vec2_type,
            f64_vec3_type,
            f64_vec4_type,
            void_fn_type,
            input_f32_ptr,
            output_f32_ptr,
            uniform_u32_ptr,
            input_u32_ptr,
            input_i32_ptr,
            output_u32_ptr,
            output_i32_ptr,
            uniform_f32_ptr,
            uniform_u32_vec2_ptr,
            uniform_u32_vec4_ptr,
            private_u32_ptr,
            image_u32: 0,
            const_zero_u32,
            const_one_u32,
            const_zero_f32,
            const_one_f32,
            const_true,
            const_false,
            glsl_ext,
            cbufs: HashMap::new(),
            uniform_types: UniformDefinitions::default(),
            load_const_func_u8: 0,
            load_const_func_u16: 0,
            load_const_func_u32: 0,
            load_const_func_f32: 0,
            load_const_func_u32x2: 0,
            load_const_func_u32x4: 0,
            storage_types: StorageTypeDefinitions::default(),
            ssbos: HashMap::new(),
            increment_cas_ssbo: 0,
            decrement_cas_ssbo: 0,
            f32_add_cas: 0,
            f16x2_add_cas: 0,
            f16x2_min_cas: 0,
            f16x2_max_cas: 0,
            f32x2_add_cas: 0,
            f32x2_min_cas: 0,
            f32x2_max_cas: 0,
            write_storage_cas_loop_func: 0,
            load_global_func_u32: 0,
            load_global_func_u32x2: 0,
            load_global_func_u32x4: 0,
            write_global_func_u32: 0,
            write_global_func_u32x2: 0,
            write_global_func_u32x4: 0,
            indexed_load_func: 0,
            indexed_store_func: 0,
            textures: Vec::new(),
            texture_buffers: Vec::new(),
            image_buffers: Vec::new(),
            images: Vec::new(),
            image_buffer_type: 0,
            input_vars: HashMap::new(),
            input_generics: [InputGenericInfo::default(); 32],
            output_vars: HashMap::new(),
            output_generics: [[GenericElementInfo::default(); 4]; 32],
            interfaces: Vec::new(),
            values: HashMap::new(),
            block_labels: Vec::new(),
            deferred_phis: Vec::new(),
        }
    }

    /// Create a u32 constant.
    pub fn constant_u32(&mut self, value: u32) -> spirv::Word {
        self.builder.constant_bit32(self.u32_type, value)
    }

    /// Port of upstream `EmitContext::BitOffset8`.
    pub(crate) fn bit_offset_8(&mut self, offset: ir::Value) -> spirv::Word {
        if offset.is_immediate() {
            return self.constant_u32((offset.imm_u32() % 4) * 8);
        }
        let offset = self.resolve_value(&offset);
        let three = self.constant_u32(3);
        let shifted = self
            .builder
            .shift_left_logical(self.u32_type, None, offset, three)
            .unwrap();
        let twenty_four = self.constant_u32(24);
        self.builder
            .bitwise_and(self.u32_type, None, shifted, twenty_four)
            .unwrap()
    }

    /// Port of upstream `EmitContext::BitOffset16`.
    pub(crate) fn bit_offset_16(&mut self, offset: ir::Value) -> spirv::Word {
        if offset.is_immediate() {
            return self.constant_u32(((offset.imm_u32() / 2) % 2) * 16);
        }
        let offset = self.resolve_value(&offset);
        let three = self.constant_u32(3);
        let shifted = self
            .builder
            .shift_left_logical(self.u32_type, None, offset, three)
            .unwrap();
        let sixteen = self.constant_u32(16);
        self.builder
            .bitwise_and(self.u32_type, None, shifted, sixteen)
            .unwrap()
    }

    /// Create an i32 constant.
    pub fn constant_i32(&mut self, value: i32) -> spirv::Word {
        self.builder.constant_bit32(self.i32_type, value as u32)
    }

    /// Create an f32 constant.
    pub fn constant_f32(&mut self, value: f32) -> spirv::Word {
        self.builder.constant_bit32(self.f32_type, value.to_bits())
    }

    fn define_constant_buffer_view(
        &mut self,
        descriptors: &[ConstantBufferDescriptor],
        mut binding: u32,
        element_type: spirv::Word,
        element_size: u32,
        kind: UniformDefinitionKind,
    ) {
        let array_len = self
            .builder
            .constant_bit32(self.u32_type, 0x1_0000 / element_size);
        let array_type = self.builder.type_array(element_type, array_len);
        self.builder.decorate(
            array_type,
            spirv::Decoration::ArrayStride,
            vec![Operand::LiteralBit32(element_size)],
        );
        let struct_type = self.builder.type_struct(vec![array_type]);
        self.builder
            .decorate(struct_type, spirv::Decoration::Block, vec![]);
        self.builder.member_decorate(
            struct_type,
            0,
            spirv::Decoration::Offset,
            vec![Operand::LiteralBit32(0)],
        );
        let pointer_type =
            self.builder
                .type_pointer(None, spirv::StorageClass::Uniform, struct_type);
        let uniform_type =
            self.builder
                .type_pointer(None, spirv::StorageClass::Uniform, element_type);
        self.uniform_types.set(kind, uniform_type);

        for desc in descriptors {
            let var = self
                .builder
                .variable(pointer_type, None, spirv::StorageClass::Uniform, None);
            self.builder.decorate(
                var,
                spirv::Decoration::DescriptorSet,
                vec![Operand::LiteralBit32(0)],
            );
            self.builder.decorate(
                var,
                spirv::Decoration::Binding,
                vec![Operand::LiteralBit32(binding)],
            );
            for index in 0..desc.count {
                let definitions = self.cbufs.entry(desc.index + index).or_default();
                definitions.set(kind, var);
            }
            if self.profile.supported_spirv >= 0x0001_0400 {
                self.interfaces.push(var);
            }
            binding += desc.count;
        }
    }

    /// Port of upstream `DefineConstantBufferIndirectFunctions`'s
    /// `make_accessor` lambda.
    fn define_constant_buffer_indirect_function(
        &mut self,
        result_type: spirv::Word,
        kind: UniformDefinitionKind,
    ) -> spirv::Word {
        let function_type = self
            .builder
            .type_function(result_type, vec![self.u32_type, self.u32_type]);
        let function = self
            .builder
            .begin_function(
                result_type,
                None,
                spirv::FunctionControl::NONE,
                function_type,
            )
            .unwrap();
        let binding = self.builder.function_parameter(self.u32_type).unwrap();
        let offset = self.builder.function_parameter(self.u32_type).unwrap();
        let labels =
            std::array::from_fn::<_, { crate::shader_info::Info::MAX_INDIRECT_CBUFS }, _>(|_| {
                self.builder.id()
            });
        let merge_label = self.builder.id();

        self.builder.begin_block(None).unwrap();
        self.builder
            .selection_merge(merge_label, spirv::SelectionControl::NONE)
            .unwrap();
        self.builder
            .switch(
                binding,
                labels[0],
                labels
                    .iter()
                    .enumerate()
                    .map(|(index, &label)| (Operand::LiteralBit32(index as u32), label)),
            )
            .unwrap();

        let pointer_type = self.uniform_types.get(kind);
        assert_ne!(pointer_type, 0, "missing indirect CBUF pointer type");
        for (index, label) in labels.into_iter().enumerate() {
            self.builder.begin_block(Some(label)).unwrap();
            let cbuf = self
                .cbufs
                .get(&(index as u32))
                .copied()
                .unwrap_or_default()
                .get(kind);
            assert_ne!(cbuf, 0, "missing indirect CBUF {index} view {kind:?}");
            let pointer = self
                .builder
                .access_chain(pointer_type, None, cbuf, vec![self.const_zero_u32, offset])
                .unwrap();
            let result = self
                .builder
                .load(result_type, None, pointer, None, [])
                .unwrap();
            self.builder.ret_value(result).unwrap();
        }
        self.builder.begin_block(Some(merge_label)).unwrap();
        self.builder.unreachable().unwrap();
        self.builder.end_function().unwrap();
        function
    }

    /// Port of upstream `EmitContext::DefineConstantBufferIndirectFunctions`.
    fn define_constant_buffer_indirect_functions(&mut self, info: &crate::shader_info::Info) {
        if !info.uses_cbuf_indirect {
            return;
        }
        let mut types = info.used_indirect_cbuf_types;
        let supports_aliasing = self.profile.support_descriptor_aliasing;
        if supports_aliasing && types & Type::U8 as u32 != 0 {
            if self.profile.support_int8 && self.profile.support_uniform_and_storage_buffer_8bit {
                self.load_const_func_u8 = self.define_constant_buffer_indirect_function(
                    self.u8_type,
                    UniformDefinitionKind::U8,
                );
            } else {
                types |= Type::U32 as u32;
            }
        }
        if supports_aliasing && types & Type::U16 as u32 != 0 {
            if self.profile.support_int16 && self.profile.support_uniform_and_storage_buffer_16bit {
                self.load_const_func_u16 = self.define_constant_buffer_indirect_function(
                    self.u16_type,
                    UniformDefinitionKind::U16,
                );
            } else {
                types |= Type::U32 as u32;
            }
        }
        if supports_aliasing && types & Type::F32 as u32 != 0 {
            self.load_const_func_f32 = self.define_constant_buffer_indirect_function(
                self.f32_type,
                UniformDefinitionKind::F32,
            );
        }
        if supports_aliasing && types & Type::U32 as u32 != 0 {
            self.load_const_func_u32 = self.define_constant_buffer_indirect_function(
                self.u32_type,
                UniformDefinitionKind::U32,
            );
        }
        if supports_aliasing && types & Type::U32x2 as u32 != 0 {
            self.load_const_func_u32x2 = self.define_constant_buffer_indirect_function(
                self.u32_vec2_type,
                UniformDefinitionKind::U32x2,
            );
        }
        if !supports_aliasing || types & Type::U32x4 as u32 != 0 {
            self.load_const_func_u32x4 = self.define_constant_buffer_indirect_function(
                self.u32_vec4_type,
                UniformDefinitionKind::U32x4,
            );
        }
    }

    /// Port of upstream `DefineSsbos`.
    fn define_storage_buffer_view(
        &mut self,
        descriptors: &[crate::shader_info::StorageBufferDescriptor],
        mut binding: u32,
        element_type: spirv::Word,
        stride: u32,
        kind: StorageDefinitionKind,
    ) {
        let array_type = self.builder.type_runtime_array(element_type);
        self.builder.decorate(
            array_type,
            spirv::Decoration::ArrayStride,
            vec![Operand::LiteralBit32(stride)],
        );
        let struct_type = self.builder.type_struct(vec![array_type]);
        self.builder
            .decorate(struct_type, spirv::Decoration::Block, vec![]);
        self.builder.member_decorate(
            struct_type,
            0,
            spirv::Decoration::Offset,
            vec![Operand::LiteralBit32(0)],
        );
        let struct_pointer =
            self.builder
                .type_pointer(None, spirv::StorageClass::StorageBuffer, struct_type);
        let element_pointer =
            self.builder
                .type_pointer(None, spirv::StorageClass::StorageBuffer, element_type);
        self.storage_types.set(
            kind,
            StorageTypeDefinition {
                array: struct_pointer,
                element: element_pointer,
            },
        );

        let mut index = 0u32;
        for desc in descriptors {
            let id = self.builder.variable(
                struct_pointer,
                None,
                spirv::StorageClass::StorageBuffer,
                None,
            );
            self.builder.decorate(
                id,
                spirv::Decoration::Binding,
                vec![Operand::LiteralBit32(binding)],
            );
            self.builder.decorate(
                id,
                spirv::Decoration::DescriptorSet,
                vec![Operand::LiteralBit32(0)],
            );
            self.builder.name(id, format!("ssbo{index}"));
            if self.profile.supported_spirv >= 0x0001_0400 {
                self.interfaces.push(id);
            }
            for descriptor_offset in 0..desc.count {
                self.ssbos
                    .entry(index + descriptor_offset)
                    .or_default()
                    .set(kind, id);
            }
            index += desc.count;
            binding += desc.count;
        }
    }

    fn define_storage_cas_operation(
        &mut self,
        operation: StorageCasOperation,
        value_type: spirv::Word,
    ) -> spirv::Word {
        let function_type = self
            .builder
            .type_function(value_type, vec![value_type, value_type]);
        let function = self
            .builder
            .begin_function(
                value_type,
                None,
                spirv::FunctionControl::NONE,
                function_type,
            )
            .unwrap();
        let op_a = self.builder.function_parameter(value_type).unwrap();
        let op_b = self.builder.function_parameter(value_type).unwrap();
        self.builder.begin_block(None).unwrap();
        let result = match operation {
            StorageCasOperation::Increment => {
                let pred = self
                    .builder
                    .u_greater_than_equal(self.bool_type, None, op_a, op_b)
                    .unwrap();
                let incr = self
                    .builder
                    .i_add(value_type, None, op_a, self.const_one_u32)
                    .unwrap();
                self.builder
                    .select(value_type, None, pred, self.const_zero_u32, incr)
                    .unwrap()
            }
            StorageCasOperation::Decrement => {
                let lhs = self
                    .builder
                    .i_equal(self.bool_type, None, op_a, self.const_zero_u32)
                    .unwrap();
                let rhs = self
                    .builder
                    .u_greater_than(self.bool_type, None, op_a, op_b)
                    .unwrap();
                let pred = self
                    .builder
                    .logical_or(self.bool_type, None, lhs, rhs)
                    .unwrap();
                let decr = self
                    .builder
                    .i_sub(value_type, None, op_a, self.const_one_u32)
                    .unwrap();
                self.builder
                    .select(value_type, None, pred, op_b, decr)
                    .unwrap()
            }
            StorageCasOperation::FpAdd => self.builder.f_add(value_type, None, op_a, op_b).unwrap(),
            StorageCasOperation::FpMin | StorageCasOperation::FpMax => {
                let instruction = if matches!(operation, StorageCasOperation::FpMin) {
                    37
                } else {
                    40
                };
                self.builder
                    .ext_inst(
                        value_type,
                        None,
                        self.glsl_ext,
                        instruction,
                        vec![Operand::IdRef(op_a), Operand::IdRef(op_b)],
                    )
                    .unwrap()
            }
        };
        self.builder.ret_value(result).unwrap();
        self.builder.end_function().unwrap();
        function
    }

    fn define_storage_cas_loop(
        &mut self,
        operation: StorageCasOperation,
        value_type: spirv::Word,
        memory_type: spirv::Word,
    ) -> spirv::Word {
        let cas_operation = self.define_storage_cas_operation(operation, value_type);
        let storage_type = self.storage_types.get(StorageDefinitionKind::U32);
        let function_type = self.builder.type_function(
            value_type,
            vec![self.u32_type, value_type, storage_type.array],
        );
        let loop_header = self.builder.id();
        let continue_block = self.builder.id();
        let merge_block = self.builder.id();
        let function = self
            .builder
            .begin_function(
                value_type,
                None,
                spirv::FunctionControl::NONE,
                function_type,
            )
            .unwrap();
        let index = self.builder.function_parameter(self.u32_type).unwrap();
        let op_b = self.builder.function_parameter(value_type).unwrap();
        let base = self.builder.function_parameter(storage_type.array).unwrap();
        self.builder.begin_block(None).unwrap();
        self.builder.branch(loop_header).unwrap();

        self.builder.begin_block(Some(loop_header)).unwrap();
        self.builder
            .loop_merge(merge_block, continue_block, spirv::LoopControl::NONE, [])
            .unwrap();
        self.builder.branch(continue_block).unwrap();

        self.builder.begin_block(Some(continue_block)).unwrap();
        let word_pointer = self
            .builder
            .access_chain(
                storage_type.element,
                None,
                base,
                vec![self.const_zero_u32, index],
            )
            .unwrap();
        let scope = self.constant_u32(spirv::Scope::Device as u32);
        if value_type == self.f32_vec2_type {
            let raw_value = self
                .builder
                .load(self.u32_type, None, word_pointer, None, [])
                .unwrap();
            let value = self
                .builder
                .ext_inst(
                    self.f32_vec2_type,
                    None,
                    self.glsl_ext,
                    62,
                    vec![Operand::IdRef(raw_value)],
                )
                .unwrap();
            let new_value = self
                .builder
                .function_call(value_type, None, cas_operation, vec![value, op_b])
                .unwrap();
            let raw_new_value = self
                .builder
                .ext_inst(
                    self.u32_type,
                    None,
                    self.glsl_ext,
                    58,
                    vec![Operand::IdRef(new_value)],
                )
                .unwrap();
            let atomic_result = self
                .builder
                .atomic_compare_exchange(
                    self.u32_type,
                    None,
                    word_pointer,
                    scope,
                    self.const_zero_u32,
                    self.const_zero_u32,
                    raw_new_value,
                    raw_value,
                )
                .unwrap();
            let success = self
                .builder
                .i_equal(self.bool_type, None, atomic_result, raw_value)
                .unwrap();
            self.builder
                .branch_conditional(success, merge_block, loop_header, [])
                .unwrap();
            self.builder.begin_block(Some(merge_block)).unwrap();
            let result = self
                .builder
                .ext_inst(
                    self.f32_vec2_type,
                    None,
                    self.glsl_ext,
                    62,
                    vec![Operand::IdRef(atomic_result)],
                )
                .unwrap();
            self.builder.ret_value(result).unwrap();
        } else {
            let value = self
                .builder
                .load(memory_type, None, word_pointer, None, [])
                .unwrap();
            let bitcast_value = if value_type == memory_type {
                value
            } else {
                self.builder.bitcast(value_type, None, value).unwrap()
            };
            let operation_result = self
                .builder
                .function_call(value_type, None, cas_operation, vec![bitcast_value, op_b])
                .unwrap();
            let new_value = if value_type == memory_type {
                operation_result
            } else {
                self.builder
                    .bitcast(memory_type, None, operation_result)
                    .unwrap()
            };
            let atomic_result = self
                .builder
                .atomic_compare_exchange(
                    self.u32_type,
                    None,
                    word_pointer,
                    scope,
                    self.const_zero_u32,
                    self.const_zero_u32,
                    new_value,
                    value,
                )
                .unwrap();
            let success = self
                .builder
                .i_equal(self.bool_type, None, atomic_result, value)
                .unwrap();
            self.builder
                .branch_conditional(success, merge_block, loop_header, [])
                .unwrap();
            self.builder.begin_block(Some(merge_block)).unwrap();
            let result = if value_type == self.u32_type {
                atomic_result
            } else {
                self.builder
                    .bitcast(value_type, None, atomic_result)
                    .unwrap()
            };
            self.builder.ret_value(result).unwrap();
        }
        self.builder.end_function().unwrap();
        function
    }

    /// Port of upstream `EmitContext::DefineStorageBuffers`'s typed
    /// descriptor declarations and atomic CAS helpers.
    fn define_storage_buffers(&mut self, info: &crate::shader_info::Info, binding: &mut u32) {
        if info.storage_buffers_descriptors.is_empty() {
            return;
        }
        self.builder
            .extension("SPV_KHR_storage_buffer_storage_class");
        let mut used_types = if self.profile.support_descriptor_aliasing {
            info.used_storage_buffer_types
        } else {
            Type::U32 as u32
        };
        used_types |= Type::U32 as u32;
        let first_binding = *binding;
        if self.profile.support_int8
            && self.profile.support_storage_buffer_8bit
            && used_types & Type::U8 as u32 != 0
        {
            self.define_storage_buffer_view(
                &info.storage_buffers_descriptors,
                first_binding,
                self.u8_type,
                1,
                StorageDefinitionKind::U8,
            );
            self.define_storage_buffer_view(
                &info.storage_buffers_descriptors,
                first_binding,
                self.i8_type,
                1,
                StorageDefinitionKind::I8,
            );
        }
        if self.profile.support_int16
            && self.profile.support_storage_buffer_16bit
            && used_types & Type::U16 as u32 != 0
        {
            self.define_storage_buffer_view(
                &info.storage_buffers_descriptors,
                first_binding,
                self.u16_type,
                2,
                StorageDefinitionKind::U16,
            );
            self.define_storage_buffer_view(
                &info.storage_buffers_descriptors,
                first_binding,
                self.i16_type,
                2,
                StorageDefinitionKind::I16,
            );
        }
        if used_types & Type::U32 as u32 != 0 {
            self.define_storage_buffer_view(
                &info.storage_buffers_descriptors,
                first_binding,
                self.u32_type,
                4,
                StorageDefinitionKind::U32,
            );
        }
        if used_types & Type::F32 as u32 != 0 {
            self.define_storage_buffer_view(
                &info.storage_buffers_descriptors,
                first_binding,
                self.f32_type,
                4,
                StorageDefinitionKind::F32,
            );
        }
        if used_types & Type::U64 as u32 != 0 {
            self.define_storage_buffer_view(
                &info.storage_buffers_descriptors,
                first_binding,
                self.u64_type,
                8,
                StorageDefinitionKind::U64,
            );
        }
        if used_types & Type::U32x2 as u32 != 0 {
            self.define_storage_buffer_view(
                &info.storage_buffers_descriptors,
                first_binding,
                self.u32_vec2_type,
                8,
                StorageDefinitionKind::U32x2,
            );
        }
        if used_types & Type::U32x4 as u32 != 0 {
            self.define_storage_buffer_view(
                &info.storage_buffers_descriptors,
                first_binding,
                self.u32_vec4_type,
                16,
                StorageDefinitionKind::U32x4,
            );
        }
        *binding += info
            .storage_buffers_descriptors
            .iter()
            .map(|desc| desc.count)
            .sum::<u32>();
        let needs_function = info.uses_global_increment
            || info.uses_global_decrement
            || info.uses_atomic_f32_add
            || info.uses_atomic_f16x2_add
            || info.uses_atomic_f16x2_min
            || info.uses_atomic_f16x2_max
            || info.uses_atomic_f32x2_add
            || info.uses_atomic_f32x2_min
            || info.uses_atomic_f32x2_max;
        if needs_function {
            self.builder
                .capability(spirv::Capability::VariablePointersStorageBuffer);
        }
        if info.uses_global_increment {
            self.increment_cas_ssbo = self.define_storage_cas_loop(
                StorageCasOperation::Increment,
                self.u32_type,
                self.u32_type,
            );
        }
        if info.uses_global_decrement {
            self.decrement_cas_ssbo = self.define_storage_cas_loop(
                StorageCasOperation::Decrement,
                self.u32_type,
                self.u32_type,
            );
        }
        if info.uses_atomic_f32_add {
            self.f32_add_cas = self.define_storage_cas_loop(
                StorageCasOperation::FpAdd,
                self.f32_type,
                self.u32_type,
            );
        }
        if info.uses_atomic_f16x2_add {
            self.f16x2_add_cas = self.define_storage_cas_loop(
                StorageCasOperation::FpAdd,
                self.f16_vec2_type,
                self.f16_vec2_type,
            );
        }
        if info.uses_atomic_f16x2_min {
            self.f16x2_min_cas = self.define_storage_cas_loop(
                StorageCasOperation::FpMin,
                self.f16_vec2_type,
                self.f16_vec2_type,
            );
        }
        if info.uses_atomic_f16x2_max {
            self.f16x2_max_cas = self.define_storage_cas_loop(
                StorageCasOperation::FpMax,
                self.f16_vec2_type,
                self.f16_vec2_type,
            );
        }
        if info.uses_atomic_f32x2_add {
            self.f32x2_add_cas = self.define_storage_cas_loop(
                StorageCasOperation::FpAdd,
                self.f32_vec2_type,
                self.f32_vec2_type,
            );
        }
        if info.uses_atomic_f32x2_min {
            self.f32x2_min_cas = self.define_storage_cas_loop(
                StorageCasOperation::FpMin,
                self.f32_vec2_type,
                self.f32_vec2_type,
            );
        }
        if info.uses_atomic_f32x2_max {
            self.f32x2_max_cas = self.define_storage_cas_loop(
                StorageCasOperation::FpMax,
                self.f32_vec2_type,
                self.f32_vec2_type,
            );
        }
    }

    /// Port of upstream `EmitContext::DefineAttributeMemAccess`'s `make_load`
    /// lambda.
    fn define_attribute_load_function(&mut self, info: &crate::shader_info::Info) -> spirv::Word {
        use crate::ir::value::Attribute;

        let is_array = self.stage == ShaderStage::Geometry;
        let mut parameters = vec![self.u32_type];
        if is_array {
            parameters.push(self.u32_type);
        }
        let function_type = self.builder.type_function(self.f32_type, parameters);
        let function = self
            .builder
            .begin_function(
                self.f32_type,
                None,
                spirv::FunctionControl::NONE,
                function_type,
            )
            .unwrap();
        let offset = self.builder.function_parameter(self.u32_type).unwrap();
        let vertex = is_array.then(|| self.builder.function_parameter(self.u32_type).unwrap());
        let end_label = self.builder.id();
        let default_label = self.builder.id();

        self.builder.begin_block(None).unwrap();
        let two = self.constant_u32(2);
        let three = self.constant_u32(3);
        let base_index = self
            .builder
            .shift_right_arithmetic(self.u32_type, None, offset, two)
            .unwrap();
        let masked_index = self
            .builder
            .bitwise_and(self.u32_type, None, base_index, three)
            .unwrap();
        let compare_index = self
            .builder
            .shift_right_arithmetic(self.u32_type, None, base_index, two)
            .unwrap();

        let mut cases = Vec::new();
        let position_loaded = info.loads.any_component(Attribute::POSITION_X.0 as usize);
        if position_loaded {
            cases.push((
                Operand::LiteralBit32(Attribute::POSITION_X.0 >> 2),
                self.builder.id(),
            ));
        }
        let generic_base = Attribute::generic(0, 0).0 >> 2;
        for index in 0..32 {
            if info.loads.generic_any(index) {
                cases.push((
                    Operand::LiteralBit32(generic_base + index as u32),
                    self.builder.id(),
                ));
            }
        }
        self.builder
            .selection_merge(end_label, spirv::SelectionControl::NONE)
            .unwrap();
        self.builder
            .switch(compare_index, default_label, cases.iter().cloned())
            .unwrap();

        self.builder.begin_block(Some(default_label)).unwrap();
        self.builder.ret_value(self.const_zero_f32).unwrap();

        let mut label_index = 0;
        if position_loaded {
            self.builder
                .begin_block(Some(cases[label_index].1))
                .unwrap();
            let mut indices = Vec::new();
            if let Some(vertex) = vertex {
                indices.push(vertex);
            }
            if self.need_input_position_indirect {
                indices.push(self.const_zero_u32);
            }
            indices.push(masked_index);
            let pointer = self
                .builder
                .access_chain(self.input_f32_ptr, None, self.input_position, indices)
                .unwrap();
            let result = self
                .builder
                .load(self.f32_type, None, pointer, None, [])
                .unwrap();
            self.builder.ret_value(result).unwrap();
            label_index += 1;
        }
        for index in 0..32 {
            if !info.loads.generic_any(index) {
                continue;
            }
            self.builder
                .begin_block(Some(cases[label_index].1))
                .unwrap();
            let generic = self.input_generics[index];
            if generic.id == 0 {
                self.builder.ret_value(self.const_zero_f32).unwrap();
                label_index += 1;
                continue;
            }
            let mut indices = Vec::new();
            if let Some(vertex) = vertex {
                indices.push(vertex);
            }
            indices.push(masked_index);
            let pointer = self
                .builder
                .access_chain(generic.pointer_type, None, generic.id, indices)
                .unwrap();
            let value = self
                .builder
                .load(generic.component_type, None, pointer, None, [])
                .unwrap();
            let result = match generic.load_op {
                InputGenericLoadOp::None => value,
                InputGenericLoadOp::Bitcast => {
                    self.builder.bitcast(self.f32_type, None, value).unwrap()
                }
                InputGenericLoadOp::SToF => self
                    .builder
                    .convert_s_to_f(self.f32_type, None, value)
                    .unwrap(),
                InputGenericLoadOp::UToF => self
                    .builder
                    .convert_u_to_f(self.f32_type, None, value)
                    .unwrap(),
            };
            self.builder.ret_value(result).unwrap();
            label_index += 1;
        }
        self.builder.begin_block(Some(end_label)).unwrap();
        self.builder.unreachable().unwrap();
        self.builder.end_function().unwrap();
        function
    }

    /// Port of upstream `EmitContext::DefineAttributeMemAccess`'s
    /// `make_store` lambda.
    fn define_attribute_store_function(&mut self, info: &crate::shader_info::Info) -> spirv::Word {
        use crate::ir::value::Attribute;

        let function_type = self
            .builder
            .type_function(self.void_type, vec![self.u32_type, self.f32_type]);
        let function = self
            .builder
            .begin_function(
                self.void_type,
                None,
                spirv::FunctionControl::NONE,
                function_type,
            )
            .unwrap();
        let offset = self.builder.function_parameter(self.u32_type).unwrap();
        let store_value = self.builder.function_parameter(self.f32_type).unwrap();
        let end_label = self.builder.id();
        let default_label = self.builder.id();

        self.builder.begin_block(None).unwrap();
        let two = self.constant_u32(2);
        let three = self.constant_u32(3);
        let base_index = self
            .builder
            .shift_right_arithmetic(self.u32_type, None, offset, two)
            .unwrap();
        let masked_index = self
            .builder
            .bitwise_and(self.u32_type, None, base_index, three)
            .unwrap();
        let compare_index = self
            .builder
            .shift_right_arithmetic(self.u32_type, None, base_index, two)
            .unwrap();

        let mut cases = Vec::new();
        let position_stored = info.stores.any_component(Attribute::POSITION_X.0 as usize);
        if position_stored {
            cases.push((
                Operand::LiteralBit32(Attribute::POSITION_X.0 >> 2),
                self.builder.id(),
            ));
        }
        let generic_base = Attribute::generic(0, 0).0 >> 2;
        for index in 0..32 {
            if info.stores.generic_any(index) {
                cases.push((
                    Operand::LiteralBit32(generic_base + index as u32),
                    self.builder.id(),
                ));
            }
        }
        let clip_distances_stored = info.stores.clip_distances();
        if clip_distances_stored && self.profile.max_user_clip_distances >= 4 {
            cases.push((
                Operand::LiteralBit32(Attribute::CLIP_DISTANCE_0.0 >> 2),
                self.builder.id(),
            ));
        }
        if clip_distances_stored && self.profile.max_user_clip_distances >= 8 {
            cases.push((
                Operand::LiteralBit32((Attribute::CLIP_DISTANCE_0.0 + 4) >> 2),
                self.builder.id(),
            ));
        }
        self.builder
            .selection_merge(end_label, spirv::SelectionControl::NONE)
            .unwrap();
        self.builder
            .switch(compare_index, default_label, cases.iter().cloned())
            .unwrap();

        self.builder.begin_block(Some(default_label)).unwrap();
        self.builder.ret().unwrap();

        let mut label_index = 0;
        if position_stored {
            self.builder
                .begin_block(Some(cases[label_index].1))
                .unwrap();
            let pointer = self
                .builder
                .access_chain(
                    self.output_f32_ptr,
                    None,
                    self.output_position,
                    vec![masked_index],
                )
                .unwrap();
            self.builder.store(pointer, store_value, None, []).unwrap();
            self.builder.ret().unwrap();
            label_index += 1;
        }
        for index in 0..32 {
            if !info.stores.generic_any(index) {
                continue;
            }
            let generic = self.output_generics[index][0];
            assert_eq!(
                generic.num_components, 4,
                "physical stores and transform feedbacks"
            );
            self.builder
                .begin_block(Some(cases[label_index].1))
                .unwrap();
            let pointer = self
                .builder
                .access_chain(self.output_f32_ptr, None, generic.id, vec![masked_index])
                .unwrap();
            self.builder.store(pointer, store_value, None, []).unwrap();
            self.builder.ret().unwrap();
            label_index += 1;
        }
        if clip_distances_stored && self.profile.max_user_clip_distances >= 4 {
            self.builder
                .begin_block(Some(cases[label_index].1))
                .unwrap();
            let pointer = self
                .builder
                .access_chain(
                    self.output_f32_ptr,
                    None,
                    self.clip_distances,
                    vec![masked_index],
                )
                .unwrap();
            self.builder.store(pointer, store_value, None, []).unwrap();
            self.builder.ret().unwrap();
            label_index += 1;
        }
        if clip_distances_stored && self.profile.max_user_clip_distances >= 8 {
            self.builder
                .begin_block(Some(cases[label_index].1))
                .unwrap();
            let four = self.constant_u32(4);
            let fixed_index = self
                .builder
                .i_add(self.u32_type, None, masked_index, four)
                .unwrap();
            let pointer = self
                .builder
                .access_chain(
                    self.output_f32_ptr,
                    None,
                    self.clip_distances,
                    vec![fixed_index],
                )
                .unwrap();
            self.builder.store(pointer, store_value, None, []).unwrap();
            self.builder.ret().unwrap();
        }
        self.builder.begin_block(Some(end_label)).unwrap();
        self.builder.unreachable().unwrap();
        self.builder.end_function().unwrap();
        function
    }

    /// Port of upstream `EmitContext::DefineAttributeMemAccess`.
    fn define_attribute_mem_access(&mut self, info: &crate::shader_info::Info) {
        if info.loads_indexed_attributes {
            self.indexed_load_func = self.define_attribute_load_function(info);
        }
        if info.stores_indexed_attributes {
            self.indexed_store_func = self.define_attribute_store_function(info);
        }
    }

    /// Port of upstream `EmitContext::DefineWriteStorageCasLoopFunction`.
    fn define_write_storage_cas_loop_function(&mut self, info: &crate::shader_info::Info) {
        if self.profile.support_int8 && self.profile.support_int16 {
            return;
        }
        if !info.uses_int8 && !info.uses_int16 {
            return;
        }
        self.builder
            .capability(spirv::Capability::VariablePointersStorageBuffer);
        let pointer_type =
            self.builder
                .type_pointer(None, spirv::StorageClass::StorageBuffer, self.u32_type);
        let function_type = self.builder.type_function(
            self.void_type,
            vec![pointer_type, self.u32_type, self.u32_type, self.u32_type],
        );
        let function = self
            .builder
            .begin_function(
                self.void_type,
                None,
                spirv::FunctionControl::NONE,
                function_type,
            )
            .unwrap();
        let pointer = self.builder.function_parameter(pointer_type).unwrap();
        let value = self.builder.function_parameter(self.u32_type).unwrap();
        let bit_offset = self.builder.function_parameter(self.u32_type).unwrap();
        let bit_count = self.builder.function_parameter(self.u32_type).unwrap();
        let body_label = self.builder.id();
        let continue_label = self.builder.id();
        let end_label = self.builder.id();
        let begin_label = self.builder.id();

        self.builder.begin_block(None).unwrap();
        self.builder.branch(begin_label).unwrap();
        self.builder.begin_block(Some(begin_label)).unwrap();
        self.builder
            .loop_merge(end_label, continue_label, spirv::LoopControl::NONE, [])
            .unwrap();
        self.builder.branch(body_label).unwrap();
        self.builder.begin_block(Some(body_label)).unwrap();
        let expected = self
            .builder
            .load(self.u32_type, None, pointer, None, [])
            .unwrap();
        let desired = self
            .builder
            .bit_field_insert(self.u32_type, None, expected, value, bit_offset, bit_count)
            .unwrap();
        let actual = self
            .builder
            .atomic_compare_exchange(
                self.u32_type,
                None,
                pointer,
                self.const_one_u32,
                self.const_zero_u32,
                self.const_zero_u32,
                desired,
                expected,
            )
            .unwrap();
        let successful = self
            .builder
            .i_equal(self.bool_type, None, expected, actual)
            .unwrap();
        self.builder
            .branch_conditional(successful, end_label, continue_label, [])
            .unwrap();
        self.builder.begin_block(Some(end_label)).unwrap();
        self.builder.ret().unwrap();
        self.builder.begin_block(Some(continue_label)).unwrap();
        self.builder.branch(begin_label).unwrap();
        self.builder.end_function().unwrap();
        self.write_storage_cas_loop_func = function;
    }

    fn define_global_memory_function(
        &mut self,
        info: &crate::shader_info::Info,
        kind: StorageDefinitionKind,
        value_type: spirv::Word,
        element_size: u32,
        write: bool,
    ) -> spirv::Word {
        let function_type = if write {
            self.builder
                .type_function(self.void_type, vec![self.u64_type, value_type])
        } else {
            self.builder.type_function(value_type, vec![self.u64_type])
        };
        let result_type = if write { self.void_type } else { value_type };
        let function = self
            .builder
            .begin_function(
                result_type,
                None,
                spirv::FunctionControl::NONE,
                function_type,
            )
            .unwrap();
        let address = self.builder.function_parameter(self.u64_type).unwrap();
        let data = write.then(|| self.builder.function_parameter(value_type).unwrap());
        self.builder.begin_block(None).unwrap();

        let element_pointer = self.storage_types.get(kind).element;
        assert_ne!(element_pointer, 0, "missing global SSBO type {kind:?}");
        let shift = element_size.trailing_zeros();
        for (index, descriptor) in info.storage_buffers_descriptors.iter().enumerate() {
            if index >= u16::BITS as usize || info.nvn_buffer_used & (1u16 << index) == 0 {
                continue;
            }
            let cbuf = self
                .cbufs
                .get(&descriptor.cbuf_index)
                .copied()
                .unwrap_or_default();
            assert_ne!(cbuf.u32x2, 0, "missing NVN address CBUF view");
            assert_ne!(cbuf.u32_scalar, 0, "missing NVN size CBUF view");
            let address_offset = self.constant_u32(descriptor.cbuf_offset / 8);
            let size_offset = self.constant_u32(descriptor.cbuf_offset / 4 + 2);
            let address_pointer = self
                .builder
                .access_chain(
                    self.uniform_types.u32x2,
                    None,
                    cbuf.u32x2,
                    vec![self.const_zero_u32, address_offset],
                )
                .unwrap();
            let size_pointer = self
                .builder
                .access_chain(
                    self.uniform_types.u32_scalar,
                    None,
                    cbuf.u32_scalar,
                    vec![self.const_zero_u32, size_offset],
                )
                .unwrap();
            let address_words = self
                .builder
                .load(self.u32_vec2_type, None, address_pointer, None, [])
                .unwrap();
            let unaligned_address = self
                .builder
                .bitcast(self.u64_type, None, address_words)
                .unwrap();
            let alignment_mask = self.builder.constant_bit64(
                self.u64_type,
                !self.profile.min_ssbo_alignment.wrapping_sub(1),
            );
            let ssbo_address = self
                .builder
                .bitwise_and(self.u64_type, None, unaligned_address, alignment_mask)
                .unwrap();
            let size = self
                .builder
                .load(self.u32_type, None, size_pointer, None, [])
                .unwrap();
            let size = self.builder.u_convert(self.u64_type, None, size).unwrap();
            let ssbo_end = self
                .builder
                .i_add(self.u64_type, None, ssbo_address, size)
                .unwrap();
            let at_or_after_start = self
                .builder
                .u_greater_than_equal(self.bool_type, None, address, ssbo_address)
                .unwrap();
            let before_end = self
                .builder
                .u_less_than(self.bool_type, None, address, ssbo_end)
                .unwrap();
            let in_range = self
                .builder
                .logical_and(self.bool_type, None, at_or_after_start, before_end)
                .unwrap();
            let then_label = self.builder.id();
            let else_label = self.builder.id();
            self.builder
                .selection_merge(else_label, spirv::SelectionControl::NONE)
                .unwrap();
            self.builder
                .branch_conditional(in_range, then_label, else_label, [])
                .unwrap();
            self.builder.begin_block(Some(then_label)).unwrap();
            let ssbo = self
                .ssbos
                .get(&(index as u32))
                .copied()
                .unwrap_or_default()
                .get(kind);
            assert_ne!(ssbo, 0, "missing global SSBO {index} view {kind:?}");
            let byte_offset = self
                .builder
                .i_sub(self.u64_type, None, address, ssbo_address)
                .unwrap();
            let byte_offset = self
                .builder
                .u_convert(self.u32_type, None, byte_offset)
                .unwrap();
            let shift = self.constant_u32(shift);
            let ssbo_index = self
                .builder
                .shift_right_logical(self.u32_type, None, byte_offset, shift)
                .unwrap();
            let pointer = self
                .builder
                .access_chain(
                    element_pointer,
                    None,
                    ssbo,
                    vec![self.const_zero_u32, ssbo_index],
                )
                .unwrap();
            if let Some(data) = data {
                self.builder.store(pointer, data, None, []).unwrap();
                self.builder.ret().unwrap();
            } else {
                let value = self
                    .builder
                    .load(value_type, None, pointer, None, [])
                    .unwrap();
                self.builder.ret_value(value).unwrap();
            }
            self.builder.begin_block(Some(else_label)).unwrap();
        }
        if write {
            self.builder.ret().unwrap();
        } else {
            let zero = self.builder.constant_null(value_type);
            self.builder.ret_value(zero).unwrap();
        }
        self.builder.end_function().unwrap();
        function
    }

    /// Port of upstream `EmitContext::DefineGlobalMemoryFunctions`.
    fn define_global_memory_functions(&mut self, info: &crate::shader_info::Info) {
        if !info.uses_global_memory || !self.profile.support_int64 {
            return;
        }
        self.load_global_func_u32 = self.define_global_memory_function(
            info,
            StorageDefinitionKind::U32,
            self.u32_type,
            4,
            false,
        );
        self.write_global_func_u32 = self.define_global_memory_function(
            info,
            StorageDefinitionKind::U32,
            self.u32_type,
            4,
            true,
        );
        self.load_global_func_u32x2 = self.define_global_memory_function(
            info,
            StorageDefinitionKind::U32x2,
            self.u32_vec2_type,
            8,
            false,
        );
        self.write_global_func_u32x2 = self.define_global_memory_function(
            info,
            StorageDefinitionKind::U32x2,
            self.u32_vec2_type,
            8,
            true,
        );
        self.load_global_func_u32x4 = self.define_global_memory_function(
            info,
            StorageDefinitionKind::U32x4,
            self.u32_vec4_type,
            16,
            false,
        );
        self.write_global_func_u32x4 = self.define_global_memory_function(
            info,
            StorageDefinitionKind::U32x4,
            self.u32_vec4_type,
            16,
            true,
        );
    }

    /// Port of upstream `EmitContext::DefineTextureBuffers`.
    fn define_texture_buffers(
        &mut self,
        descriptors: &[TextureBufferDescriptor],
        binding: &mut u32,
    ) {
        if descriptors.is_empty() {
            return;
        }
        self.image_buffer_type = self.builder.type_image(
            self.f32_type,
            spirv::Dim::DimBuffer,
            0,
            0,
            0,
            1,
            spirv::ImageFormat::Unknown,
            None,
        );
        let pointer_type = self.builder.type_pointer(
            None,
            spirv::StorageClass::UniformConstant,
            self.image_buffer_type,
        );
        self.texture_buffers.reserve(descriptors.len());
        for desc in descriptors {
            assert_eq!(desc.count, 1, "SPIR-V: array of texture buffers");
            let id = self.builder.variable(
                pointer_type,
                None,
                spirv::StorageClass::UniformConstant,
                None,
            );
            self.builder.decorate(
                id,
                spirv::Decoration::Binding,
                vec![Operand::LiteralBit32(*binding)],
            );
            self.builder.decorate(
                id,
                spirv::Decoration::DescriptorSet,
                vec![Operand::LiteralBit32(0)],
            );
            self.texture_buffers.push(TextureBufferDefinition {
                id,
                count: desc.count,
            });
            if self.profile.supported_spirv >= 0x0001_0400 {
                self.interfaces.push(id);
            }
            *binding += 1;
        }
    }

    /// Port of upstream `EmitContext::DefineImageBuffers`.
    fn define_image_buffers(&mut self, descriptors: &[ImageBufferDescriptor], binding: &mut u32) {
        self.image_buffers.reserve(descriptors.len());
        for desc in descriptors {
            let sampled_type = if desc.is_integer {
                self.u32_type
            } else {
                self.f32_type
            };
            let image_type = self.builder.type_image(
                sampled_type,
                spirv::Dim::DimBuffer,
                0,
                0,
                0,
                2,
                image_format(desc.format),
                None,
            );
            let pointer_type =
                self.builder
                    .type_pointer(None, spirv::StorageClass::UniformConstant, image_type);
            let id = self.builder.variable(
                pointer_type,
                None,
                spirv::StorageClass::UniformConstant,
                None,
            );
            self.builder.decorate(
                id,
                spirv::Decoration::Binding,
                vec![Operand::LiteralBit32(*binding)],
            );
            self.builder.decorate(
                id,
                spirv::Decoration::DescriptorSet,
                vec![Operand::LiteralBit32(0)],
            );
            self.image_buffers.push(ImageBufferDefinition {
                id,
                image_type,
                pointer_type,
                count: desc.count,
                is_integer: desc.is_integer,
            });
            if self.profile.supported_spirv >= 0x0001_0400 {
                self.interfaces.push(id);
            }
            *binding += 1;
        }
    }

    /// Port of upstream `EmitContext::DefineImages`.
    fn define_images(&mut self, descriptors: &[ImageDescriptor], binding: &mut u32) {
        self.images.reserve(descriptors.len());
        for desc in descriptors {
            let sampled_type = if desc.is_integer {
                self.u32_type
            } else {
                self.f32_type
            };
            let image_type = storage_image_type(self, desc, sampled_type);
            let pointer_type =
                self.builder
                    .type_pointer(None, spirv::StorageClass::UniformConstant, image_type);
            let id = self.builder.variable(
                pointer_type,
                None,
                spirv::StorageClass::UniformConstant,
                None,
            );
            self.builder.decorate(
                id,
                spirv::Decoration::Binding,
                vec![Operand::LiteralBit32(*binding)],
            );
            self.builder.decorate(
                id,
                spirv::Decoration::DescriptorSet,
                vec![Operand::LiteralBit32(0)],
            );
            self.images.push(ImageDefinition {
                id,
                image_type,
                pointer_type,
                count: desc.count,
                is_integer: desc.is_integer,
            });
            if self.profile.supported_spirv >= 0x0001_0400 {
                self.interfaces.push(id);
            }
            *binding += 1;
        }
    }

    /// Port of upstream `EmitContext::DefineLocalMemory`.
    fn define_local_memory(&mut self, program: &ir::Program) {
        if program.local_memory_size == 0 {
            return;
        }
        let num_elements = program.local_memory_size.div_ceil(4);
        let count = self.constant_u32(num_elements);
        let array_type = self.builder.type_array(self.u32_type, count);
        let pointer_type =
            self.builder
                .type_pointer(None, spirv::StorageClass::Private, array_type);
        self.local_memory =
            self.builder
                .variable(pointer_type, None, spirv::StorageClass::Private, None);
        if self.profile.supported_spirv >= 0x0001_0400 {
            self.interfaces.push(self.local_memory);
        }
    }

    fn define_explicit_shared_memory(
        &mut self,
        element_type: spirv::Word,
        element_size: u32,
        shared_memory_size: u32,
    ) -> (spirv::Word, spirv::Word, spirv::Word) {
        let num_elements = shared_memory_size.div_ceil(element_size);
        let count = self.constant_u32(num_elements);
        let array_type = self.builder.type_array(element_type, count);
        self.builder.decorate(
            array_type,
            spirv::Decoration::ArrayStride,
            vec![Operand::LiteralBit32(element_size)],
        );
        let struct_type = self.builder.type_struct(vec![array_type]);
        self.builder.member_decorate(
            struct_type,
            0,
            spirv::Decoration::Offset,
            vec![Operand::LiteralBit32(0)],
        );
        self.builder
            .decorate(struct_type, spirv::Decoration::Block, vec![]);
        let pointer_type =
            self.builder
                .type_pointer(None, spirv::StorageClass::Workgroup, struct_type);
        let element_pointer =
            self.builder
                .type_pointer(None, spirv::StorageClass::Workgroup, element_type);
        let variable =
            self.builder
                .variable(pointer_type, None, spirv::StorageClass::Workgroup, None);
        self.builder
            .decorate(variable, spirv::Decoration::Aliased, vec![]);
        self.interfaces.push(variable);
        (variable, element_pointer, pointer_type)
    }

    fn define_shared_subword_store_function(&mut self, mask: u32, size: u32) -> spirv::Word {
        let function_type = self
            .builder
            .type_function(self.void_type, vec![self.u32_type, self.u32_type]);
        let loop_header = self.builder.id();
        let continue_block = self.builder.id();
        let merge_block = self.builder.id();
        let function = self
            .builder
            .begin_function(
                self.void_type,
                None,
                spirv::FunctionControl::NONE,
                function_type,
            )
            .unwrap();
        let offset = self.builder.function_parameter(self.u32_type).unwrap();
        let insert_value = self.builder.function_parameter(self.u32_type).unwrap();
        self.builder.begin_block(None).unwrap();
        self.builder.branch(loop_header).unwrap();

        self.builder.begin_block(Some(loop_header)).unwrap();
        let two = self.constant_u32(2);
        let three = self.constant_u32(3);
        let mask_id = self.constant_u32(mask);
        let count = self.constant_u32(size);
        let word_offset = self
            .builder
            .shift_right_arithmetic(self.u32_type, None, offset, two)
            .unwrap();
        let shift_offset = self
            .builder
            .shift_left_logical(self.u32_type, None, offset, three)
            .unwrap();
        let bit_offset = self
            .builder
            .bitwise_and(self.u32_type, None, shift_offset, mask_id)
            .unwrap();
        self.builder
            .loop_merge(merge_block, continue_block, spirv::LoopControl::NONE, [])
            .unwrap();
        self.builder.branch(continue_block).unwrap();

        self.builder.begin_block(Some(continue_block)).unwrap();
        let word_pointer = self
            .builder
            .access_chain(
                self.shared_u32,
                None,
                self.shared_memory_u32,
                vec![word_offset],
            )
            .unwrap();
        let old_value = self
            .builder
            .load(self.u32_type, None, word_pointer, None, vec![])
            .unwrap();
        let new_value = self
            .builder
            .bit_field_insert(
                self.u32_type,
                None,
                old_value,
                insert_value,
                bit_offset,
                count,
            )
            .unwrap();
        let atomic_res = self
            .builder
            .atomic_compare_exchange(
                self.u32_type,
                None,
                word_pointer,
                self.const_one_u32,
                self.const_zero_u32,
                self.const_zero_u32,
                new_value,
                old_value,
            )
            .unwrap();
        let success = self
            .builder
            .i_equal(self.bool_type, None, atomic_res, old_value)
            .unwrap();
        self.builder
            .branch_conditional(success, merge_block, loop_header, [])
            .unwrap();

        self.builder.begin_block(Some(merge_block)).unwrap();
        self.builder.ret().unwrap();
        self.builder.end_function().unwrap();
        function
    }

    /// Port of upstream `EmitContext::DefineSharedMemory`.
    fn define_shared_memory(&mut self, program: &ir::Program) {
        // Upstream computes this before the early return: the flag is read back
        // by the shared 64-bit atomic emitters even when no shared memory is
        // declared.
        self.uses_explicit_workgroup_layout = self.profile.support_explicit_workgroup_layout
            && (!program.info.uses_int8 || self.profile.support_workgroup_layout_8bit_access)
            && (!program.info.uses_int16 || self.profile.support_workgroup_layout_16bit_access);
        if program.shared_memory_size == 0 {
            return;
        }
        if self.uses_explicit_workgroup_layout {
            self.builder
                .extension("SPV_KHR_workgroup_memory_explicit_layout");
            self.builder
                .capability(spirv::Capability::WorkgroupMemoryExplicitLayoutKHR);
            if program.info.uses_int8 && self.profile.support_int8 {
                self.builder
                    .capability(spirv::Capability::WorkgroupMemoryExplicitLayout8BitAccessKHR);
                (self.shared_memory_u8, self.shared_u8, _) =
                    self.define_explicit_shared_memory(self.u8_type, 1, program.shared_memory_size);
            }
            if program.info.uses_int16 && self.profile.support_int16 {
                self.builder
                    .capability(spirv::Capability::WorkgroupMemoryExplicitLayout16BitAccessKHR);
                (self.shared_memory_u16, self.shared_u16, _) = self.define_explicit_shared_memory(
                    self.u16_type,
                    2,
                    program.shared_memory_size,
                );
            }
            if program.info.uses_int64 && self.profile.support_int64 {
                (self.shared_memory_u64, self.shared_u64, _) = self.define_explicit_shared_memory(
                    self.u64_type,
                    8,
                    program.shared_memory_size,
                );
            }
            (
                self.shared_memory_u32,
                self.shared_u32,
                self.shared_memory_u32_type,
            ) = self.define_explicit_shared_memory(self.u32_type, 4, program.shared_memory_size);
            (self.shared_memory_u32x2, self.shared_u32x2, _) = self.define_explicit_shared_memory(
                self.u32_vec2_type,
                8,
                program.shared_memory_size,
            );
            (self.shared_memory_u32x4, self.shared_u32x4, _) = self.define_explicit_shared_memory(
                self.u32_vec4_type,
                16,
                program.shared_memory_size,
            );
            return;
        }

        let num_elements = program.shared_memory_size.div_ceil(4);
        let count = self.constant_u32(num_elements);
        let array_type = self.builder.type_array(self.u32_type, count);
        self.shared_memory_u32_type =
            self.builder
                .type_pointer(None, spirv::StorageClass::Workgroup, array_type);
        self.shared_u32 =
            self.builder
                .type_pointer(None, spirv::StorageClass::Workgroup, self.u32_type);
        self.shared_memory_u32 = self.builder.variable(
            self.shared_memory_u32_type,
            None,
            spirv::StorageClass::Workgroup,
            None,
        );
        self.interfaces.push(self.shared_memory_u32);
        if program.info.uses_int8 {
            self.shared_store_u8_func = self.define_shared_subword_store_function(24, 8);
        }
        if program.info.uses_int16 {
            self.shared_store_u16_func = self.define_shared_subword_store_function(16, 16);
        }
    }

    fn define_shared_cas_operation(&mut self, increment: bool) -> spirv::Word {
        let function_type = self
            .builder
            .type_function(self.u32_type, vec![self.u32_type, self.u32_type]);
        let function = self
            .builder
            .begin_function(
                self.u32_type,
                None,
                spirv::FunctionControl::NONE,
                function_type,
            )
            .unwrap();
        let op_a = self.builder.function_parameter(self.u32_type).unwrap();
        let op_b = self.builder.function_parameter(self.u32_type).unwrap();
        self.builder.begin_block(None).unwrap();
        let result = if increment {
            let pred = self
                .builder
                .u_greater_than_equal(self.bool_type, None, op_a, op_b)
                .unwrap();
            let incr = self
                .builder
                .i_add(self.u32_type, None, op_a, self.const_one_u32)
                .unwrap();
            self.builder
                .select(self.u32_type, None, pred, self.const_zero_u32, incr)
                .unwrap()
        } else {
            let lhs = self
                .builder
                .i_equal(self.bool_type, None, op_a, self.const_zero_u32)
                .unwrap();
            let rhs = self
                .builder
                .u_greater_than(self.bool_type, None, op_a, op_b)
                .unwrap();
            let pred = self
                .builder
                .logical_or(self.bool_type, None, lhs, rhs)
                .unwrap();
            let decr = self
                .builder
                .i_sub(self.u32_type, None, op_a, self.const_one_u32)
                .unwrap();
            self.builder
                .select(self.u32_type, None, pred, op_b, decr)
                .unwrap()
        };
        self.builder.ret_value(result).unwrap();
        self.builder.end_function().unwrap();
        function
    }

    fn define_shared_cas_loop(&mut self, increment: bool) -> spirv::Word {
        let operation = self.define_shared_cas_operation(increment);
        let function_type = self
            .builder
            .type_function(self.u32_type, vec![self.u32_type, self.u32_type]);
        let loop_header = self.builder.id();
        let continue_block = self.builder.id();
        let merge_block = self.builder.id();
        let function = self
            .builder
            .begin_function(
                self.u32_type,
                None,
                spirv::FunctionControl::NONE,
                function_type,
            )
            .unwrap();
        let index = self.builder.function_parameter(self.u32_type).unwrap();
        let op_b = self.builder.function_parameter(self.u32_type).unwrap();
        self.builder.begin_block(None).unwrap();
        self.builder.branch(loop_header).unwrap();

        self.builder.begin_block(Some(loop_header)).unwrap();
        self.builder
            .loop_merge(merge_block, continue_block, spirv::LoopControl::NONE, [])
            .unwrap();
        self.builder.branch(continue_block).unwrap();

        self.builder.begin_block(Some(continue_block)).unwrap();
        let indices = if self.profile.support_explicit_workgroup_layout {
            vec![self.const_zero_u32, index]
        } else {
            vec![index]
        };
        let word_pointer = self
            .builder
            .access_chain(self.shared_u32, None, self.shared_memory_u32, indices)
            .unwrap();
        let value = self
            .builder
            .load(self.u32_type, None, word_pointer, None, vec![])
            .unwrap();
        let new_value = self
            .builder
            .function_call(self.u32_type, None, operation, vec![value, op_b])
            .unwrap();
        let scope = self.constant_u32(spirv::Scope::Workgroup as u32);
        let atomic_res = self
            .builder
            .atomic_compare_exchange(
                self.u32_type,
                None,
                word_pointer,
                scope,
                self.const_zero_u32,
                self.const_zero_u32,
                new_value,
                value,
            )
            .unwrap();
        let success = self
            .builder
            .i_equal(self.bool_type, None, atomic_res, value)
            .unwrap();
        self.builder
            .branch_conditional(success, merge_block, loop_header, [])
            .unwrap();

        self.builder.begin_block(Some(merge_block)).unwrap();
        self.builder.ret_value(atomic_res).unwrap();
        self.builder.end_function().unwrap();
        function
    }

    /// Port of upstream `EmitContext::DefineSharedMemoryFunctions`.
    fn define_shared_memory_functions(&mut self, program: &ir::Program) {
        if program.info.uses_shared_increment {
            self.increment_cas_shared = self.define_shared_cas_loop(true);
        }
        if program.info.uses_shared_decrement {
            self.decrement_cas_shared = self.define_shared_cas_loop(false);
        }
    }

    fn define_variable(
        &mut self,
        value_type: spirv::Word,
        built_in: Option<spirv::BuiltIn>,
        storage_class: spirv::StorageClass,
        initializer: Option<spirv::Word>,
    ) -> spirv::Word {
        let pointer_type = self.builder.type_pointer(None, storage_class, value_type);
        let id = self
            .builder
            .variable(pointer_type, None, storage_class, initializer);
        if let Some(built_in) = built_in {
            self.builder.decorate(
                id,
                spirv::Decoration::BuiltIn,
                vec![Operand::BuiltIn(built_in)],
            );
        }
        self.interfaces.push(id);
        id
    }

    fn input_vertices(&self) -> u32 {
        match self.runtime_info.input_topology {
            InputTopology::Points => 1,
            InputTopology::Lines => 2,
            InputTopology::LinesAdjacency => 4,
            InputTopology::Triangles => 3,
            InputTopology::TrianglesAdjacency => 6,
        }
    }

    fn define_input(
        &mut self,
        mut value_type: spirv::Word,
        per_invocation: bool,
        built_in: Option<spirv::BuiltIn>,
    ) -> spirv::Word {
        if per_invocation {
            let count = match self.stage {
                ShaderStage::TessellationControl | ShaderStage::TessellationEval => Some(32),
                ShaderStage::Geometry => Some(self.input_vertices()),
                _ => None,
            };
            if let Some(count) = count {
                let count = self.constant_u32(count);
                value_type = self.builder.type_array(value_type, count);
            }
        }
        self.define_variable(value_type, built_in, spirv::StorageClass::Input, None)
    }

    fn define_output(
        &mut self,
        mut value_type: spirv::Word,
        invocations: Option<u32>,
        built_in: Option<spirv::BuiltIn>,
        initializer: Option<spirv::Word>,
    ) -> spirv::Word {
        if self.stage == ShaderStage::TessellationControl {
            if let Some(invocations) = invocations {
                let count = self.constant_u32(invocations);
                value_type = self.builder.type_array(value_type, count);
            }
        }
        self.define_variable(
            value_type,
            built_in,
            spirv::StorageClass::Output,
            initializer,
        )
    }

    fn f32_vector_type(&self, components: u32) -> spirv::Word {
        match components {
            1 => self.f32_type,
            2 => self.f32_vec2_type,
            3 => self.f32_vec3_type,
            4 => self.f32_vec4_type,
            _ => panic!("invalid F32 vector component count {components}"),
        }
    }

    fn attribute_type(&self, attribute_type: AttributeType) -> spirv::Word {
        match attribute_type {
            AttributeType::Float => self.f32_vec4_type,
            AttributeType::SignedInt => self.i32_vec4_type,
            AttributeType::UnsignedInt => self.u32_vec4_type,
            AttributeType::SignedScaled => {
                if self.profile.support_scaled_attributes {
                    self.f32_vec4_type
                } else {
                    self.i32_vec4_type
                }
            }
            AttributeType::UnsignedScaled => {
                if self.profile.support_scaled_attributes {
                    self.f32_vec4_type
                } else {
                    self.u32_vec4_type
                }
            }
            AttributeType::Disabled => panic!("disabled attribute has no SPIR-V type"),
        }
    }

    fn attribute_info(&self, attribute_type: AttributeType, id: spirv::Word) -> InputGenericInfo {
        match attribute_type {
            AttributeType::Float => InputGenericInfo {
                id,
                pointer_type: self.input_f32_ptr,
                component_type: self.f32_type,
                load_op: InputGenericLoadOp::None,
            },
            AttributeType::UnsignedInt => InputGenericInfo {
                id,
                pointer_type: self.input_u32_ptr,
                component_type: self.u32_type,
                load_op: InputGenericLoadOp::Bitcast,
            },
            AttributeType::SignedInt => InputGenericInfo {
                id,
                pointer_type: self.input_i32_ptr,
                component_type: self.i32_type,
                load_op: InputGenericLoadOp::Bitcast,
            },
            AttributeType::SignedScaled if self.profile.support_scaled_attributes => {
                InputGenericInfo {
                    id,
                    pointer_type: self.input_f32_ptr,
                    component_type: self.f32_type,
                    load_op: InputGenericLoadOp::None,
                }
            }
            AttributeType::SignedScaled => InputGenericInfo {
                id,
                pointer_type: self.input_i32_ptr,
                component_type: self.i32_type,
                load_op: InputGenericLoadOp::SToF,
            },
            AttributeType::UnsignedScaled if self.profile.support_scaled_attributes => {
                InputGenericInfo {
                    id,
                    pointer_type: self.input_f32_ptr,
                    component_type: self.f32_type,
                    load_op: InputGenericLoadOp::None,
                }
            }
            AttributeType::UnsignedScaled => InputGenericInfo {
                id,
                pointer_type: self.input_u32_ptr,
                component_type: self.u32_type,
                load_op: InputGenericLoadOp::UToF,
            },
            AttributeType::Disabled => InputGenericInfo::default(),
        }
    }

    fn define_generic_output(&mut self, index: usize, invocations: Option<u32>) {
        let base_attribute = ir::value::Attribute::generic(index as u32, 0).0 as usize;
        let mut element = 0u32;
        while element < 4 {
            let remainder = 4 - element;
            let varying_index = base_attribute + element as usize;
            let xfb = (varying_index < self.runtime_info.xfb_count as usize)
                .then(|| self.runtime_info.xfb_varyings.get(varying_index).copied())
                .flatten()
                .filter(|varying| varying.components > 0);
            let num_components = xfb.map_or(remainder, |varying| varying.components);
            let value_type = self.f32_vector_type(num_components);
            let id = self.define_output(value_type, invocations, None, None);
            self.builder.decorate(
                id,
                spirv::Decoration::Location,
                vec![Operand::LiteralBit32(index as u32)],
            );
            if element > 0 {
                self.builder.decorate(
                    id,
                    spirv::Decoration::Component,
                    vec![Operand::LiteralBit32(element)],
                );
            }
            if let Some(varying) = xfb {
                self.builder.decorate(
                    id,
                    spirv::Decoration::XfbBuffer,
                    vec![Operand::LiteralBit32(varying.buffer)],
                );
                self.builder.decorate(
                    id,
                    spirv::Decoration::XfbStride,
                    vec![Operand::LiteralBit32(varying.stride)],
                );
                self.builder.decorate(
                    id,
                    spirv::Decoration::Offset,
                    vec![Operand::LiteralBit32(varying.offset)],
                );
                if self.stage == ShaderStage::Geometry && varying.stream != 0 {
                    self.builder.decorate(
                        id,
                        spirv::Decoration::Stream,
                        vec![Operand::LiteralBit32(varying.stream)],
                    );
                }
            }
            const SWIZZLE: &str = "xyzw";
            if num_components < 4 || element > 0 {
                let end = (element + num_components) as usize;
                self.builder.name(
                    id,
                    format!("out_attr{}_{}", index, &SWIZZLE[element as usize..end]),
                );
            } else {
                self.builder.name(id, format!("out_attr{index}"));
            }
            let info = GenericElementInfo {
                id,
                first_element: element,
                num_components,
            };
            for component in element..element + num_components {
                self.output_generics[index][component as usize] = info;
            }
            self.output_vars.entry(index as u32).or_insert(id);
            element += num_components;
        }
    }

    fn define_inputs(&mut self, program: &ir::Program) {
        use crate::ir::value::Attribute;

        let info = &program.info;
        let mut loads = info.loads.clone();
        for (load, passthrough) in loads.mask.iter_mut().zip(info.passthrough.mask) {
            *load |= passthrough;
        }

        if info.uses_workgroup_id {
            self.workgroup_id =
                self.define_input(self.u32_vec3_type, false, Some(spirv::BuiltIn::WorkgroupId));
        }
        if info.uses_local_invocation_id {
            self.local_invocation_id = self.define_input(
                self.u32_vec3_type,
                false,
                Some(spirv::BuiltIn::LocalInvocationId),
            );
        }
        if info.uses_invocation_id {
            self.invocation_id =
                self.define_input(self.u32_type, false, Some(spirv::BuiltIn::InvocationId));
        }
        if info.uses_invocation_info
            && matches!(
                self.stage,
                ShaderStage::TessellationControl | ShaderStage::TessellationEval
            )
        {
            self.patch_vertices_in =
                self.define_input(self.u32_type, false, Some(spirv::BuiltIn::PatchVertices));
        }
        if info.uses_sample_id {
            self.sample_id =
                self.define_input(self.u32_type, false, Some(spirv::BuiltIn::SampleId));
            if self.stage == ShaderStage::Fragment {
                self.builder
                    .decorate(self.sample_id, spirv::Decoration::Flat, vec![]);
            }
        }
        if info.uses_is_helper_invocation {
            self.is_helper_invocation = self.define_input(
                self.bool_type,
                false,
                Some(spirv::BuiltIn::HelperInvocation),
            );
        }
        if info.uses_subgroup_mask && self.profile.supports_subgroup_stage(self.stage) {
            self.subgroup_mask_eq = self.define_input(
                self.u32_vec4_type,
                false,
                Some(spirv::BuiltIn::SubgroupEqMaskKHR),
            );
            self.subgroup_mask_lt = self.define_input(
                self.u32_vec4_type,
                false,
                Some(spirv::BuiltIn::SubgroupLtMaskKHR),
            );
            self.subgroup_mask_le = self.define_input(
                self.u32_vec4_type,
                false,
                Some(spirv::BuiltIn::SubgroupLeMaskKHR),
            );
            self.subgroup_mask_gt = self.define_input(
                self.u32_vec4_type,
                false,
                Some(spirv::BuiltIn::SubgroupGtMaskKHR),
            );
            self.subgroup_mask_ge = self.define_input(
                self.u32_vec4_type,
                false,
                Some(spirv::BuiltIn::SubgroupGeMaskKHR),
            );
            if self.stage == ShaderStage::Fragment {
                for mask in [
                    self.subgroup_mask_eq,
                    self.subgroup_mask_lt,
                    self.subgroup_mask_le,
                    self.subgroup_mask_gt,
                    self.subgroup_mask_ge,
                ] {
                    self.builder.decorate(mask, spirv::Decoration::Flat, vec![]);
                }
            }
        }
        if (info.uses_fswzadd
            || info.uses_subgroup_invocation_id
            || info.uses_subgroup_shuffles
            || (self.profile.warp_size_potentially_larger_than_guest
                && (info.uses_subgroup_vote || info.uses_subgroup_mask)))
            && self.profile.supports_subgroup_stage(self.stage)
        {
            self.builder.capability(spirv::Capability::GroupNonUniform);
            self.subgroup_local_invocation_id = self.define_input(
                self.u32_type,
                false,
                Some(spirv::BuiltIn::SubgroupLocalInvocationId),
            );
            if self.stage == ShaderStage::Fragment {
                self.builder.decorate(
                    self.subgroup_local_invocation_id,
                    spirv::Decoration::Flat,
                    vec![],
                );
            }
        }
        if info.uses_fswzadd {
            let minus_one = self.constant_f32(-1.0);
            let one = self.const_one_f32;
            let zero = self.const_zero_f32;
            self.fswzadd_lut_a = self
                .builder
                .constant_composite(self.f32_vec4_type, vec![minus_one, one, minus_one, zero]);
            self.fswzadd_lut_b = self.builder.constant_composite(
                self.f32_vec4_type,
                vec![minus_one, minus_one, one, minus_one],
            );
        }
        if loads.get(Attribute::PRIMITIVE_ID.0 as usize) {
            self.primitive_id =
                self.define_input(self.u32_type, false, Some(spirv::BuiltIn::PrimitiveId));
            if self.stage == ShaderStage::Fragment {
                self.builder
                    .decorate(self.primitive_id, spirv::Decoration::Flat, vec![]);
            }
        }
        if loads.get(Attribute::LAYER.0 as usize) {
            self.builder.capability(spirv::Capability::Geometry);
            self.layer = self.define_input(self.u32_type, false, Some(spirv::BuiltIn::Layer));
            self.builder
                .decorate(self.layer, spirv::Decoration::Flat, vec![]);
        }
        if loads.any_component(Attribute::POSITION_X.0 as usize) {
            let is_fragment = self.stage == ShaderStage::Fragment;
            if !is_fragment && self.profile.has_broken_spirv_position_input {
                self.need_input_position_indirect = true;
                let position_struct = self.builder.type_struct(vec![self.f32_vec4_type]);
                self.input_position = self.define_input(position_struct, true, None);
                self.builder.member_decorate(
                    position_struct,
                    0,
                    spirv::Decoration::BuiltIn,
                    vec![Operand::BuiltIn(spirv::BuiltIn::Position)],
                );
                self.builder
                    .decorate(position_struct, spirv::Decoration::Block, vec![]);
            } else {
                let built_in = if is_fragment {
                    spirv::BuiltIn::FragCoord
                } else {
                    spirv::BuiltIn::Position
                };
                self.input_position = self.define_input(self.f32_vec4_type, true, Some(built_in));
                if self.profile.support_geometry_shader_passthrough
                    && info
                        .passthrough
                        .any_component(Attribute::POSITION_X.0 as usize)
                {
                    self.builder.decorate(
                        self.input_position,
                        spirv::Decoration::PassthroughNV,
                        vec![],
                    );
                }
            }
        }
        if loads.get(Attribute::INSTANCE_ID.0 as usize) {
            if self.profile.support_vertex_instance_id {
                self.instance_id =
                    self.define_input(self.u32_type, true, Some(spirv::BuiltIn::InstanceId));
                if loads.get(Attribute::BASE_INSTANCE.0 as usize) {
                    self.base_instance =
                        self.define_input(self.u32_type, true, Some(spirv::BuiltIn::BaseInstance));
                }
            } else {
                self.instance_index =
                    self.define_input(self.u32_type, true, Some(spirv::BuiltIn::InstanceIndex));
                self.base_instance =
                    self.define_input(self.u32_type, true, Some(spirv::BuiltIn::BaseInstance));
            }
        } else if loads.get(Attribute::BASE_INSTANCE.0 as usize) {
            self.base_instance =
                self.define_input(self.u32_type, true, Some(spirv::BuiltIn::BaseInstance));
        }
        if loads.get(Attribute::VERTEX_ID.0 as usize) {
            if self.profile.support_vertex_instance_id {
                self.vertex_id =
                    self.define_input(self.u32_type, true, Some(spirv::BuiltIn::VertexId));
                if loads.get(Attribute::BASE_VERTEX.0 as usize) {
                    self.base_vertex =
                        self.define_input(self.u32_type, true, Some(spirv::BuiltIn::BaseVertex));
                }
            } else {
                self.vertex_index =
                    self.define_input(self.u32_type, true, Some(spirv::BuiltIn::VertexIndex));
                self.base_vertex =
                    self.define_input(self.u32_type, true, Some(spirv::BuiltIn::BaseVertex));
            }
        } else if loads.get(Attribute::BASE_VERTEX.0 as usize) {
            self.base_vertex =
                self.define_input(self.u32_type, true, Some(spirv::BuiltIn::BaseVertex));
        }
        if loads.get(Attribute::DRAW_ID.0 as usize) {
            self.draw_index =
                self.define_input(self.u32_type, true, Some(spirv::BuiltIn::DrawIndex));
        }
        if loads.get(Attribute::FRONT_FACE.0 as usize) {
            self.front_face =
                self.define_input(self.bool_type, true, Some(spirv::BuiltIn::FrontFacing));
        }
        if loads.get(Attribute::POINT_SPRITE_S.0 as usize)
            || loads.get(Attribute::POINT_SPRITE_T.0 as usize)
        {
            self.point_coord =
                self.define_input(self.f32_vec2_type, true, Some(spirv::BuiltIn::PointCoord));
        }
        if loads.get(Attribute::TESSELLATION_EVALUATION_POINT_U.0 as usize)
            || loads.get(Attribute::TESSELLATION_EVALUATION_POINT_V.0 as usize)
        {
            self.tess_coord =
                self.define_input(self.f32_vec3_type, false, Some(spirv::BuiltIn::TessCoord));
        }
        for index in 0..32 {
            let input_type = self.runtime_info.generic_input_types[index];
            if !self.runtime_info.previous_stage_stores.generic_any(index)
                || !loads.generic_any(index)
                || input_type == AttributeType::Disabled
            {
                continue;
            }
            let value_type = self.attribute_type(input_type);
            let id = self.define_input(value_type, true, None);
            self.builder.decorate(
                id,
                spirv::Decoration::Location,
                vec![Operand::LiteralBit32(index as u32)],
            );
            self.builder.name(id, format!("in_attr{index}"));
            self.input_generics[index] = self.attribute_info(input_type, id);
            self.input_vars.insert(index as u32, id);
            if info.passthrough.generic_any(index)
                && self.profile.support_geometry_shader_passthrough
            {
                self.builder
                    .decorate(id, spirv::Decoration::PassthroughNV, vec![]);
            }
            if self.stage != ShaderStage::Fragment {
                continue;
            }
            // Integer fragment inputs cannot be interpolated: upstream always
            // decorates them `Flat` regardless of the recorded interpolation.
            let is_integer = matches!(
                input_type,
                AttributeType::SignedInt | AttributeType::UnsignedInt
            );
            if is_integer {
                self.builder.decorate(id, spirv::Decoration::Flat, vec![]);
            } else {
                match info.interpolation[index] {
                    crate::shader_info::Interpolation::Smooth => {}
                    crate::shader_info::Interpolation::NoPerspective => {
                        self.builder
                            .decorate(id, spirv::Decoration::NoPerspective, vec![]);
                    }
                    crate::shader_info::Interpolation::Flat => {
                        self.builder.decorate(id, spirv::Decoration::Flat, vec![]);
                    }
                }
            }
        }
        if self.stage == ShaderStage::TessellationEval {
            for index in 0..info.uses_patches.len() {
                if !info.uses_patches[index] {
                    continue;
                }
                let id = self.define_input(self.f32_vec4_type, false, None);
                self.builder.decorate(id, spirv::Decoration::Patch, vec![]);
                self.builder.decorate(
                    id,
                    spirv::Decoration::Location,
                    vec![Operand::LiteralBit32(index as u32)],
                );
                self.patches[index] = id;
            }
        }
    }

    fn define_outputs(&mut self, program: &ir::Program) {
        use crate::ir::value::Attribute;

        let info = &program.info;
        let invocations = Some(program.invocations);
        if self.runtime_info.convert_depth_mode
            || info.stores.any_component(Attribute::POSITION_X.0 as usize)
            || self.stage == ShaderStage::VertexB
        {
            self.output_position = self.define_output(
                self.f32_vec4_type,
                invocations,
                Some(spirv::BuiltIn::Position),
                None,
            );
            self.output_vars.insert(0xffff_0000, self.output_position);
        }
        if info.stores.get(Attribute::POINT_SIZE.0 as usize)
            || self.runtime_info.fixed_state_point_size.is_some()
        {
            assert_ne!(
                self.stage,
                ShaderStage::Fragment,
                "storing PointSize in fragment stage is unsupported upstream"
            );
            self.output_point_size = self.define_output(
                self.f32_type,
                invocations,
                Some(spirv::BuiltIn::PointSize),
                None,
            );
        }
        if info.stores.clip_distances() {
            assert_ne!(
                self.stage,
                ShaderStage::Fragment,
                "storing ClipDistance in fragment stage is unsupported upstream"
            );
            if self.profile.max_user_clip_distances > 0 {
                let used = self.profile.max_user_clip_distances.min(8);
                let count = self.constant_u32(used);
                let array_type = self.builder.type_array(self.f32_type, count);
                let initializer = self
                    .builder
                    .constant_composite(array_type, vec![self.const_zero_f32; used as usize]);
                self.clip_distances = self.define_output(
                    array_type,
                    invocations,
                    Some(spirv::BuiltIn::ClipDistance),
                    Some(initializer),
                );
            }
        }
        if info.stores.get(Attribute::LAYER.0 as usize)
            && (self.profile.support_viewport_index_layer_non_geometry
                || self.stage == ShaderStage::Geometry)
        {
            assert_ne!(
                self.stage,
                ShaderStage::Fragment,
                "storing Layer in fragment stage is unsupported upstream"
            );
            self.layer = self.define_output(
                self.u32_type,
                invocations,
                Some(spirv::BuiltIn::Layer),
                None,
            );
        }
        if info.stores.get(Attribute::VIEWPORT_INDEX.0 as usize)
            && (self.profile.support_viewport_index_layer_non_geometry
                || self.stage == ShaderStage::Geometry)
        {
            assert_ne!(
                self.stage,
                ShaderStage::Fragment,
                "storing ViewportIndex in fragment stage is unsupported upstream"
            );
            self.viewport_index = self.define_output(
                self.u32_type,
                invocations,
                Some(spirv::BuiltIn::ViewportIndex),
                None,
            );
        }
        if info.stores.get(Attribute::VIEWPORT_MASK.0 as usize)
            && self.profile.support_viewport_mask
        {
            let count = self.constant_u32(1);
            let array_type = self.builder.type_array(self.u32_type, count);
            self.viewport_mask =
                self.define_output(array_type, None, Some(spirv::BuiltIn::ViewportMaskNV), None);
        }
        for index in 0..32 {
            if info.stores.generic_any(index) {
                self.define_generic_output(index, invocations);
            }
        }
        match self.stage {
            ShaderStage::TessellationControl => {
                if info.stores_tess_level_outer {
                    let count = self.constant_u32(4);
                    let array_type = self.builder.type_array(self.f32_type, count);
                    self.output_tess_level_outer = self.define_output(
                        array_type,
                        None,
                        Some(spirv::BuiltIn::TessLevelOuter),
                        None,
                    );
                    self.builder.decorate(
                        self.output_tess_level_outer,
                        spirv::Decoration::Patch,
                        vec![],
                    );
                }
                if info.stores_tess_level_inner {
                    let count = self.constant_u32(2);
                    let array_type = self.builder.type_array(self.f32_type, count);
                    self.output_tess_level_inner = self.define_output(
                        array_type,
                        None,
                        Some(spirv::BuiltIn::TessLevelInner),
                        None,
                    );
                    self.builder.decorate(
                        self.output_tess_level_inner,
                        spirv::Decoration::Patch,
                        vec![],
                    );
                }
                for index in 0..info.uses_patches.len() {
                    if !info.uses_patches[index] {
                        continue;
                    }
                    let id = self.define_output(self.f32_vec4_type, None, None, None);
                    self.builder.decorate(id, spirv::Decoration::Patch, vec![]);
                    self.builder.decorate(
                        id,
                        spirv::Decoration::Location,
                        vec![Operand::LiteralBit32(index as u32)],
                    );
                    self.patches[index] = id;
                }
            }
            ShaderStage::Fragment => {
                for index in 0..8 {
                    let need_dual_source = self.runtime_info.dual_source_blend && index <= 1;
                    if !need_dual_source
                        && !info.stores_frag_color[index]
                        && !self.profile.need_declared_frag_colors
                    {
                        continue;
                    }
                    let output_type = match self.runtime_info.frag_color_types[index] {
                        AttributeType::UnsignedInt => self.u32_vec4_type,
                        AttributeType::SignedInt => self.i32_vec4_type,
                        _ => self.f32_vec4_type,
                    };
                    let id = self.define_output(output_type, None, None, None);
                    if self.runtime_info.dual_source_blend && index <= 1 {
                        self.builder.decorate(
                            id,
                            spirv::Decoration::Location,
                            vec![Operand::LiteralBit32(0)],
                        );
                        self.builder.decorate(
                            id,
                            spirv::Decoration::Index,
                            vec![Operand::LiteralBit32(index as u32)],
                        );
                        self.builder.name(
                            id,
                            if index == 0 {
                                "frag_color0"
                            } else {
                                "frag_color0_secondary"
                            },
                        );
                    } else {
                        self.builder.decorate(
                            id,
                            spirv::Decoration::Location,
                            vec![Operand::LiteralBit32(index as u32)],
                        );
                        self.builder.name(id, format!("frag_color{index}"));
                    }
                    self.frag_color[index] = id;
                    self.output_vars.insert(index as u32, id);
                }
                if info.stores_frag_depth {
                    self.frag_depth = self.define_output(
                        self.f32_type,
                        None,
                        Some(spirv::BuiltIn::FragDepth),
                        None,
                    );
                }
                if info.stores_sample_mask {
                    let count = self.constant_u32(1);
                    let array_type = self.builder.type_array(self.u32_type, count);
                    self.sample_mask = self.define_output(
                        array_type,
                        None,
                        Some(spirv::BuiltIn::SampleMask),
                        None,
                    );
                }
            }
            _ => {}
        }
    }

    /// Define global variables (inputs, outputs, UBOs, textures) from shader info.
    pub fn define_global_variables(&mut self, program: &ir::Program, bindings: &mut Bindings) {
        let info = &program.info;
        self.define_inputs(program);
        self.define_outputs(program);

        self.define_local_memory(program);
        self.define_shared_memory(program);
        self.define_shared_memory_functions(program);

        // Constant buffers (UBOs), matching upstream DefineConstantBuffers.
        if !info.constant_buffer_descriptors.is_empty() {
            let binding = if self.profile.unified_descriptor_binding {
                &mut bindings.unified
            } else {
                &mut bindings.uniform_buffer
            };
            let first_binding = *binding;
            if !self.profile.support_descriptor_aliasing {
                self.define_constant_buffer_view(
                    &info.constant_buffer_descriptors,
                    first_binding,
                    self.u32_vec4_type,
                    16,
                    UniformDefinitionKind::U32x4,
                );
                *binding += info
                    .constant_buffer_descriptors
                    .iter()
                    .map(|desc| desc.count)
                    .sum::<u32>();
            } else {
                let mut types = info.used_constant_buffer_types | info.used_indirect_cbuf_types;
                if types & Type::U8 as u32 != 0 {
                    if self.profile.support_int8
                        && self.profile.support_uniform_and_storage_buffer_8bit
                    {
                        self.define_constant_buffer_view(
                            &info.constant_buffer_descriptors,
                            first_binding,
                            self.u8_type,
                            1,
                            UniformDefinitionKind::U8,
                        );
                        self.define_constant_buffer_view(
                            &info.constant_buffer_descriptors,
                            first_binding,
                            self.i8_type,
                            1,
                            UniformDefinitionKind::I8,
                        );
                    } else {
                        types |= Type::U32 as u32;
                    }
                }
                if types & Type::U16 as u32 != 0 {
                    if self.profile.support_int16
                        && self.profile.support_uniform_and_storage_buffer_16bit
                    {
                        self.define_constant_buffer_view(
                            &info.constant_buffer_descriptors,
                            first_binding,
                            self.u16_type,
                            2,
                            UniformDefinitionKind::U16,
                        );
                        self.define_constant_buffer_view(
                            &info.constant_buffer_descriptors,
                            first_binding,
                            self.i16_type,
                            2,
                            UniformDefinitionKind::I16,
                        );
                    } else {
                        types |= Type::U32 as u32;
                    }
                }
                if types & Type::U32 as u32 != 0 {
                    self.define_constant_buffer_view(
                        &info.constant_buffer_descriptors,
                        first_binding,
                        self.u32_type,
                        4,
                        UniformDefinitionKind::U32,
                    );
                }
                if types & Type::F32 as u32 != 0 {
                    self.define_constant_buffer_view(
                        &info.constant_buffer_descriptors,
                        first_binding,
                        self.f32_type,
                        4,
                        UniformDefinitionKind::F32,
                    );
                }
                if types & Type::U32x2 as u32 != 0 {
                    self.define_constant_buffer_view(
                        &info.constant_buffer_descriptors,
                        first_binding,
                        self.u32_vec2_type,
                        8,
                        UniformDefinitionKind::U32x2,
                    );
                }
                if types & Type::U32x4 as u32 != 0 {
                    self.define_constant_buffer_view(
                        &info.constant_buffer_descriptors,
                        first_binding,
                        self.u32_vec4_type,
                        16,
                        UniformDefinitionKind::U32x4,
                    );
                }
                *binding += info.constant_buffer_descriptors.len() as u32;
            }
        }
        self.define_constant_buffer_indirect_functions(info);

        let storage_binding = if self.profile.unified_descriptor_binding {
            &mut bindings.unified
        } else {
            &mut bindings.storage_buffer
        };
        self.define_storage_buffers(info, storage_binding);

        let mut texture_binding = if self.profile.unified_descriptor_binding {
            bindings.unified
        } else {
            bindings.texture
        };
        self.define_texture_buffers(&info.texture_buffer_descriptors, &mut texture_binding);
        if self.profile.unified_descriptor_binding {
            bindings.unified = texture_binding;
        } else {
            bindings.texture = texture_binding;
        }

        let mut image_binding = if self.profile.unified_descriptor_binding {
            bindings.unified
        } else {
            bindings.image
        };
        self.define_image_buffers(&info.image_buffer_descriptors, &mut image_binding);
        if self.profile.unified_descriptor_binding {
            bindings.unified = image_binding;
        } else {
            bindings.image = image_binding;
        }

        // Textures (combined image samplers)
        self.textures.reserve(info.texture_descriptors.len());
        for desc in &info.texture_descriptors {
            let binding = if self.profile.unified_descriptor_binding {
                &mut bindings.unified
            } else {
                &mut bindings.texture
            };
            let image_type = texture_image_type(self, desc);
            let sampled_image = self.builder.type_sampled_image(image_type);
            let pointer_type = self.builder.type_pointer(
                None,
                spirv::StorageClass::UniformConstant,
                sampled_image,
            );
            let descriptor_type = if desc.count > 1 {
                let count = self.builder.constant_bit32(self.u32_type, desc.count);
                self.builder.type_array(sampled_image, count)
            } else {
                sampled_image
            };
            let descriptor_pointer_type = self.builder.type_pointer(
                None,
                spirv::StorageClass::UniformConstant,
                descriptor_type,
            );
            let var = self.builder.variable(
                descriptor_pointer_type,
                None,
                spirv::StorageClass::UniformConstant,
                None,
            );
            self.builder.decorate(
                var,
                spirv::Decoration::DescriptorSet,
                vec![Operand::LiteralBit32(0)],
            );
            self.builder.decorate(
                var,
                spirv::Decoration::Binding,
                vec![Operand::LiteralBit32(*binding)],
            );

            self.textures.push(TextureDefinition {
                id: var,
                sampled_type: sampled_image,
                pointer_type,
                image_type,
                count: desc.count,
                is_multisample: desc.is_multisample,
                is_integer: desc.is_integer,
            });
            if self.profile.supported_spirv >= 0x0001_0400 {
                self.interfaces.push(var);
            }
            *binding += 1;
        }

        if info.uses_atomic_image_u32 {
            self.image_u32 =
                self.builder
                    .type_pointer(None, spirv::StorageClass::Image, self.u32_type);
        }

        let mut image_binding = if self.profile.unified_descriptor_binding {
            bindings.unified
        } else {
            bindings.image
        };
        self.define_images(&info.image_descriptors, &mut image_binding);
        if self.profile.unified_descriptor_binding {
            bindings.unified = image_binding;
        } else {
            bindings.image = image_binding;
        }

        self.define_attribute_mem_access(info);
        self.define_write_storage_cas_loop_function(info);
        self.define_global_memory_functions(info);
        self.define_rescaling_input(info);
        self.define_render_area(info);
    }

    fn define_rescaling_input(&mut self, info: &ShaderInfo) {
        if !info.uses_rescaling_uniform {
            return;
        }
        if self.profile.unified_descriptor_binding {
            self.define_rescaling_input_push_constant();
        } else {
            self.define_rescaling_input_uniform_constant();
        }
    }

    fn define_rescaling_input_push_constant(&mut self) {
        use super::emit_spirv::{
            NUM_IMAGE_SCALING_WORDS, NUM_TEXTURE_SCALING_WORDS,
            RESCALING_LAYOUT_DOWN_FACTOR_OFFSET, RESCALING_LAYOUT_WORDS_OFFSET,
        };

        let textures_len = self
            .builder
            .constant_bit32(self.u32_type, NUM_TEXTURE_SCALING_WORDS);
        let textures_type = self.builder.type_array(self.u32_type, textures_len);
        self.builder.decorate(
            textures_type,
            spirv::Decoration::ArrayStride,
            vec![Operand::LiteralBit32(4)],
        );

        let images_len = self
            .builder
            .constant_bit32(self.u32_type, NUM_IMAGE_SCALING_WORDS);
        let images_type = self.builder.type_array(self.u32_type, images_len);
        self.builder.decorate(
            images_type,
            spirv::Decoration::ArrayStride,
            vec![Operand::LiteralBit32(4)],
        );

        let mut members = vec![textures_type, images_type];
        if self.stage != ShaderStage::Compute {
            self.rescaling_downfactor_member_index = members.len() as u32;
            members.push(self.f32_type);
        }

        let push_constant_struct = self.builder.type_struct(members);
        self.builder
            .decorate(push_constant_struct, spirv::Decoration::Block, vec![]);
        self.builder.name(push_constant_struct, "ResolutionInfo");
        self.builder.member_decorate(
            push_constant_struct,
            0,
            spirv::Decoration::Offset,
            vec![Operand::LiteralBit32(RESCALING_LAYOUT_WORDS_OFFSET)],
        );
        self.builder
            .member_name(push_constant_struct, 0, "rescaling_textures");
        self.builder.member_decorate(
            push_constant_struct,
            1,
            spirv::Decoration::Offset,
            vec![Operand::LiteralBit32(16)],
        );
        self.builder
            .member_name(push_constant_struct, 1, "rescaling_images");
        if self.stage != ShaderStage::Compute {
            self.builder.member_decorate(
                push_constant_struct,
                self.rescaling_downfactor_member_index,
                spirv::Decoration::Offset,
                vec![Operand::LiteralBit32(RESCALING_LAYOUT_DOWN_FACTOR_OFFSET)],
            );
            self.builder.member_name(
                push_constant_struct,
                self.rescaling_downfactor_member_index,
                "down_factor",
            );
        }

        let pointer_type = self.builder.type_pointer(
            None,
            spirv::StorageClass::PushConstant,
            push_constant_struct,
        );
        self.rescaling_push_constants =
            self.builder
                .variable(pointer_type, None, spirv::StorageClass::PushConstant, None);
        self.builder
            .name(self.rescaling_push_constants, "rescaling_push_constants");
        if self.profile.supported_spirv >= 0x0001_0400 {
            self.interfaces.push(self.rescaling_push_constants);
        }
    }

    fn define_rescaling_input_uniform_constant(&mut self) {
        let pointer_type = self.builder.type_pointer(
            None,
            spirv::StorageClass::UniformConstant,
            self.f32_vec4_type,
        );
        self.rescaling_uniform_constant = self.builder.variable(
            pointer_type,
            None,
            spirv::StorageClass::UniformConstant,
            None,
        );
        self.builder.decorate(
            self.rescaling_uniform_constant,
            spirv::Decoration::Location,
            vec![Operand::LiteralBit32(0)],
        );
        if self.profile.supported_spirv >= 0x0001_0400 {
            self.interfaces.push(self.rescaling_uniform_constant);
        }
    }

    fn define_render_area(&mut self, info: &ShaderInfo) {
        if !info.uses_render_area || !self.profile.unified_descriptor_binding {
            return;
        }

        self.render_are_member_index = 0;
        let push_constant_struct = self.builder.type_struct(vec![self.f32_vec4_type]);
        self.builder
            .decorate(push_constant_struct, spirv::Decoration::Block, vec![]);
        self.builder.member_decorate(
            push_constant_struct,
            self.render_are_member_index,
            spirv::Decoration::Offset,
            vec![Operand::LiteralBit32(0)],
        );

        let pointer_type = self.builder.type_pointer(
            None,
            spirv::StorageClass::PushConstant,
            push_constant_struct,
        );
        self.render_area_push_constant =
            self.builder
                .variable(pointer_type, None, spirv::StorageClass::PushConstant, None);
        if self.profile.supported_spirv >= 0x0001_0400 {
            self.interfaces.push(self.render_area_push_constant);
        }
    }

    fn phi_type_id(&self, inst: &ir::Inst) -> spirv::Word {
        use crate::ir::types::Type;
        match inst.flags {
            x if x == Type::U1 as u32 => self.bool_type,
            x if x == Type::U8 as u32 || x == Type::U16 as u32 || x == Type::U32 as u32 => {
                self.u32_type
            }
            x if x == Type::U64 as u32 => self.u64_type,
            x if x == Type::F32 as u32 => self.f32_type,
            x if x == Type::F64 as u32 => self.f64_type,
            flags => panic!("SPIR-V: unimplemented Phi result type flags {flags:#x}"),
        }
    }

    pub(crate) fn begin_ir_block(&mut self, block_idx: u32) {
        let label = self
            .block_labels
            .get(block_idx as usize)
            .copied()
            .unwrap_or_else(|| panic!("SPIR-V: missing label for block {block_idx}"));
        self.builder.begin_block(Some(label)).unwrap();
    }

    pub(crate) fn emit_block_instructions(&mut self, program: &ir::Program, block_idx: u32) {
        let block = program
            .blocks
            .get(block_idx as usize)
            .unwrap_or_else(|| panic!("SPIR-V: syntax references missing block {block_idx}"));
        for (inst_idx, inst) in block.indexed_iter() {
            if matches!(inst.opcode, ir::Opcode::Phi) {
                self.emit_instruction(program, inst, block_idx, inst_idx);
            }
        }
        for (inst_idx, inst) in block.indexed_iter() {
            if matches!(
                inst.opcode,
                ir::Opcode::UndefU1
                    | ir::Opcode::UndefU8
                    | ir::Opcode::UndefU16
                    | ir::Opcode::UndefU32
                    | ir::Opcode::UndefU64
            ) {
                self.emit_instruction(program, inst, block_idx, inst_idx);
            }
        }
        for (inst_idx, inst) in block.indexed_iter() {
            if matches!(
                inst.opcode,
                ir::Opcode::Phi
                    | ir::Opcode::UndefU1
                    | ir::Opcode::UndefU8
                    | ir::Opcode::UndefU16
                    | ir::Opcode::UndefU32
                    | ir::Opcode::UndefU64
            ) {
                continue;
            }
            self.emit_instruction(program, inst, block_idx, inst_idx);
        }
    }

    /// Emit a single IR instruction as SPIR-V.
    fn emit_instruction(
        &mut self,
        program: &ir::Program,
        inst: &ir::Inst,
        block_idx: u32,
        inst_idx: u32,
    ) {
        use ir::Opcode;
        match inst.opcode {
            // ── FP16 arithmetic ───────────────────────────────────────
            Opcode::FPAdd16 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_add_16(self, inst, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPMul16 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_mul_16(self, inst, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPFma16 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let c = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_floating_point::emit_fp_fma_16(self, inst, a, b, c);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPNeg16 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_neg_16(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPAbs16 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_abs_16(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPMin16 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_min_16(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPMax16 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_max_16(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPSaturate16 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_saturate_16(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPClamp16 => {
                let value = self.resolve_value(inst.arg(0));
                let min = self.resolve_value(inst.arg(1));
                let max = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_floating_point::emit_fp_clamp_16(self, value, min, max);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPRoundEven16 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_round_even_16(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPFloor16 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_floor_16(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPCeil16 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_ceil_16(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPTrunc16 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_trunc_16(self, value);
                self.set_value(block_idx, inst_idx, id);
            }

            // ── FP32 arithmetic ───────────────────────────────────────
            Opcode::FPAdd32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_add_32(self, inst, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPSub32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_sub_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPMul32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_mul_32(self, inst, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPDiv32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_div_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPFma32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let c = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_floating_point::emit_fp_fma_32(self, inst, a, b, c);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPNeg32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_neg_32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPAbs32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_abs_32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPSaturate32 => {
                let a = self.resolve_value(inst.arg(0));
                let zero = self.const_zero_f32;
                let one = self.const_one_f32;
                let id = super::emit_spirv_floating_point::emit_fp_clamp_32(self, a, zero, one);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPClamp32 => {
                let value = self.resolve_value(inst.arg(0));
                let min = self.resolve_value(inst.arg(1));
                let max = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_floating_point::emit_fp_clamp_32(self, value, min, max);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPMin32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_min_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPMax32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_max_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPSin => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_sin(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPCos => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_cos(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPExp2 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_exp2(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPLog2 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_log2(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPSqrt32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_sqrt_32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPRecip32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_recip_32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPRecipSqrt32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_recip_sqrt_32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPFloor32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_floor_32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPCeil32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_ceil_32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPTrunc32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_trunc_32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPRoundEven32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_round_even_32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }

            // ── FP64 arithmetic ───────────────────────────────────────
            Opcode::FPAdd64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_add_64(self, inst, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPSub64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_sub_64(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPMul64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_mul_64(self, inst, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPDiv64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_div_64(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPFma64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let c = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_floating_point::emit_fp_fma_64(self, inst, a, b, c);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPNeg64 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_neg_64(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPAbs64 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_abs_64(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPMin64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_min_64(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPMax64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_max_64(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPRecip64 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_recip_64(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPRecipSqrt64 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_recip_sqrt_64(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPSqrt64 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_sqrt_64(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPSaturate64 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_saturate_64(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPClamp64 => {
                let value = self.resolve_value(inst.arg(0));
                let min = self.resolve_value(inst.arg(1));
                let max = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_floating_point::emit_fp_clamp_64(self, value, min, max);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPRoundEven64 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_round_even_64(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPFloor64 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_floor_64(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPCeil64 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_ceil_64(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPTrunc64 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_trunc_64(self, value);
                self.set_value(block_idx, inst_idx, id);
            }

            // ── FP comparison ─────────────────────────────────────────
            Opcode::FPOrdEqual16 | Opcode::FPOrdEqual64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_ord_equal(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPOrdEqual32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_ord_equal_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPOrdNotEqual16 | Opcode::FPOrdNotEqual64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_ord_not_equal(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPOrdNotEqual32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_ord_not_equal_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPOrdLessThan16 | Opcode::FPOrdLessThan64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_ord_less_than(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPOrdLessThan32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_ord_less_than_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPOrdGreaterThan16 | Opcode::FPOrdGreaterThan64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_ord_greater_than(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPOrdGreaterThan32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_ord_greater_than_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPOrdLessThanEqual16 | Opcode::FPOrdLessThanEqual64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_ord_less_than_equal(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPOrdLessThanEqual32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id =
                    super::emit_spirv_floating_point::emit_fp_ord_less_than_equal_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPOrdGreaterThanEqual16 | Opcode::FPOrdGreaterThanEqual64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id =
                    super::emit_spirv_floating_point::emit_fp_ord_greater_than_equal(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPOrdGreaterThanEqual32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id =
                    super::emit_spirv_floating_point::emit_fp_ord_greater_than_equal_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPUnordEqual16 | Opcode::FPUnordEqual64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_unord_equal(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPUnordEqual32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_unord_equal_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPUnordNotEqual16 | Opcode::FPUnordNotEqual64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_unord_not_equal(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPUnordNotEqual32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_unord_not_equal_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPUnordLessThan16 | Opcode::FPUnordLessThan64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_unord_less_than(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPUnordLessThan32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_unord_less_than_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPUnordGreaterThan16 | Opcode::FPUnordGreaterThan64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_unord_greater_than(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPUnordGreaterThan32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id =
                    super::emit_spirv_floating_point::emit_fp_unord_greater_than_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPUnordLessThanEqual16 | Opcode::FPUnordLessThanEqual64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id =
                    super::emit_spirv_floating_point::emit_fp_unord_less_than_equal(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPUnordLessThanEqual32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id =
                    super::emit_spirv_floating_point::emit_fp_unord_less_than_equal_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPUnordGreaterThanEqual16 | Opcode::FPUnordGreaterThanEqual64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id =
                    super::emit_spirv_floating_point::emit_fp_unord_greater_than_equal(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPUnordGreaterThanEqual32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_floating_point::emit_fp_unord_greater_than_equal_32(
                    self, a, b,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPIsNan16 | Opcode::FPIsNan64 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_is_nan(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FPIsNan32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_floating_point::emit_fp_is_nan_32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }

            // ── Integer arithmetic ────────────────────────────────────
            Opcode::IAdd32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_iadd_32(self, inst, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::IAdd64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_iadd_64(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ISub32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_isub_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ISub64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_isub_64(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::IMul32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_imul_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SDiv32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_sdiv_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::UDiv32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_udiv_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::INeg32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_integer::emit_ineg_32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::INeg64 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_integer::emit_ineg_64(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::IAbs32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_integer::emit_iabs_32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::IAbs64 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_integer::emit_iabs_64(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ShiftLeftLogical32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_shift_left_logical_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ShiftLeftLogical64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_shift_left_logical_64(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ShiftRightLogical32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_shift_right_logical_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ShiftRightLogical64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_shift_right_logical_64(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ShiftRightArithmetic32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_shift_right_arithmetic_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ShiftRightArithmetic64 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_shift_right_arithmetic_64(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::BitwiseAnd32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_bitwise_and_32(self, inst, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::BitwiseOr32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_bitwise_or_32(self, inst, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::BitwiseXor32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_bitwise_xor_32(self, inst, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::BitwiseNot32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_integer::emit_bitwise_not_32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::BitFieldInsert => {
                let base = self.resolve_value(inst.arg(0));
                let insert = self.resolve_value(inst.arg(1));
                let offset = self.resolve_value(inst.arg(2));
                let count = self.resolve_value(inst.arg(3));
                let id = super::emit_spirv_integer::emit_bit_field_insert(
                    self, base, insert, offset, count,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::BitFieldSExtract => {
                let base = self.resolve_value(inst.arg(0));
                let offset = self.resolve_value(inst.arg(1));
                let count = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_integer::emit_bit_field_s_extract(
                    self, inst, base, offset, count,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::BitFieldUExtract => {
                let base = self.resolve_value(inst.arg(0));
                let offset = self.resolve_value(inst.arg(1));
                let count = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_integer::emit_bit_field_u_extract(
                    self, inst, base, offset, count,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::BitCount32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_integer::emit_bit_count_32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::BitReverse32 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_integer::emit_bit_reverse_32(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FindSMsb32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_integer::emit_find_s_msb_32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FindUMsb32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_integer::emit_find_u_msb_32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SMin32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_s_min_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::UMin32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_u_min_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SMax32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_s_max_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::UMax32 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_u_max_32(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SClamp32 => {
                let value = self.resolve_value(inst.arg(0));
                let min = self.resolve_value(inst.arg(1));
                let max = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_integer::emit_s_clamp_32(self, inst, value, min, max);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::UClamp32 => {
                let value = self.resolve_value(inst.arg(0));
                let min = self.resolve_value(inst.arg(1));
                let max = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_integer::emit_u_clamp_32(self, inst, value, min, max);
                self.set_value(block_idx, inst_idx, id);
            }

            // ── Integer comparison ────────────────────────────────────
            Opcode::IEqual => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_i_equal(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::INotEqual => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_i_not_equal(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SLessThan => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_s_less_than(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ULessThan => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_u_less_than(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SLessThanEqual => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_s_less_than_equal(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ULessThanEqual => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_u_less_than_equal(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SGreaterThan => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_s_greater_than(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::UGreaterThan => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_u_greater_than(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SGreaterThanEqual => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_s_greater_than_equal(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::UGreaterThanEqual => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_integer::emit_u_greater_than_equal(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }

            // ── Logic ─────────────────────────────────────────────────
            Opcode::LogicalOr => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_logical::emit_logical_or(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::LogicalAnd => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_logical::emit_logical_and(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::LogicalXor => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_logical::emit_logical_xor(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::LogicalNot => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_logical::emit_logical_not(self, a);
                self.set_value(block_idx, inst_idx, id);
            }

            // ── Select ────────────────────────────────────────────────
            Opcode::SelectU32 => {
                let cond = self.resolve_value(inst.arg(0));
                let t = self.resolve_value(inst.arg(1));
                let f = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_select::emit_select_u32(self, cond, t, f);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SelectU16 => {
                let cond = self.resolve_value(inst.arg(0));
                let t = self.resolve_value(inst.arg(1));
                let f = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_select::emit_select_u16(self, cond, t, f);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SelectU64 => {
                let cond = self.resolve_value(inst.arg(0));
                let t = self.resolve_value(inst.arg(1));
                let f = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_select::emit_select_u64(self, cond, t, f);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SelectF16 => {
                let cond = self.resolve_value(inst.arg(0));
                let t = self.resolve_value(inst.arg(1));
                let f = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_select::emit_select_f16(self, cond, t, f);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SelectF32 => {
                let cond = self.resolve_value(inst.arg(0));
                let t = self.resolve_value(inst.arg(1));
                let f = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_select::emit_select_f32(self, cond, t, f);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SelectF64 => {
                let cond = self.resolve_value(inst.arg(0));
                let t = self.resolve_value(inst.arg(1));
                let f = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_select::emit_select_f64(self, cond, t, f);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SelectU1 => {
                let cond = self.resolve_value(inst.arg(0));
                let t = self.resolve_value(inst.arg(1));
                let f = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_select::emit_select_u1(self, cond, t, f);
                self.set_value(block_idx, inst_idx, id);
            }

            // ── Conversion ────────────────────────────────────────────
            Opcode::ConvertS16F16 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_s16_f16(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertS16F32 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_s16_f32(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertS16F64 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_s16_f64(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertS32F16 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_s32_f16(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertS32F32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_s32_f32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertS32F64 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_s32_f64(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertS64F16 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_s64_f16(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertS64F32 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_s64_f32(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertS64F64 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_s64_f64(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertU16F16 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_u16_f16(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertU16F32 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_u16_f32(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertU16F64 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_u16_f64(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertU32F16 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_u32_f16(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertU32F32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_u32_f32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertU32F64 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_u32_f64(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertU64F16 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_u64_f16(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertU64F32 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_u64_f32(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertU64F64 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_u64_f64(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertU64U32 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_u64_u32(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertU32U64 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_u32_u64(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF16F32 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f16_f32(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF32F16 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f32_f16(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF32F64 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f32_f64(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF64F32 => {
                let value = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f64_f32(self, value);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF32S32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f32_s32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF32U32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f32_u32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF16S8 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f16_s8(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF16S16 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f16_s16(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF16S32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f16_s32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF16S64 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f16_s64(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF16U8 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f16_u8(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF16U16 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f16_u16(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF16U32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f16_u32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF16U64 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f16_u64(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF32S8 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f32_s8(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF32S16 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f32_s16(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF32S64 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f32_s64(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF32U8 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f32_u8(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF32U16 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f32_u16(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF32U64 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f32_u64(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF64S8 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f64_s8(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF64S16 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f64_s16(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF64S32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f64_s32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF64S64 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f64_s64(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF64U8 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f64_u8(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF64U16 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f64_u16(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF64U32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f64_u32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ConvertF64U64 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_convert::emit_convert_f64_u64(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::BitCastU32F32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_bitwise_conversion::emit_bit_cast_u32_f32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::BitCastF32U32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_bitwise_conversion::emit_bit_cast_f32_u32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::PackUint2x32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_bitwise_conversion::emit_pack_uint2x32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::UnpackUint2x32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_bitwise_conversion::emit_unpack_uint2x32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::PackDouble2x32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_bitwise_conversion::emit_pack_double2x32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::UnpackDouble2x32 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_bitwise_conversion::emit_unpack_double2x32(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::PackFloat2x16 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_bitwise_conversion::emit_pack_float2x16(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::UnpackFloat2x16 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_bitwise_conversion::emit_unpack_float2x16(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::PackHalf2x16 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_bitwise_conversion::emit_pack_half2x16(self, a);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::UnpackHalf2x16 => {
                let a = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_bitwise_conversion::emit_unpack_half2x16(self, a);
                self.set_value(block_idx, inst_idx, id);
            }

            // ── Composite ─────────────────────────────────────────────
            Opcode::CompositeConstructU32x2 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_composite::emit_composite_construct_u32x2(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeConstructU32x3 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let c = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_composite::emit_composite_construct_u32x3(self, a, b, c);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeConstructU32x4 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let c = self.resolve_value(inst.arg(2));
                let d = self.resolve_value(inst.arg(3));
                let id =
                    super::emit_spirv_composite::emit_composite_construct_u32x4(self, a, b, c, d);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeExtractU32x2 => {
                let composite = self.resolve_value(inst.arg(0));
                let index = inst.arg(1).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_extract_u32x2(
                    self, composite, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeExtractU32x3 => {
                let composite = self.resolve_value(inst.arg(0));
                let index = inst.arg(1).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_extract_u32x3(
                    self, composite, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeExtractU32x4 => {
                let composite = self.resolve_value(inst.arg(0));
                let index = inst.arg(1).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_extract_u32x4(
                    self, composite, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeInsertU32x2 => {
                let composite = self.resolve_value(inst.arg(0));
                let object = self.resolve_value(inst.arg(1));
                let index = inst.arg(2).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_insert_u32x2(
                    self, composite, object, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeInsertU32x3 => {
                let composite = self.resolve_value(inst.arg(0));
                let object = self.resolve_value(inst.arg(1));
                let index = inst.arg(2).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_insert_u32x3(
                    self, composite, object, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeInsertU32x4 => {
                let composite = self.resolve_value(inst.arg(0));
                let object = self.resolve_value(inst.arg(1));
                let index = inst.arg(2).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_insert_u32x4(
                    self, composite, object, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeConstructF16x2 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_composite::emit_composite_construct_f16x2(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeConstructF16x3 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let c = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_composite::emit_composite_construct_f16x3(self, a, b, c);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeConstructF16x4 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let c = self.resolve_value(inst.arg(2));
                let d = self.resolve_value(inst.arg(3));
                let id =
                    super::emit_spirv_composite::emit_composite_construct_f16x4(self, a, b, c, d);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeExtractF16x2 => {
                let composite = self.resolve_value(inst.arg(0));
                let index = inst.arg(1).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_extract_f16x2(
                    self, composite, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeExtractF16x3 => {
                let composite = self.resolve_value(inst.arg(0));
                let index = inst.arg(1).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_extract_f16x3(
                    self, composite, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeExtractF16x4 => {
                let composite = self.resolve_value(inst.arg(0));
                let index = inst.arg(1).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_extract_f16x4(
                    self, composite, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeInsertF16x2 => {
                let composite = self.resolve_value(inst.arg(0));
                let object = self.resolve_value(inst.arg(1));
                let index = inst.arg(2).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_insert_f16x2(
                    self, composite, object, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeInsertF16x3 => {
                let composite = self.resolve_value(inst.arg(0));
                let object = self.resolve_value(inst.arg(1));
                let index = inst.arg(2).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_insert_f16x3(
                    self, composite, object, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeInsertF16x4 => {
                let composite = self.resolve_value(inst.arg(0));
                let object = self.resolve_value(inst.arg(1));
                let index = inst.arg(2).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_insert_f16x4(
                    self, composite, object, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeConstructF32x2 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let id = super::emit_spirv_composite::emit_composite_construct_f32x2(self, a, b);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeConstructF32x3 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let c = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_composite::emit_composite_construct_f32x3(self, a, b, c);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeConstructF32x4 => {
                let a = self.resolve_value(inst.arg(0));
                let b = self.resolve_value(inst.arg(1));
                let c = self.resolve_value(inst.arg(2));
                let d = self.resolve_value(inst.arg(3));
                let id =
                    super::emit_spirv_composite::emit_composite_construct_f32x4(self, a, b, c, d);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeExtractF32x2 => {
                let composite = self.resolve_value(inst.arg(0));
                let index = inst.arg(1).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_extract_f32x2(
                    self, composite, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeExtractF32x3 => {
                let composite = self.resolve_value(inst.arg(0));
                let index = inst.arg(1).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_extract_f32x3(
                    self, composite, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeExtractF32x4 => {
                let composite = self.resolve_value(inst.arg(0));
                let index = inst.arg(1).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_extract_f32x4(
                    self, composite, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeInsertF32x2 => {
                let composite = self.resolve_value(inst.arg(0));
                let object = self.resolve_value(inst.arg(1));
                let index = inst.arg(2).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_insert_f32x2(
                    self, composite, object, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeInsertF32x3 => {
                let composite = self.resolve_value(inst.arg(0));
                let object = self.resolve_value(inst.arg(1));
                let index = inst.arg(2).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_insert_f32x3(
                    self, composite, object, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeInsertF32x4 => {
                let composite = self.resolve_value(inst.arg(0));
                let object = self.resolve_value(inst.arg(1));
                let index = inst.arg(2).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_insert_f32x4(
                    self, composite, object, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeInsertF64x2 => {
                let composite = self.resolve_value(inst.arg(0));
                let object = self.resolve_value(inst.arg(1));
                let index = inst.arg(2).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_insert_f64x2(
                    self, composite, object, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeInsertF64x3 => {
                let composite = self.resolve_value(inst.arg(0));
                let object = self.resolve_value(inst.arg(1));
                let index = inst.arg(2).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_insert_f64x3(
                    self, composite, object, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::CompositeInsertF64x4 => {
                let composite = self.resolve_value(inst.arg(0));
                let object = self.resolve_value(inst.arg(1));
                let index = inst.arg(2).imm_u32();
                let id = super::emit_spirv_composite::emit_composite_insert_f64x4(
                    self, composite, object, index,
                );
                self.set_value(block_idx, inst_idx, id);
            }

            // ── Context (registers, attributes, cbufs) ────────────────
            Opcode::GetCbufU8
            | Opcode::GetCbufS8
            | Opcode::GetCbufU16
            | Opcode::GetCbufS16
            | Opcode::GetCbufU32
            | Opcode::GetCbufF32
            | Opcode::GetCbufU32x2 => {
                super::emit_spirv_context_get_set::emit_get_cbuf(self, inst, block_idx, inst_idx);
            }
            Opcode::GetAttribute | Opcode::GetAttributeU32 => {
                super::emit_spirv_context_get_set::emit_get_attribute_inst(
                    self, inst, block_idx, inst_idx,
                );
            }
            Opcode::SetAttribute => {
                super::emit_spirv_context_get_set::emit_set_attribute_inst(
                    self, inst, block_idx, inst_idx,
                );
            }
            Opcode::GetAttributeIndexed => {
                super::emit_spirv_context_get_set::emit_get_attribute_indexed_inst(
                    self, inst, block_idx, inst_idx,
                );
            }
            Opcode::SetAttributeIndexed => {
                super::emit_spirv_context_get_set::emit_set_attribute_indexed_inst(
                    self, inst, block_idx, inst_idx,
                );
            }
            Opcode::GetPatch => {
                super::emit_spirv_context_get_set::emit_get_patch_inst(
                    self, inst, block_idx, inst_idx,
                );
            }
            Opcode::SetPatch => {
                super::emit_spirv_context_get_set::emit_set_patch_inst(
                    self, inst, block_idx, inst_idx,
                );
            }
            Opcode::SetFragColor => {
                super::emit_spirv_context_get_set::emit_set_frag_color_inst(
                    self, inst, block_idx, inst_idx,
                );
            }
            Opcode::SetSampleMask => {
                super::emit_spirv_context_get_set::emit_set_sample_mask_inst(
                    self, inst, block_idx, inst_idx,
                );
            }
            Opcode::SetFragDepth => {
                super::emit_spirv_context_get_set::emit_set_frag_depth_inst(
                    self, inst, block_idx, inst_idx,
                );
            }

            // ── Image (texture) ───────────────────────────────────────
            Opcode::ImageSampleImplicitLod | Opcode::ImageSampleExplicitLod => {
                super::emit_spirv_image::emit_image_sample(
                    self, program, inst, block_idx, inst_idx,
                );
            }
            Opcode::ImageSampleDrefImplicitLod | Opcode::ImageSampleDrefExplicitLod => {
                super::emit_spirv_image::emit_image_sample_dref(
                    self, program, inst, block_idx, inst_idx,
                );
            }
            Opcode::ImageFetch => {
                super::emit_spirv_image::emit_image_fetch_inst(self, inst, block_idx, inst_idx);
            }
            Opcode::ImageQueryDimensions => {
                super::emit_spirv_image::emit_image_query(self, inst, block_idx, inst_idx);
            }
            Opcode::ImageQueryLod => {
                super::emit_spirv_image::emit_image_query_lod(self, inst, block_idx, inst_idx);
            }
            Opcode::ImageGradient => {
                super::emit_spirv_image::emit_image_gradient_inst(
                    self, program, inst, block_idx, inst_idx,
                );
            }
            Opcode::ImageGather | Opcode::ImageGatherDref => {
                super::emit_spirv_image::emit_image_gather_inst(
                    self, program, inst, block_idx, inst_idx,
                );
            }
            Opcode::ImageRead => {
                super::emit_spirv_image::emit_image_read_inst(self, inst, block_idx, inst_idx);
            }
            Opcode::ImageWrite => {
                super::emit_spirv_image::emit_image_write_inst(self, inst);
            }
            Opcode::ImageAtomicIAdd32
            | Opcode::ImageAtomicSMin32
            | Opcode::ImageAtomicUMin32
            | Opcode::ImageAtomicSMax32
            | Opcode::ImageAtomicUMax32
            | Opcode::ImageAtomicInc32
            | Opcode::ImageAtomicDec32
            | Opcode::ImageAtomicAnd32
            | Opcode::ImageAtomicOr32
            | Opcode::ImageAtomicXor32
            | Opcode::ImageAtomicExchange32 => {
                super::emit_spirv_image_atomic::emit_image_atomic(self, inst, block_idx, inst_idx);
            }

            Opcode::BoundImageRead
            | Opcode::BindlessImageRead
            | Opcode::BoundImageWrite
            | Opcode::BindlessImageWrite => {
                panic!(
                    "SPIR-V: unreachable non-indexed storage-image instruction {:?}",
                    inst.opcode
                );
            }
            Opcode::BoundImageAtomicIAdd32
            | Opcode::BindlessImageAtomicIAdd32
            | Opcode::BoundImageAtomicSMin32
            | Opcode::BindlessImageAtomicSMin32
            | Opcode::BoundImageAtomicUMin32
            | Opcode::BindlessImageAtomicUMin32
            | Opcode::BoundImageAtomicSMax32
            | Opcode::BindlessImageAtomicSMax32
            | Opcode::BoundImageAtomicUMax32
            | Opcode::BindlessImageAtomicUMax32
            | Opcode::BoundImageAtomicInc32
            | Opcode::BindlessImageAtomicInc32
            | Opcode::BoundImageAtomicDec32
            | Opcode::BindlessImageAtomicDec32
            | Opcode::BoundImageAtomicAnd32
            | Opcode::BindlessImageAtomicAnd32
            | Opcode::BoundImageAtomicOr32
            | Opcode::BindlessImageAtomicOr32
            | Opcode::BoundImageAtomicXor32
            | Opcode::BindlessImageAtomicXor32
            | Opcode::BoundImageAtomicExchange32
            | Opcode::BindlessImageAtomicExchange32 => {
                panic!(
                    "SPIR-V: non-indexed image atomic is not implemented upstream: {:?}",
                    inst.opcode
                );
            }

            // ── Memory ────────────────────────────────────────────────
            Opcode::LoadGlobalU8
            | Opcode::LoadGlobalS8
            | Opcode::LoadGlobalU16
            | Opcode::LoadGlobalS16
            | Opcode::LoadGlobal32
            | Opcode::LoadGlobal64
            | Opcode::LoadGlobal128
            | Opcode::LoadStorageU8
            | Opcode::LoadStorageS8
            | Opcode::LoadStorageU16
            | Opcode::LoadStorageS16
            | Opcode::LoadStorage32
            | Opcode::LoadStorage64
            | Opcode::LoadStorage128 => {
                super::emit_spirv_memory::emit_load(self, inst, block_idx, inst_idx);
            }
            Opcode::WriteGlobalU8
            | Opcode::WriteGlobalS8
            | Opcode::WriteGlobalU16
            | Opcode::WriteGlobalS16
            | Opcode::WriteGlobal32
            | Opcode::WriteGlobal64
            | Opcode::WriteGlobal128
            | Opcode::WriteStorageU8
            | Opcode::WriteStorageS8
            | Opcode::WriteStorageU16
            | Opcode::WriteStorageS16
            | Opcode::WriteStorage32
            | Opcode::WriteStorage64
            | Opcode::WriteStorage128 => {
                super::emit_spirv_memory::emit_store(self, inst, block_idx, inst_idx);
            }
            Opcode::LoadLocal => {
                let offset = self.resolve_value(inst.arg(0));
                let value = super::emit_spirv_context_get_set::emit_load_local(self, offset);
                self.set_value(block_idx, inst_idx, value);
            }
            Opcode::WriteLocal => {
                let offset = self.resolve_value(inst.arg(0));
                let value = self.resolve_value(inst.arg(1));
                super::emit_spirv_context_get_set::emit_write_local(self, offset, value);
            }
            Opcode::LoadSharedU8
            | Opcode::LoadSharedS8
            | Opcode::LoadSharedU16
            | Opcode::LoadSharedS16
            | Opcode::LoadSharedU32
            | Opcode::LoadSharedU64
            | Opcode::LoadSharedU128 => {
                super::emit_spirv_shared_memory::emit_load(self, inst, block_idx, inst_idx);
            }
            Opcode::WriteSharedU8
            | Opcode::WriteSharedU16
            | Opcode::WriteSharedU32
            | Opcode::WriteSharedU64
            | Opcode::WriteSharedU128 => {
                super::emit_spirv_shared_memory::emit_store(self, inst);
            }
            Opcode::SharedAtomicIAdd32
            | Opcode::SharedAtomicSMin32
            | Opcode::SharedAtomicUMin32
            | Opcode::SharedAtomicSMax32
            | Opcode::SharedAtomicUMax32
            | Opcode::SharedAtomicInc32
            | Opcode::SharedAtomicDec32
            | Opcode::SharedAtomicAnd32
            | Opcode::SharedAtomicOr32
            | Opcode::SharedAtomicXor32
            | Opcode::SharedAtomicExchange32
            | Opcode::SharedAtomicExchange64
            | Opcode::SharedAtomicExchange32x2 => {
                super::emit_spirv_atomic::emit_shared_atomic(self, inst, block_idx, inst_idx);
            }
            Opcode::StorageAtomicIAdd32
            | Opcode::StorageAtomicSMin32
            | Opcode::StorageAtomicUMin32
            | Opcode::StorageAtomicSMax32
            | Opcode::StorageAtomicUMax32
            | Opcode::StorageAtomicInc32
            | Opcode::StorageAtomicDec32
            | Opcode::StorageAtomicAnd32
            | Opcode::StorageAtomicOr32
            | Opcode::StorageAtomicXor32
            | Opcode::StorageAtomicExchange32
            | Opcode::StorageAtomicIAdd64
            | Opcode::StorageAtomicSMin64
            | Opcode::StorageAtomicUMin64
            | Opcode::StorageAtomicSMax64
            | Opcode::StorageAtomicUMax64
            | Opcode::StorageAtomicAnd64
            | Opcode::StorageAtomicOr64
            | Opcode::StorageAtomicXor64
            | Opcode::StorageAtomicExchange64
            | Opcode::StorageAtomicIAdd32x2
            | Opcode::StorageAtomicSMin32x2
            | Opcode::StorageAtomicUMin32x2
            | Opcode::StorageAtomicSMax32x2
            | Opcode::StorageAtomicUMax32x2
            | Opcode::StorageAtomicAnd32x2
            | Opcode::StorageAtomicOr32x2
            | Opcode::StorageAtomicXor32x2
            | Opcode::StorageAtomicExchange32x2
            | Opcode::StorageAtomicAddF32
            | Opcode::StorageAtomicAddF16x2
            | Opcode::StorageAtomicAddF32x2
            | Opcode::StorageAtomicMinF16x2
            | Opcode::StorageAtomicMinF32x2
            | Opcode::StorageAtomicMaxF16x2
            | Opcode::StorageAtomicMaxF32x2 => {
                super::emit_spirv_atomic::emit_storage_atomic(self, inst, block_idx, inst_idx);
            }

            // ── Warp / subgroup ──────────────────────────────────────
            Opcode::VoteAll | Opcode::VoteAny | Opcode::VoteEqual => {
                let pred = self.resolve_value(inst.arg(0));
                let id = match inst.opcode {
                    Opcode::VoteAll => super::emit_spirv_warp::emit_vote_all(self, pred),
                    Opcode::VoteAny => super::emit_spirv_warp::emit_vote_any(self, pred),
                    Opcode::VoteEqual => super::emit_spirv_warp::emit_vote_equal(self, pred),
                    _ => unreachable!(),
                };
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::LaneId => {
                let id = super::emit_spirv_warp::emit_lane_id(self);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SubgroupBallot => {
                let pred = self.resolve_value(inst.arg(0));
                let id = super::emit_spirv_warp::emit_subgroup_ballot(self, pred);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SubgroupEqMask
            | Opcode::SubgroupLtMask
            | Opcode::SubgroupLeMask
            | Opcode::SubgroupGtMask
            | Opcode::SubgroupGeMask => {
                let id = match inst.opcode {
                    Opcode::SubgroupEqMask => super::emit_spirv_warp::emit_subgroup_eq_mask(self),
                    Opcode::SubgroupLtMask => super::emit_spirv_warp::emit_subgroup_lt_mask(self),
                    Opcode::SubgroupLeMask => super::emit_spirv_warp::emit_subgroup_le_mask(self),
                    Opcode::SubgroupGtMask => super::emit_spirv_warp::emit_subgroup_gt_mask(self),
                    Opcode::SubgroupGeMask => super::emit_spirv_warp::emit_subgroup_ge_mask(self),
                    _ => unreachable!(),
                };
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ShuffleIndex
            | Opcode::ShuffleUp
            | Opcode::ShuffleDown
            | Opcode::ShuffleButterfly => {
                let value = self.resolve_value(inst.arg(0));
                let index = self.resolve_value(inst.arg(1));
                let clamp = self.resolve_value(inst.arg(2));
                let segmentation_mask = self.resolve_value(inst.arg(3));
                let id = match inst.opcode {
                    Opcode::ShuffleIndex => super::emit_spirv_warp::emit_shuffle_index(
                        self,
                        inst,
                        value,
                        index,
                        clamp,
                        segmentation_mask,
                    ),
                    Opcode::ShuffleUp => super::emit_spirv_warp::emit_shuffle_up(
                        self,
                        inst,
                        value,
                        index,
                        clamp,
                        segmentation_mask,
                    ),
                    Opcode::ShuffleDown => super::emit_spirv_warp::emit_shuffle_down(
                        self,
                        inst,
                        value,
                        index,
                        clamp,
                        segmentation_mask,
                    ),
                    Opcode::ShuffleButterfly => super::emit_spirv_warp::emit_shuffle_butterfly(
                        self,
                        inst,
                        value,
                        index,
                        clamp,
                        segmentation_mask,
                    ),
                    _ => unreachable!(),
                };
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::FSwizzleAdd => {
                let op_a = self.resolve_value(inst.arg(0));
                let op_b = self.resolve_value(inst.arg(1));
                let swizzle = self.resolve_value(inst.arg(2));
                let id = super::emit_spirv_warp::emit_fswizzle_add(self, op_a, op_b, swizzle);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::DPdxFine | Opcode::DPdyFine | Opcode::DPdxCoarse | Opcode::DPdyCoarse => {
                let value = self.resolve_value(inst.arg(0));
                let id = match inst.opcode {
                    Opcode::DPdxFine => super::emit_spirv_warp::emit_dpdx_fine(self, value),
                    Opcode::DPdyFine => super::emit_spirv_warp::emit_dpdy_fine(self, value),
                    Opcode::DPdxCoarse => super::emit_spirv_warp::emit_dpdx_coarse(self, value),
                    Opcode::DPdyCoarse => super::emit_spirv_warp::emit_dpdy_coarse(self, value),
                    _ => unreachable!(),
                };
                self.set_value(block_idx, inst_idx, id);
            }

            // ── Control ───────────────────────────────────────────────
            Opcode::DemoteToHelperInvocation => {
                super::emit_spirv_control_flow::emit_demote_to_helper_invocation(self);
            }
            Opcode::Barrier => {
                super::emit_spirv_barriers::emit_barrier(self);
            }
            Opcode::WorkgroupMemoryBarrier => {
                super::emit_spirv_barriers::emit_workgroup_memory_barrier(self);
            }
            Opcode::DeviceMemoryBarrier => {
                super::emit_spirv_barriers::emit_device_memory_barrier(self);
            }

            // ── Register/predicate access — these are handled during
            //    SSA construction and don't emit SPIR-V directly ───────
            Opcode::Phi => {
                if inst.phi_args.is_empty() {
                    let id = self.builder.undef(self.phi_type_id(inst), None);
                    self.set_value(block_idx, inst_idx, id);
                    return;
                }
                let result_id = self.builder.id();
                self.set_value(block_idx, inst_idx, result_id);
                let incoming: Vec<_> = inst
                    .phi_args
                    .iter()
                    .map(|(block, _)| {
                        let label = self
                            .block_labels
                            .get(*block as usize)
                            .copied()
                            .unwrap_or_else(|| {
                                panic!("SPIR-V: Phi references missing block {block}")
                            });
                        (0, label)
                    })
                    .collect();
                self.builder
                    .phi(self.phi_type_id(inst), Some(result_id), incoming)
                    .unwrap();
                self.deferred_phis.push(DeferredPhi {
                    result_id,
                    values: inst.phi_args.iter().map(|(_, value)| *value).collect(),
                });
            }
            Opcode::Identity | Opcode::ConditionRef => {
                let id = self.resolve_value(inst.arg(0));
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::Prologue => {
                super::emit_spirv_special::emit_prologue(self);
            }
            Opcode::Epilogue => {
                super::emit_spirv_special::emit_epilogue(self);
            }
            Opcode::EmitVertex => {
                super::emit_spirv_special::emit_emit_vertex(self, inst.arg(0));
            }
            Opcode::EndPrimitive => {
                super::emit_spirv_special::emit_end_primitive(self, inst.arg(0));
            }
            Opcode::Void
            | Opcode::GetZeroFromOp
            | Opcode::GetSignFromOp
            | Opcode::GetCarryFromOp
            | Opcode::GetOverflowFromOp
            | Opcode::GetInBoundsFromOp
            | Opcode::GetSparseFromOp
            | Opcode::Reference
            | Opcode::PhiMove => {
                // No SPIR-V emission needed
            }
            Opcode::GetRegister => super::emit_spirv_context_get_set::emit_get_register(self),
            Opcode::SetRegister => super::emit_spirv_context_get_set::emit_set_register(self),
            Opcode::GetPred => super::emit_spirv_context_get_set::emit_get_pred(self),
            Opcode::SetPred => super::emit_spirv_context_get_set::emit_set_pred(self),
            Opcode::SetGotoVariable => {
                super::emit_spirv_context_get_set::emit_set_goto_variable(self)
            }
            Opcode::GetGotoVariable => {
                super::emit_spirv_context_get_set::emit_get_goto_variable(self)
            }
            Opcode::SetIndirectBranchVariable => {
                super::emit_spirv_context_get_set::emit_set_indirect_branch_variable(self)
            }
            Opcode::GetIndirectBranchVariable => {
                super::emit_spirv_context_get_set::emit_get_indirect_branch_variable(self)
            }
            Opcode::GetZFlag => super::emit_spirv_context_get_set::emit_get_z_flag(self),
            Opcode::GetSFlag => super::emit_spirv_context_get_set::emit_get_s_flag(self),
            Opcode::GetCFlag => super::emit_spirv_context_get_set::emit_get_c_flag(self),
            Opcode::GetOFlag => super::emit_spirv_context_get_set::emit_get_o_flag(self),
            Opcode::SetZFlag => super::emit_spirv_context_get_set::emit_set_z_flag(self),
            Opcode::SetSFlag => super::emit_spirv_context_get_set::emit_set_s_flag(self),
            Opcode::SetCFlag => super::emit_spirv_context_get_set::emit_set_c_flag(self),
            Opcode::SetOFlag => super::emit_spirv_context_get_set::emit_set_o_flag(self),
            Opcode::Join => super::emit_spirv_control_flow::emit_join(self),

            // System values
            Opcode::WorkgroupId => {
                let id = super::emit_spirv_context_get_set::emit_workgroup_id(self);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::LocalInvocationId => {
                let id = super::emit_spirv_context_get_set::emit_local_invocation_id(self);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::InvocationId => {
                let id = super::emit_spirv_context_get_set::emit_invocation_id(self);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::InvocationInfo => {
                let id = super::emit_spirv_context_get_set::emit_invocation_info(self);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::IsHelperInvocation => {
                let id = super::emit_spirv_context_get_set::emit_is_helper_invocation(self);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SampleId => {
                let id = super::emit_spirv_context_get_set::emit_sample_id(self);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SRWScaleFactorXY => {
                let id = super::emit_spirv_context_get_set::emit_sr_w_scale_factor_xy(self);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::SRWScaleFactorZ => {
                let id = super::emit_spirv_context_get_set::emit_sr_w_scale_factor_z(self);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::YDirection => {
                let id = super::emit_spirv_context_get_set::emit_y_direction(self);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::ResolutionDownFactor => {
                let id = super::emit_spirv_context_get_set::emit_resolution_down_factor(self);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::IsTextureScaled => {
                let id = super::emit_spirv_image::emit_is_texture_scaled(self, inst.args[0]);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::IsImageScaled => {
                let id = super::emit_spirv_image::emit_is_image_scaled(self, inst.args[0]);
                self.set_value(block_idx, inst_idx, id);
            }
            Opcode::RenderArea => {
                let id = super::emit_spirv_context_get_set::emit_render_area(self);
                self.set_value(block_idx, inst_idx, id);
            }

            // Undefined values
            Opcode::UndefU1
            | Opcode::UndefU8
            | Opcode::UndefU16
            | Opcode::UndefU32
            | Opcode::UndefU64 => {
                let result_type = match inst.opcode {
                    Opcode::UndefU1 => self.bool_type,
                    Opcode::UndefU32 | Opcode::UndefU8 | Opcode::UndefU16 => self.u32_type,
                    Opcode::UndefU64 => self.u64_type,
                    _ => self.u32_type,
                };
                let id = self.builder.undef(result_type, None);
                self.set_value(block_idx, inst_idx, id);
            }

            // These opcodes must have been rewritten by the texture pass.
            // Their upstream emitters throw `LogicError("Unreachable instruction")`.
            Opcode::BindlessImageSampleImplicitLod
            | Opcode::BindlessImageSampleExplicitLod
            | Opcode::BindlessImageSampleDrefImplicitLod
            | Opcode::BindlessImageSampleDrefExplicitLod
            | Opcode::BindlessImageGather
            | Opcode::BindlessImageGatherDref
            | Opcode::BindlessImageFetch
            | Opcode::BindlessImageQueryDimensions
            | Opcode::BindlessImageQueryLod
            | Opcode::BindlessImageGradient
            | Opcode::BoundImageSampleImplicitLod
            | Opcode::BoundImageSampleExplicitLod
            | Opcode::BoundImageSampleDrefImplicitLod
            | Opcode::BoundImageSampleDrefExplicitLod
            | Opcode::BoundImageGather
            | Opcode::BoundImageGatherDref
            | Opcode::BoundImageFetch
            | Opcode::BoundImageQueryDimensions
            | Opcode::BoundImageQueryLod
            | Opcode::BoundImageGradient => {
                panic!(
                    "SPIR-V: image opcode {:?} reached the backend before indexing",
                    inst.opcode
                );
            }

            // Upstream declares these opcodes but its SPIR-V emitters throw
            // `NotImplementedException` rather than silently omitting them.
            Opcode::BitCastU16F16
            | Opcode::BitCastU64F64
            | Opcode::BitCastF16U16
            | Opcode::BitCastF64U64
            | Opcode::CompositeConstructF64x2
            | Opcode::CompositeConstructF64x3
            | Opcode::CompositeConstructF64x4
            | Opcode::CompositeExtractF64x2
            | Opcode::CompositeExtractF64x3
            | Opcode::CompositeExtractF64x4
            | Opcode::SelectU8
            | Opcode::GlobalAtomicIAdd32
            | Opcode::GlobalAtomicSMin32
            | Opcode::GlobalAtomicUMin32
            | Opcode::GlobalAtomicSMax32
            | Opcode::GlobalAtomicUMax32
            | Opcode::GlobalAtomicInc32
            | Opcode::GlobalAtomicDec32
            | Opcode::GlobalAtomicAnd32
            | Opcode::GlobalAtomicOr32
            | Opcode::GlobalAtomicXor32
            | Opcode::GlobalAtomicExchange32
            | Opcode::GlobalAtomicIAdd64
            | Opcode::GlobalAtomicSMin64
            | Opcode::GlobalAtomicUMin64
            | Opcode::GlobalAtomicSMax64
            | Opcode::GlobalAtomicUMax64
            | Opcode::GlobalAtomicAnd64
            | Opcode::GlobalAtomicOr64
            | Opcode::GlobalAtomicXor64
            | Opcode::GlobalAtomicExchange64
            | Opcode::GlobalAtomicIAdd32x2
            | Opcode::GlobalAtomicSMin32x2
            | Opcode::GlobalAtomicUMin32x2
            | Opcode::GlobalAtomicSMax32x2
            | Opcode::GlobalAtomicUMax32x2
            | Opcode::GlobalAtomicAnd32x2
            | Opcode::GlobalAtomicOr32x2
            | Opcode::GlobalAtomicXor32x2
            | Opcode::GlobalAtomicExchange32x2
            | Opcode::GlobalAtomicAddF32
            | Opcode::GlobalAtomicAddF16x2
            | Opcode::GlobalAtomicAddF32x2
            | Opcode::GlobalAtomicMinF16x2
            | Opcode::GlobalAtomicMinF32x2
            | Opcode::GlobalAtomicMaxF16x2
            | Opcode::GlobalAtomicMaxF32x2 => {
                panic!(
                    "SPIR-V: opcode {:?} is not implemented upstream",
                    inst.opcode
                );
            }

            // Structured control flow is emitted from `Program::syntax_list`,
            // matching upstream's `Traverse`; these Rust-only marker opcodes
            // are invalid inside a basic block.
            Opcode::Branch
            | Opcode::BranchConditional
            | Opcode::LoopMerge
            | Opcode::SelectionMerge
            | Opcode::Return
            | Opcode::Unreachable => {
                panic!(
                    "SPIR-V: control-flow marker {:?} reached instruction emission",
                    inst.opcode
                );
            }
        }
    }

    /// Get or create a SPIR-V constant for an IR value.
    pub fn resolve_value(&mut self, value: &ir::Value) -> spirv::Word {
        match value {
            ir::Value::Inst(r) => *self.values.get(&(r.block, r.inst)).unwrap_or_else(|| {
                panic!(
                    "SPIR-V: unresolved IR value reference block={} inst={}",
                    r.block, r.inst
                )
            }),
            ir::Value::ImmU32(v) => self.builder.constant_bit32(self.u32_type, *v),
            ir::Value::ImmF32(v) => self.builder.constant_bit32(self.f32_type, v.to_bits()),
            ir::Value::ImmU1(v) => {
                if *v {
                    self.const_true
                } else {
                    self.const_false
                }
            }
            ir::Value::ImmU64(v) => self.builder.constant_bit64(self.u64_type, *v),
            ir::Value::ImmF64(v) => self.builder.constant_bit64(self.f64_type, v.to_bits()),
            other => panic!("SPIR-V: unsupported immediate/reference value {other:?}"),
        }
    }

    /// Store a result ID for an IR instruction.
    pub fn set_value(&mut self, block_idx: u32, inst_idx: u32, id: spirv::Word) {
        self.values.insert((block_idx, inst_idx), id);
    }

    /// Port of upstream `PatchPhiNodes` + Sirit's `PatchDeferredPhi`.
    pub(crate) fn patch_deferred_phis(&mut self) {
        for deferred in std::mem::take(&mut self.deferred_phis) {
            let values: Vec<_> = deferred
                .values
                .iter()
                .map(|value| self.resolve_value(value))
                .collect();
            let phi = self
                .builder
                .module_mut()
                .functions
                .iter_mut()
                .flat_map(|function| function.blocks.iter_mut())
                .flat_map(|block| block.instructions.iter_mut())
                .find(|inst| {
                    inst.class.opcode == spirv::Op::Phi
                        && inst.result_id == Some(deferred.result_id)
                })
                .unwrap_or_else(|| {
                    panic!(
                        "SPIR-V: deferred Phi result {} was not emitted",
                        deferred.result_id
                    )
                });
            assert_eq!(
                phi.operands.len(),
                values.len() * 2,
                "SPIR-V: deferred Phi operand count changed before patching"
            );
            for (index, value) in values.into_iter().enumerate() {
                phi.operands[index * 2] = Operand::IdRef(value);
            }
        }
    }

    /// Emit the complete program (entry point used by emit_spirv.rs).
    pub fn emit_program(&mut self, program: &ir::Program) {
        let mut bindings = Bindings::default();
        self.emit_program_with_bindings(program, &mut bindings);
    }

    pub fn emit_program_with_bindings(&mut self, program: &ir::Program, bindings: &mut Bindings) {
        super::emit_spirv::emit_into_context(self, program, bindings);
    }

    /// Finalize and return SPIR-V words.
    pub fn finalize(self) -> Vec<u32> {
        let module = self.builder.module();
        let mut words = Vec::new();
        module.assemble_into(&mut words);
        words
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::bindings::Bindings;
    use crate::ir::basic_block::Block;
    use crate::ir::emitter::Emitter;
    use crate::ir::instruction::Inst;
    use crate::ir::opcodes::Opcode;
    use crate::ir::types::{ShaderStage, TextureInstInfo, Type};
    use crate::ir::value::{Attribute, InstRef, Patch, Value};
    use crate::runtime_info::TransformFeedbackVarying;

    fn has_capability(ctx: &SpirvEmitContext, capability: spirv::Capability) -> bool {
        ctx.builder
            .module_ref()
            .capabilities
            .iter()
            .any(|instruction| {
                matches!(
                    instruction.operands.as_slice(),
                    [Operand::Capability(found)] if *found == capability
                )
            })
    }

    fn context_with_capabilities(
        program: &ir::Program,
        profile: &Profile,
        runtime_info: &RuntimeInfo,
    ) -> SpirvEmitContext {
        let mut ctx = SpirvEmitContext::new(program, profile, runtime_info);
        super::super::emit_spirv::setup_capabilities(
            profile,
            &program.info,
            program.stage,
            &mut ctx,
        );
        ctx
    }

    fn switch_literals_for_function(ctx: &SpirvEmitContext, function_id: spirv::Word) -> Vec<u32> {
        let function = ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .find(|function| {
                function
                    .def
                    .as_ref()
                    .and_then(|definition| definition.result_id)
                    == Some(function_id)
            })
            .expect("function must exist");
        let switch = function
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
            .find(|instruction| instruction.class.opcode == spirv::Op::Switch)
            .expect("function must contain OpSwitch");
        switch.operands[2..]
            .chunks_exact(2)
            .map(|operands| match operands[0] {
                Operand::LiteralBit32(value) => value,
                ref operand => panic!("unexpected switch literal {operand:?}"),
            })
            .collect()
    }

    #[test]
    fn indexed_attribute_load_switches_over_position_and_used_generics() {
        let mut program = ir::Program::new(ShaderStage::VertexB);
        program.info.loads_indexed_attributes = true;
        program
            .info
            .loads
            .set(Attribute::POSITION_X.0 as usize, true);
        program
            .info
            .loads
            .set(Attribute::generic(3, 2).0 as usize, true);
        let mut runtime_info = RuntimeInfo::default();
        runtime_info
            .previous_stage_stores
            .set(Attribute::generic(3, 2).0 as usize, true);
        let profile = Profile::default();
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        let mut bindings = Bindings::default();

        ctx.define_global_variables(&program, &mut bindings);

        assert_ne!(ctx.indexed_load_func, 0);
        assert_eq!(
            switch_literals_for_function(&ctx, ctx.indexed_load_func),
            vec![
                Attribute::POSITION_X.0 >> 2,
                Attribute::generic(3, 0).0 >> 2
            ]
        );
    }

    #[test]
    fn geometry_indexed_attribute_load_function_accepts_vertex_parameter() {
        let mut program = ir::Program::new(ShaderStage::Geometry);
        program.info.loads_indexed_attributes = true;
        program
            .info
            .loads
            .set(Attribute::POSITION_X.0 as usize, true);
        let profile = Profile::default();
        let runtime_info = RuntimeInfo::default();
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        let mut bindings = Bindings::default();

        ctx.define_global_variables(&program, &mut bindings);

        let function = ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .find(|function| {
                function
                    .def
                    .as_ref()
                    .and_then(|definition| definition.result_id)
                    == Some(ctx.indexed_load_func)
            })
            .expect("indexed load function must exist");
        assert_eq!(function.parameters.len(), 2);
    }

    #[test]
    fn indexed_attribute_store_switches_over_position_generic_and_clip_groups() {
        let mut program = ir::Program::new(ShaderStage::VertexB);
        program.info.stores_indexed_attributes = true;
        for attribute in [
            Attribute::POSITION_X,
            Attribute::generic(2, 1),
            Attribute::CLIP_DISTANCE_0,
            Attribute(Attribute::CLIP_DISTANCE_0.0 + 4),
        ] {
            program.info.stores.set(attribute.0 as usize, true);
        }
        let profile = Profile {
            max_user_clip_distances: 8,
            ..Profile::default()
        };
        let runtime_info = RuntimeInfo::default();
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        let mut bindings = Bindings::default();

        ctx.define_global_variables(&program, &mut bindings);

        assert_ne!(ctx.indexed_store_func, 0);
        assert_eq!(
            switch_literals_for_function(&ctx, ctx.indexed_store_func),
            vec![
                Attribute::POSITION_X.0 >> 2,
                Attribute::generic(2, 0).0 >> 2,
                Attribute::CLIP_DISTANCE_0.0 >> 2,
                (Attribute::CLIP_DISTANCE_0.0 + 4) >> 2,
            ]
        );
    }

    #[test]
    fn indexed_attribute_ir_calls_context_helpers_with_upstream_arguments() {
        let mut program = ir::Program::new(ShaderStage::Geometry);
        program.blocks.push(Block::new());
        {
            let mut emitter = Emitter::new(&mut program, 0);
            emitter.get_attribute_indexed(Value::ImmU32(0), Value::ImmU32(1));
            emitter.set_attribute_indexed(Value::ImmU32(0), Value::ImmF32(1.0), Value::ImmU32(1));
        }
        program.info.loads_indexed_attributes = true;
        program.info.stores_indexed_attributes = true;
        program
            .info
            .loads
            .set(Attribute::POSITION_X.0 as usize, true);
        program
            .info
            .stores
            .set(Attribute::POSITION_X.0 as usize, true);
        program.syntax_list = vec![ir::SyntaxNode::Block(0), ir::SyntaxNode::Return];

        let profile = Profile::default();
        let runtime_info = RuntimeInfo::default();
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        ctx.emit_program(&program);

        let calls = ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| instruction.class.opcode == spirv::Op::FunctionCall)
            .collect::<Vec<_>>();
        let load = calls
            .iter()
            .find(|call| call.operands[0] == Operand::IdRef(ctx.indexed_load_func))
            .expect("indexed load helper must be called");
        let store = calls
            .iter()
            .find(|call| call.operands[0] == Operand::IdRef(ctx.indexed_store_func))
            .expect("indexed store helper must be called");
        assert_eq!(load.operands.len(), 3);
        assert_eq!(store.operands.len(), 3);
    }

    #[test]
    fn non_unified_rescaling_uses_location_zero_uniform_constant() {
        let mut program = ir::Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        program
            .block_mut(0)
            .append_inst(Inst::new(Opcode::ResolutionDownFactor, vec![]));
        program.info.uses_rescaling_uniform = true;
        program.syntax_list = vec![ir::SyntaxNode::Block(0), ir::SyntaxNode::Return];
        let profile = Profile {
            unified_descriptor_binding: false,
            supported_spirv: 0x0001_0400,
            ..Profile::default()
        };
        let runtime_info = RuntimeInfo::default();
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);

        ctx.emit_program(&program);

        assert_ne!(ctx.rescaling_uniform_constant, 0);
        assert_eq!(ctx.rescaling_push_constants, 0);
        assert!(ctx.interfaces.contains(&ctx.rescaling_uniform_constant));
        assert!(ctx.builder.module_ref().annotations.iter().any(|inst| {
            matches!(
                inst.operands.as_slice(),
                [
                    Operand::IdRef(id),
                    Operand::Decoration(spirv::Decoration::Location),
                    Operand::LiteralBit32(0)
                ] if *id == ctx.rescaling_uniform_constant
            )
        }));
        let instructions = ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .collect::<Vec<_>>();
        let load = instructions
            .iter()
            .find(|instruction| {
                instruction.class.opcode == spirv::Op::Load
                    && instruction.operands.first()
                        == Some(&Operand::IdRef(ctx.rescaling_uniform_constant))
            })
            .expect("resolution factor must load the uniform constant");
        assert!(instructions.iter().any(|instruction| {
            instruction.class.opcode == spirv::Op::CompositeExtract
                && instruction.operands.first() == load.result_id.map(Operand::IdRef).as_ref()
                && instruction.operands.get(1) == Some(&Operand::LiteralBit32(2))
        }));
    }

    #[test]
    fn fragment_depth_declares_builtin_mode_and_store() {
        let mut program = ir::Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        {
            let mut emitter = Emitter::new(&mut program, 0);
            emitter.set_frag_depth(Value::ImmF32(0.25));
        }
        program.info.stores_frag_depth = true;
        program.syntax_list = vec![ir::SyntaxNode::Block(0), ir::SyntaxNode::Return];

        let profile = Profile::default();
        let runtime_info = RuntimeInfo {
            convert_depth_mode: true,
            ..RuntimeInfo::default()
        };
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        ctx.emit_program(&program);

        let frag_depth = ctx.frag_depth;
        let module = ctx.builder.module_ref();
        assert_ne!(frag_depth, 0);
        assert!(module.annotations.iter().any(|inst| {
            matches!(
                inst.operands.as_slice(),
                [
                    Operand::IdRef(id),
                    Operand::Decoration(spirv::Decoration::BuiltIn),
                    Operand::BuiltIn(spirv::BuiltIn::FragDepth)
                ] if *id == frag_depth
            )
        }));
        assert!(module.execution_modes.iter().any(|inst| {
            matches!(
                inst.operands.as_slice(),
                [
                    Operand::IdRef(_),
                    Operand::ExecutionMode(spirv::ExecutionMode::DepthReplacing)
                ]
            )
        }));
        assert!(module.functions.iter().any(|function| {
            function.blocks.iter().any(|block| {
                block.instructions.iter().any(|inst| {
                    inst.class.opcode == spirv::Op::Store
                        && matches!(inst.operands.first(), Some(Operand::IdRef(id)) if *id == frag_depth)
                })
            })
        }));
        assert!(module.functions.iter().any(|function| {
            function.blocks.iter().any(|block| {
                block
                    .instructions
                    .iter()
                    .any(|inst| inst.class.opcode == spirv::Op::ExtInst)
            })
        }));
    }

    #[test]
    fn descriptor_aliasing_uses_typed_scalar_cbuf_view() {
        let mut program = ir::Program::new(ShaderStage::Fragment);
        program.info.used_constant_buffer_types = Type::F32 as u32;
        program
            .info
            .constant_buffer_descriptors
            .push(ConstantBufferDescriptor { index: 3, count: 1 });
        let profile = Profile {
            unified_descriptor_binding: true,
            support_descriptor_aliasing: true,
            ..Profile::default()
        };
        let mut ctx = SpirvEmitContext::new(&program, &profile, &RuntimeInfo::default());
        let mut bindings = Bindings::default();

        ctx.define_global_variables(&program, &mut bindings);

        let definitions = ctx.cbufs.get(&3).expect("CB3 must be declared");
        assert_ne!(definitions.f32_scalar, 0);
        assert_eq!(definitions.u32x4, 0);
        assert_eq!(bindings.unified, 1);
        assert!(ctx.builder.module_ref().annotations.iter().any(|inst| {
            inst.class.opcode == spirv::Op::Decorate
                && matches!(
                    inst.operands.as_slice(),
                    [
                        Operand::IdRef(_),
                        Operand::Decoration(spirv::Decoration::ArrayStride),
                        Operand::LiteralBit32(4)
                    ]
                )
        }));
    }

    #[test]
    fn indirect_cbuf_accessors_switch_over_all_hardware_bindings() {
        let mut program = ir::Program::new(ShaderStage::Fragment);
        program.info.uses_cbuf_indirect = true;
        program.info.uses_int8 = true;
        program.info.uses_int16 = true;
        program.info.used_indirect_cbuf_types =
            Type::U8 as u32 | Type::U16 as u32 | Type::U32x4 as u32;
        program
            .info
            .constant_buffer_descriptors
            .push(ConstantBufferDescriptor {
                index: 0,
                count: crate::shader_info::Info::MAX_INDIRECT_CBUFS as u32,
            });
        let profile = Profile {
            unified_descriptor_binding: true,
            support_descriptor_aliasing: true,
            support_int8: true,
            support_uniform_and_storage_buffer_8bit: true,
            support_int16: true,
            support_uniform_and_storage_buffer_16bit: true,
            ..Profile::default()
        };
        let mut ctx = SpirvEmitContext::new(&program, &profile, &RuntimeInfo::default());
        let mut bindings = Bindings::default();

        ctx.define_global_variables(&program, &mut bindings);

        assert_ne!(ctx.load_const_func_u8, 0);
        assert_ne!(ctx.load_const_func_u16, 0);
        assert_ne!(ctx.load_const_func_u32x4, 0);
        assert_ne!(ctx.cbufs[&0].u8_scalar, 0);
        assert_ne!(ctx.cbufs[&0].i8_scalar, 0);
        assert_ne!(ctx.cbufs[&0].u16_scalar, 0);
        assert_ne!(ctx.cbufs[&0].i16_scalar, 0);
        let switches: Vec<_> = ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .filter(|inst| inst.class.opcode == spirv::Op::Switch)
            .collect();
        assert_eq!(switches.len(), 3);
        assert!(switches.iter().all(|switch| {
            switch.operands.len() == 2 + crate::shader_info::Info::MAX_INDIRECT_CBUFS * 2
        }));
        assert_eq!(bindings.unified, 1);
    }

    #[test]
    fn descriptor_aliasing_declares_all_used_typed_ssbo_views() {
        let mut program = ir::Program::new(ShaderStage::Compute);
        program.info.uses_int8 = true;
        program.info.uses_int16 = true;
        program.info.uses_int64 = true;
        program.info.used_storage_buffer_types = Type::U8 as u32
            | Type::U16 as u32
            | Type::U32 as u32
            | Type::F32 as u32
            | Type::U64 as u32
            | Type::U32x2 as u32
            | Type::U32x4 as u32;
        program.info.storage_buffers_descriptors =
            vec![crate::shader_info::StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 2,
                is_written: true,
            }];
        let profile = Profile {
            unified_descriptor_binding: true,
            support_descriptor_aliasing: true,
            support_int8: true,
            support_storage_buffer_8bit: true,
            support_int16: true,
            support_storage_buffer_16bit: true,
            support_int64: true,
            ..Profile::default()
        };
        let mut ctx = SpirvEmitContext::new(&program, &profile, &RuntimeInfo::default());
        let mut bindings = Bindings::default();

        ctx.define_global_variables(&program, &mut bindings);

        let ssbo = ctx.ssbos[&0];
        assert_ne!(ssbo.u8_scalar, 0);
        assert_ne!(ssbo.i8_scalar, 0);
        assert_ne!(ssbo.u16_scalar, 0);
        assert_ne!(ssbo.i16_scalar, 0);
        assert_ne!(ssbo.u32_scalar, 0);
        assert_ne!(ssbo.f32_scalar, 0);
        assert_ne!(ssbo.u64_scalar, 0);
        assert_ne!(ssbo.u32x2, 0);
        assert_ne!(ssbo.u32x4, 0);
        assert_eq!(ctx.ssbos[&1].u32x4, ssbo.u32x4);
        assert_eq!(bindings.unified, 2);
        assert_ne!(ctx.storage_types.u8_scalar.element, 0);
        assert_ne!(ctx.storage_types.u32x4.element, 0);
    }

    #[test]
    fn storage_subword_fallback_defines_cas_loop() {
        let mut program = ir::Program::new(ShaderStage::Compute);
        program.info.uses_int8 = true;
        program.info.used_storage_buffer_types = Type::U32 as u32;
        program.info.storage_buffers_descriptors =
            vec![crate::shader_info::StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                is_written: true,
            }];
        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());
        let mut bindings = Bindings::default();

        ctx.define_global_variables(&program, &mut bindings);

        assert_ne!(ctx.write_storage_cas_loop_func, 0);
        assert!(has_capability(
            &ctx,
            spirv::Capability::VariablePointersStorageBuffer
        ));
        assert!(ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .any(|inst| inst.class.opcode == spirv::Op::AtomicCompareExchange));
    }

    #[test]
    fn global_memory_helpers_search_nvn_ssbo_ranges() {
        let mut program = ir::Program::new(ShaderStage::Compute);
        program.info.uses_int64 = true;
        program.info.uses_global_memory = true;
        program.info.nvn_buffer_used = 1;
        program.info.used_constant_buffer_types = Type::U32 as u32 | Type::U32x2 as u32;
        program.info.used_storage_buffer_types =
            Type::U32 as u32 | Type::U32x2 as u32 | Type::U32x4 as u32;
        program
            .info
            .constant_buffer_descriptors
            .push(ConstantBufferDescriptor { index: 0, count: 1 });
        program.info.storage_buffers_descriptors =
            vec![crate::shader_info::StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 16,
                count: 1,
                is_written: true,
            }];
        let profile = Profile {
            unified_descriptor_binding: true,
            support_descriptor_aliasing: true,
            support_int64: true,
            min_ssbo_alignment: 16,
            ..Profile::default()
        };
        let mut ctx = SpirvEmitContext::new(&program, &profile, &RuntimeInfo::default());
        ctx.define_global_variables(&program, &mut Bindings::default());

        assert_ne!(ctx.load_global_func_u32, 0);
        assert_ne!(ctx.load_global_func_u32x2, 0);
        assert_ne!(ctx.load_global_func_u32x4, 0);
        assert_ne!(ctx.write_global_func_u32, 0);
        assert_ne!(ctx.write_global_func_u32x2, 0);
        assert_ne!(ctx.write_global_func_u32x4, 0);
        let instructions = ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter());
        let opcodes: Vec<_> = instructions.map(|inst| inst.class.opcode).collect();
        assert!(opcodes.contains(&spirv::Op::UGreaterThanEqual));
        assert!(opcodes.contains(&spirv::Op::ULessThan));
        assert!(opcodes.contains(&spirv::Op::LogicalAnd));
        assert!(opcodes.contains(&spirv::Op::ISub));
    }

    #[test]
    fn demote_capability_and_extension_are_usage_gated() {
        let mut unused = ir::Program::new(ShaderStage::Fragment);
        let profile = Profile {
            supported_spirv: 0x0001_0500,
            support_demote_to_helper_invocation: true,
            ..Profile::default()
        };
        let runtime_info = RuntimeInfo::default();
        let unused_ctx = context_with_capabilities(&unused, &profile, &runtime_info);
        assert!(!unused_ctx
            .builder
            .module_ref()
            .capabilities
            .iter()
            .any(|inst| {
                matches!(
                    inst.operands.as_slice(),
                    [Operand::Capability(
                        spirv::Capability::DemoteToHelperInvocation
                    )]
                )
            }));

        unused.info.uses_demote_to_helper_invocation = true;
        let used_ctx = context_with_capabilities(&unused, &profile, &runtime_info);
        let module = used_ctx.builder.module_ref();
        assert!(module.capabilities.iter().any(|inst| {
            matches!(
                inst.operands.as_slice(),
                [Operand::Capability(
                    spirv::Capability::DemoteToHelperInvocation
                )]
            )
        }));
        assert!(module.extensions.iter().any(|inst| {
            matches!(inst.operands.as_slice(), [Operand::LiteralString(name)]
                if name == "SPV_EXT_demote_to_helper_invocation")
        }));
    }

    #[test]
    fn float_control_extension_and_signed_zero_mode_follow_profile() {
        let mut program = ir::Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        program.syntax_list = vec![ir::SyntaxNode::Block(0), ir::SyntaxNode::Return];
        let profile = Profile {
            supported_spirv: 0x0001_0600,
            support_float_controls: true,
            support_fp32_signed_zero_nan_preserve: true,
            ..Profile::default()
        };
        let runtime_info = RuntimeInfo::default();
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        ctx.emit_program(&program);
        let module = ctx.builder.module_ref();

        assert!(module.extensions.iter().any(|inst| {
            matches!(inst.operands.as_slice(), [Operand::LiteralString(name)]
                if name == "SPV_KHR_float_controls")
        }));
        assert!(module.capabilities.iter().any(|inst| {
            matches!(
                inst.operands.as_slice(),
                [Operand::Capability(
                    spirv::Capability::SignedZeroInfNanPreserve
                )]
            )
        }));
        assert!(module.execution_modes.iter().any(|inst| {
            matches!(
                inst.operands.as_slice(),
                [
                    Operand::IdRef(_),
                    Operand::ExecutionMode(spirv::ExecutionMode::SignedZeroInfNanPreserve),
                    Operand::LiteralBit32(32)
                ]
            )
        }));
    }

    #[test]
    fn vertex_id_declares_vertex_index_and_base_vertex_without_vertex_id_support() {
        let mut program = ir::Program::new(ShaderStage::VertexB);
        program
            .info
            .loads
            .set(Attribute::VERTEX_ID.0 as usize, true);

        let profile = Profile {
            support_vertex_instance_id: false,
            ..Profile::default()
        };
        let runtime_info = RuntimeInfo::default();
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        let mut bindings = Bindings::default();

        ctx.define_global_variables(&program, &mut bindings);

        assert_eq!(ctx.vertex_id, 0);
        assert_ne!(ctx.vertex_index, 0);
        assert_ne!(ctx.base_vertex, 0);
        assert!(ctx.interfaces.contains(&ctx.vertex_index));
        assert!(ctx.interfaces.contains(&ctx.base_vertex));
    }

    #[test]
    fn vertex_id_declares_vertex_id_when_supported() {
        let mut program = ir::Program::new(ShaderStage::VertexB);
        program
            .info
            .loads
            .set(Attribute::VERTEX_ID.0 as usize, true);

        let profile = Profile {
            support_vertex_instance_id: true,
            ..Profile::default()
        };
        let runtime_info = RuntimeInfo::default();
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        let mut bindings = Bindings::default();

        ctx.define_global_variables(&program, &mut bindings);

        assert_ne!(ctx.vertex_id, 0);
        assert_eq!(ctx.vertex_index, 0);
        assert_eq!(ctx.base_vertex, 0);
        assert!(ctx.interfaces.contains(&ctx.vertex_id));
    }

    #[test]
    fn subgroup_mask_loads_declared_builtin_and_extracts_host_warp_partition() {
        let mut program = ir::Program::new(ShaderStage::Compute);
        program.blocks.push(Block::new());
        let result = {
            let mut emitter = Emitter::new(&mut program, 0);
            emitter.subgroup_eq_mask()
        };
        let Value::Inst(result_ref) = result else {
            panic!("subgroup mask should be an instruction value");
        };
        program.info.uses_subgroup_mask = true;
        program.syntax_list = vec![ir::SyntaxNode::Block(0), ir::SyntaxNode::Return];

        let profile = Profile {
            warp_size_potentially_larger_than_guest: true,
            ..Profile::default()
        };
        let runtime_info = RuntimeInfo::default();
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        ctx.emit_program(&program);

        let result_id = ctx.values[&(result_ref.block, result_ref.inst)];
        assert_ne!(ctx.subgroup_mask_eq, 0);
        assert_ne!(ctx.subgroup_local_invocation_id, 0);
        assert!(ctx.builder.module_ref().functions.iter().any(|function| {
            function.blocks.iter().any(|block| {
                block.instructions.iter().any(|inst| {
                    inst.class.opcode == spirv::Op::VectorExtractDynamic
                        && inst.result_id == Some(result_id)
                })
            })
        }));
    }

    #[test]
    fn fragment_outputs_use_context_owned_color_and_sample_mask_variables() {
        let mut program = ir::Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        {
            let mut emitter = Emitter::new(&mut program, 0);
            emitter.set_frag_color(Value::ImmU32(2), Value::ImmU32(1), Value::ImmF32(0.5));
            emitter.set_sample_mask(Value::ImmU32(0x55aa));
        }
        program.info.stores_frag_color[2] = true;
        program.info.stores_sample_mask = true;
        program.syntax_list = vec![ir::SyntaxNode::Block(0), ir::SyntaxNode::Return];

        let profile = Profile::default();
        let runtime_info = RuntimeInfo::default();
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        ctx.emit_program(&program);

        assert_ne!(ctx.frag_color[2], 0);
        assert_ne!(ctx.sample_mask, 0);
        let stores = ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .filter(|inst| inst.class.opcode == spirv::Op::Store)
            .count();
        assert_eq!(stores, 2);
    }

    #[test]
    fn fragment_dual_source_outputs_share_location_and_use_distinct_indices() {
        let mut program = ir::Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        program.info.stores_frag_color[0] = true;
        program.syntax_list = vec![ir::SyntaxNode::Block(0), ir::SyntaxNode::Return];

        let profile = Profile::default();
        let runtime_info = RuntimeInfo {
            dual_source_blend: true,
            ..RuntimeInfo::default()
        };
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        ctx.emit_program(&program);

        let primary = ctx.frag_color[0];
        let secondary = ctx.frag_color[1];
        assert_ne!(primary, 0);
        assert_ne!(secondary, 0);
        for (id, index) in [(primary, 0), (secondary, 1)] {
            assert!(ctx.builder.module_ref().annotations.iter().any(|annotation| {
                matches!(
                    annotation.operands.as_slice(),
                    [
                        Operand::IdRef(target),
                        Operand::Decoration(spirv::Decoration::Location),
                        Operand::LiteralBit32(0)
                    ] if *target == id
                )
            }));
            assert!(ctx.builder.module_ref().annotations.iter().any(|annotation| {
                matches!(
                    annotation.operands.as_slice(),
                    [
                        Operand::IdRef(target),
                        Operand::Decoration(spirv::Decoration::Index),
                        Operand::LiteralBit32(value)
                    ] if *target == id && *value == index
                )
            }));
        }
    }

    #[test]
    fn tessellation_patch_outputs_preserve_upstream_outer_level_order() {
        let mut program = ir::Program::new(ShaderStage::TessellationControl);
        program.blocks.push(Block::new());
        program.invocations = 4;
        {
            let mut emitter = Emitter::new(&mut program, 0);
            emitter.set_patch(Patch::TESS_LOD_TOP, Value::ImmF32(2.0));
            emitter.set_patch(Patch::generic(3, 2), Value::ImmF32(3.0));
        }
        program.info.stores_tess_level_outer = true;
        program.info.uses_patches[3] = true;
        program.syntax_list = vec![ir::SyntaxNode::Block(0), ir::SyntaxNode::Return];

        let profile = Profile::default();
        let runtime_info = RuntimeInfo::default();
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        ctx.emit_program(&program);

        assert_ne!(ctx.output_tess_level_outer, 0);
        assert_ne!(ctx.patches[3], 0);
        let top_index = ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .find_map(|inst| {
                (inst.class.opcode == spirv::Op::AccessChain
                    && matches!(
                        inst.operands.first(),
                        Some(Operand::IdRef(id)) if *id == ctx.output_tess_level_outer
                    ))
                .then(|| match inst.operands.last() {
                    Some(Operand::IdRef(id)) => *id,
                    other => panic!("unexpected tessellation index operand {other:?}"),
                })
            })
            .expect("outer tessellation store must access its output array");
        assert!(ctx
            .builder
            .module_ref()
            .types_global_values
            .iter()
            .any(|inst| {
                inst.class.opcode == spirv::Op::Constant
                    && inst.result_id == Some(top_index)
                    && matches!(
                        inst.operands.as_slice(),
                        [Operand::LiteralBit32(value)] if *value == Patch::TESS_LOD_TOP.0
                    )
            }));
    }

    #[test]
    fn generic_output_ignores_transform_feedback_entries_past_xfb_count() {
        let mut program = ir::Program::new(ShaderStage::VertexB);
        program
            .info
            .stores
            .set(Attribute::generic(0, 0).0 as usize, true);
        let varying = TransformFeedbackVarying {
            buffer: 1,
            stream: 0,
            stride: 16,
            offset: 4,
            components: 1,
        };
        let mut runtime_info = RuntimeInfo {
            xfb_varyings: vec![TransformFeedbackVarying::default(); 33],
            xfb_count: 0,
            ..RuntimeInfo::default()
        };
        runtime_info.xfb_varyings[32] = varying;

        let profile = Profile::default();
        let mut without_xfb = SpirvEmitContext::new(&program, &profile, &runtime_info);
        let mut bindings = Bindings::default();
        without_xfb.define_global_variables(&program, &mut bindings);
        assert!(!without_xfb
            .builder
            .module_ref()
            .annotations
            .iter()
            .any(|inst| matches!(
                inst.operands.as_slice(),
                [
                    Operand::IdRef(_),
                    Operand::Decoration(spirv::Decoration::XfbBuffer),
                    ..
                ]
            )));

        runtime_info.xfb_count = 33;
        let mut with_xfb = SpirvEmitContext::new(&program, &profile, &runtime_info);
        let mut bindings = Bindings::default();
        with_xfb.define_global_variables(&program, &mut bindings);
        assert!(with_xfb
            .builder
            .module_ref()
            .annotations
            .iter()
            .any(|inst| matches!(
                inst.operands.as_slice(),
                [
                    Operand::IdRef(_),
                    Operand::Decoration(spirv::Decoration::XfbBuffer),
                    Operand::LiteralBit32(1)
                ]
            )));
    }

    #[test]
    fn geometry_transform_feedback_output_uses_stream_decoration() {
        let mut program = ir::Program::new(ShaderStage::Geometry);
        program
            .info
            .stores
            .set(Attribute::generic(0, 0).0 as usize, true);
        let base = Attribute::generic(0, 0).0 as usize;
        let mut runtime_info = RuntimeInfo {
            xfb_varyings: vec![TransformFeedbackVarying::default(); base + 1],
            xfb_count: (base + 1) as u32,
            ..RuntimeInfo::default()
        };
        runtime_info.xfb_varyings[base] = TransformFeedbackVarying {
            buffer: 1,
            stream: 2,
            stride: 16,
            offset: 0,
            components: 4,
        };

        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &runtime_info);
        ctx.define_global_variables(&program, &mut Bindings::default());

        assert!(ctx.builder.module_ref().annotations.iter().any(|inst| {
            matches!(
                inst.operands.as_slice(),
                [
                    Operand::IdRef(_),
                    Operand::Decoration(spirv::Decoration::Stream),
                    Operand::LiteralBit32(2)
                ]
            )
        }));
    }

    /// Declares generic attribute 0 as a fragment input of `input_type` with
    /// `Interpolation::Smooth`, then reports whether it was decorated `Flat`.
    fn generic_input_is_flat(input_type: AttributeType) -> bool {
        let mut program = ir::Program::new(ShaderStage::Fragment);
        program
            .info
            .loads
            .set(Attribute::generic(0, 0).0 as usize, true);
        program.info.interpolation[0] = crate::shader_info::Interpolation::Smooth;

        let mut runtime_info = RuntimeInfo::default();
        runtime_info.generic_input_types[0] = input_type;
        runtime_info
            .previous_stage_stores
            .set(Attribute::generic(0, 0).0 as usize, true);

        let profile = Profile::default();
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        ctx.define_global_variables(&program, &mut Bindings::default());

        let id = *ctx.input_vars.get(&0).expect("generic input 0 declared");
        ctx.builder.module_ref().annotations.iter().any(|inst| {
            inst.class.opcode == spirv::Op::Decorate
                && matches!(inst.operands.as_slice(), [
                    Operand::IdRef(decorated),
                    Operand::Decoration(spirv::Decoration::Flat),
                ] if *decorated == id)
        })
    }

    /// SPIR-V forbids interpolating integer fragment inputs, so upstream
    /// decorates them `Flat` even when the recorded interpolation is Smooth.
    #[test]
    fn integer_fragment_inputs_are_always_flat() {
        assert!(generic_input_is_flat(AttributeType::SignedInt));
        assert!(generic_input_is_flat(AttributeType::UnsignedInt));
    }

    #[test]
    fn smooth_float_fragment_inputs_are_not_flat() {
        assert!(!generic_input_is_flat(AttributeType::Float));
    }

    #[test]
    fn fragment_position_load_declares_frag_coord_input() {
        let mut program = ir::Program::new(ShaderStage::Fragment);
        program
            .info
            .loads
            .set(Attribute::POSITION_W.0 as usize, true);

        let profile = Profile::default();
        let runtime_info = RuntimeInfo::default();
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        let mut bindings = Bindings::default();

        ctx.define_global_variables(&program, &mut bindings);

        assert_ne!(ctx.input_position, 0);
        assert!(ctx.interfaces.contains(&ctx.input_position));

        let input_position = ctx.input_position;
        let module = ctx.builder.module();
        let has_frag_coord = module.annotations.iter().any(|inst| {
            inst.class.opcode == spirv::Op::Decorate
                && matches!(inst.operands.as_slice(), [
                    Operand::IdRef(id),
                    Operand::Decoration(spirv::Decoration::BuiltIn),
                    Operand::BuiltIn(spirv::BuiltIn::FragCoord),
                ] if *id == input_position)
        });
        let has_position = module.annotations.iter().any(|inst| {
            inst.class.opcode == spirv::Op::Decorate
                && matches!(inst.operands.as_slice(), [
                    Operand::IdRef(id),
                    Operand::Decoration(spirv::Decoration::BuiltIn),
                    Operand::BuiltIn(spirv::BuiltIn::Position),
                ] if *id == input_position)
        });

        assert!(has_frag_coord);
        assert!(!has_position);
    }

    #[test]
    fn sampled_array_texture_preserves_upstream_image_type_flags() {
        let mut program = ir::Program::new(ShaderStage::Fragment);
        program.info.texture_descriptors.push(TextureDescriptor {
            texture_type: TextureType::ColorArray2D,
            is_depth: true,
            is_multisample: true,
            is_integer: false,
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count: 1,
            size_shift: 0,
        });

        let profile = Profile::default();
        let runtime_info = RuntimeInfo::default();
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        let mut bindings = Bindings::default();
        ctx.define_global_variables(&program, &mut bindings);

        let image_type = ctx
            .builder
            .module_ref()
            .types_global_values
            .iter()
            .find(|inst| inst.class.opcode == spirv::Op::TypeImage)
            .expect("sampled texture must declare OpTypeImage");
        assert!(matches!(
            image_type.operands.as_slice(),
            [
                Operand::IdRef(_),
                Operand::Dim(spirv::Dim::Dim2D),
                Operand::LiteralBit32(1),
                Operand::LiteralBit32(1),
                Operand::LiteralBit32(1),
                Operand::LiteralBit32(1),
                Operand::ImageFormat(spirv::ImageFormat::Unknown),
            ]
        ));
    }

    #[test]
    fn texel_buffer_advances_unified_binding_before_later_stage_textures() {
        fn binding_of(ctx: &SpirvEmitContext, id: spirv::Word) -> u32 {
            ctx.builder
                .module_ref()
                .annotations
                .iter()
                .find_map(|inst| match inst.operands.as_slice() {
                    [
                        Operand::IdRef(target),
                        Operand::Decoration(spirv::Decoration::Binding),
                        Operand::LiteralBit32(binding),
                    ] if *target == id => Some(*binding),
                    _ => None,
                })
                .expect("resource must have a binding")
        }

        let profile = Profile {
            unified_descriptor_binding: true,
            ..Profile::default()
        };
        let runtime_info = RuntimeInfo::default();
        let mut bindings = Bindings::default();

        let mut vertex = ir::Program::new(ShaderStage::VertexB);
        vertex
            .info
            .constant_buffer_descriptors
            .push(ConstantBufferDescriptor { index: 1, count: 1 });
        vertex
            .info
            .texture_buffer_descriptors
            .push(TextureBufferDescriptor {
                has_secondary: false,
                cbuf_index: 0,
                cbuf_offset: 0,
                shift_left: 0,
                secondary_cbuf_index: 0,
                secondary_cbuf_offset: 0,
                secondary_shift_left: 0,
                count: 1,
                size_shift: 0,
            });
        let mut vertex_ctx = SpirvEmitContext::new(&vertex, &profile, &runtime_info);
        vertex_ctx.define_global_variables(&vertex, &mut bindings);
        assert_eq!(bindings.unified, 2);
        assert_eq!(binding_of(&vertex_ctx, vertex_ctx.texture_buffers[0].id), 1);

        let texture = TextureDescriptor {
            texture_type: TextureType::Color2D,
            is_depth: false,
            is_multisample: false,
            is_integer: false,
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count: 1,
            size_shift: 0,
        };
        let mut fragment = ir::Program::new(ShaderStage::Fragment);
        fragment.info.texture_descriptors = vec![texture.clone(), texture];
        let mut fragment_ctx = SpirvEmitContext::new(&fragment, &profile, &runtime_info);
        fragment_ctx.define_global_variables(&fragment, &mut bindings);

        assert_eq!(bindings.unified, 4);
        assert_eq!(binding_of(&fragment_ctx, fragment_ctx.textures[0].id), 2);
        assert_eq!(binding_of(&fragment_ctx, fragment_ctx.textures[1].id), 3);
    }

    #[test]
    fn texel_buffer_declares_sampled_buffer_capability() {
        let mut program = ir::Program::new(ShaderStage::VertexB);
        program
            .info
            .texture_buffer_descriptors
            .push(TextureBufferDescriptor {
                has_secondary: false,
                cbuf_index: 0,
                cbuf_offset: 0,
                shift_left: 0,
                secondary_cbuf_index: 0,
                secondary_cbuf_offset: 0,
                secondary_shift_left: 0,
                count: 1,
                size_shift: 0,
            });

        let context =
            context_with_capabilities(&program, &Profile::default(), &RuntimeInfo::default());
        assert!(has_capability(&context, spirv::Capability::SampledBuffer));
    }

    #[test]
    fn image_buffer_array_declaration_matches_upstream() {
        let mut program = ir::Program::new(ShaderStage::Fragment);
        program
            .info
            .image_buffer_descriptors
            .push(ImageBufferDescriptor {
                format: ImageFormat::R32Uint,
                is_written: true,
                is_read: true,
                is_integer: true,
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 4,
                size_shift: 0,
            });

        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());
        ctx.define_global_variables(&program, &mut Bindings::default());

        assert_eq!(ctx.image_buffers.len(), 1);
        assert_eq!(ctx.image_buffers[0].count, 4);
    }

    #[test]
    fn setup_capabilities_matches_upstream_usage_gates() {
        let unused = ir::Program::new(ShaderStage::VertexB);
        let unused_ctx =
            context_with_capabilities(&unused, &Profile::default(), &RuntimeInfo::default());
        assert!(!has_capability(&unused_ctx, spirv::Capability::Sampled1D));
        assert!(!has_capability(
            &unused_ctx,
            spirv::Capability::DrawParameters
        ));
        assert!(!has_capability(
            &unused_ctx,
            spirv::Capability::DerivativeControl
        ));

        let mut used = ir::Program::new(ShaderStage::VertexB);
        used.info.uses_sampled_1d = true;
        used.info.uses_derivatives = true;
        used.info.uses_typeless_image_reads = true;
        used.info.uses_typeless_image_writes = true;
        used.info.uses_image_buffers = true;
        used.info.loads.set(Attribute::VERTEX_ID.0 as usize, true);
        let profile = Profile {
            support_typeless_image_loads: true,
            support_vertex_instance_id: false,
            ..Profile::default()
        };
        let used_ctx = context_with_capabilities(&used, &profile, &RuntimeInfo::default());

        for capability in [
            spirv::Capability::Sampled1D,
            spirv::Capability::DerivativeControl,
            spirv::Capability::StorageImageReadWithoutFormat,
            spirv::Capability::StorageImageWriteWithoutFormat,
            spirv::Capability::ImageBuffer,
            spirv::Capability::DrawParameters,
        ] {
            assert!(
                has_capability(&used_ctx, capability),
                "missing capability {capability:?}"
            );
        }
        assert!(used_ctx.builder.module_ref().extensions.iter().any(|inst| {
            matches!(
                inst.operands.as_slice(),
                [Operand::LiteralString(extension)]
                    if extension == "SPV_KHR_shader_draw_parameters"
            )
        }));
    }

    #[test]
    fn setup_capabilities_respects_subgroup_stage_support() {
        let mut program = ir::Program::new(ShaderStage::Fragment);
        program.info.uses_subgroup_vote = true;
        let unsupported = Profile {
            support_vote: true,
            supported_subgroup_stages: 0,
            ..Profile::default()
        };
        let unsupported_ctx =
            context_with_capabilities(&program, &unsupported, &RuntimeInfo::default());
        assert!(!has_capability(
            &unsupported_ctx,
            spirv::Capability::GroupNonUniformBallot
        ));

        let supported = Profile {
            supported_subgroup_stages: 1 << ShaderStage::Fragment as u32,
            ..unsupported
        };
        let supported_ctx =
            context_with_capabilities(&program, &supported, &RuntimeInfo::default());
        assert!(has_capability(
            &supported_ctx,
            spirv::Capability::GroupNonUniformBallot
        ));
    }

    #[test]
    fn setup_capabilities_declares_image_1d() {
        let mut program = ir::Program::new(ShaderStage::Fragment);
        program.info.uses_image_1d = true;
        let ctx = context_with_capabilities(&program, &Profile::default(), &RuntimeInfo::default());
        assert!(has_capability(&ctx, spirv::Capability::Image1D));
    }

    #[test]
    fn w_scale_factor_stub_matches_upstream_value() {
        let mut program = ir::Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        program
            .block_mut(0)
            .append_inst(Inst::new(Opcode::SRWScaleFactorXY, vec![]));

        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());
        ctx.emit_program(&program);

        assert!(ctx
            .builder
            .module_ref()
            .types_global_values
            .iter()
            .any(|inst| {
                inst.class.opcode == spirv::Op::Constant
                    && matches!(
                        inst.operands.as_slice(),
                        [Operand::LiteralBit32(0x00ff_0000)]
                    )
            }));
    }

    fn backend_panic_for(opcode: Opcode, args: Vec<Value>) -> Box<dyn std::any::Any + Send> {
        let mut program = ir::Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        program.block_mut(0).append_inst(Inst::new(opcode, args));
        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ctx.emit_program(&program)))
            .expect_err("opcode must fail like its upstream emitter")
    }

    #[test]
    fn register_opcode_reaching_spirv_is_a_logic_error() {
        let payload = backend_panic_for(Opcode::GetRegister, vec![Value::Reg(ir::Reg(0))]);
        let error = payload
            .downcast_ref::<crate::exception::LogicError>()
            .expect("typed LogicError");
        assert_eq!(error.0, "Unreachable instruction");
    }

    #[test]
    fn flag_opcode_reaching_spirv_is_not_implemented() {
        let payload = backend_panic_for(Opcode::GetZFlag, vec![]);
        let error = payload
            .downcast_ref::<crate::exception::NotImplementedException>()
            .expect("typed NotImplementedException");
        assert_eq!(error.0, "SPIR-V Instruction is not implemented");
    }

    #[test]
    fn texel_buffer_fetch_applies_offset_without_lod() {
        let mut program = ir::Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        program
            .info
            .texture_buffer_descriptors
            .push(TextureBufferDescriptor {
                has_secondary: false,
                cbuf_index: 0,
                cbuf_offset: 0,
                shift_left: 0,
                secondary_cbuf_index: 0,
                secondary_cbuf_offset: 0,
                secondary_shift_left: 0,
                count: 1,
                size_shift: 0,
            });
        let info = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::Buffer as u8,
            ..TextureInstInfo::default()
        };
        program.block_mut(0).append_inst(Inst::with_flags(
            Opcode::ImageFetch,
            vec![
                Value::ImmU32(0),
                Value::ImmU32(4),
                Value::ImmU32(2),
                Value::Void,
                Value::Void,
            ],
            info.to_u32(),
        ));
        program.syntax_list = vec![ir::SyntaxNode::Block(0), ir::SyntaxNode::Return];

        let profile = Profile::default();
        let runtime_info = RuntimeInfo::default();
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        ctx.emit_program(&program);

        let instructions = ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .collect::<Vec<_>>();
        let add = instructions
            .iter()
            .find(|inst| inst.class.opcode == spirv::Op::IAdd)
            .expect("buffer AOFFI must be added to the texel coordinate");
        let fetch = instructions
            .iter()
            .find(|inst| inst.class.opcode == spirv::Op::ImageFetch)
            .expect("buffer fetch must emit OpImageFetch");

        assert_eq!(fetch.operands.len(), 2, "buffer fetch must not carry LOD");
        assert_eq!(fetch.operands[1], Operand::IdRef(add.result_id.unwrap()));
    }

    #[test]
    fn phi_values_are_patched_after_forward_definitions() {
        let mut program = ir::Program::new(ShaderStage::VertexB);
        let phi_block = program.add_block();
        let value_block = program.add_block();

        let value_inst = program.block_mut(value_block).append_inst(Inst::new(
            Opcode::IAdd32,
            vec![Value::ImmU32(3), Value::ImmU32(4)],
        ));
        let mut phi = Inst::phi();
        phi.flags = Type::U32 as u32;
        phi.add_phi_operand(
            value_block,
            Value::Inst(InstRef {
                block: value_block,
                inst: value_inst,
            }),
        );
        let phi_inst = program.block_mut(phi_block).append_inst(phi);

        let profile = Profile::default();
        let runtime_info = RuntimeInfo::default();
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        ctx.emit_program(&program);

        let phi_id = ctx.values[&(phi_block, phi_inst)];
        let value_id = ctx.values[&(value_block, value_inst)];
        let emitted_phi = ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .find(|inst| inst.result_id == Some(phi_id))
            .expect("Phi result was not emitted");

        assert_eq!(emitted_phi.class.opcode, spirv::Op::Phi);
        assert_eq!(emitted_phi.operands[0], Operand::IdRef(value_id));
        assert_ne!(emitted_phi.operands[0], Operand::IdRef(0));
    }

    #[test]
    fn f16_f2i_pipeline_emits_every_translated_opcode() {
        let mut program = ir::Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());

        let value = program
            .block_mut(0)
            .append_inst(Inst::new(Opcode::ConvertF16F32, vec![Value::ImmF32(1.5)]));
        let min = program
            .block_mut(0)
            .append_inst(Inst::new(Opcode::ConvertF16F32, vec![Value::ImmF32(0.0)]));
        let max = program
            .block_mut(0)
            .append_inst(Inst::new(Opcode::ConvertF16F32, vec![Value::ImmF32(2.0)]));
        let pair = program.block_mut(0).append_inst(Inst::new(
            Opcode::CompositeConstructF16x2,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: value,
                }),
                Value::Inst(InstRef {
                    block: 0,
                    inst: min,
                }),
            ],
        ));
        let extracted = program.block_mut(0).append_inst(Inst::new(
            Opcode::CompositeExtractF16x2,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: pair,
                }),
                Value::ImmU32(0),
            ],
        ));
        let inserted = program.block_mut(0).append_inst(Inst::new(
            Opcode::CompositeInsertF16x2,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: pair,
                }),
                Value::Inst(InstRef {
                    block: 0,
                    inst: max,
                }),
                Value::ImmU32(1),
            ],
        ));
        program.block_mut(0).append_inst(Inst::new(
            Opcode::PackFloat2x16,
            vec![Value::Inst(InstRef {
                block: 0,
                inst: inserted,
            })],
        ));
        let promoted = program.block_mut(0).append_inst(Inst::new(
            Opcode::ConvertF32F16,
            vec![Value::Inst(InstRef {
                block: 0,
                inst: extracted,
            })],
        ));
        let lowered = program.block_mut(0).append_inst(Inst::new(
            Opcode::ConvertF16F32,
            vec![Value::Inst(InstRef {
                block: 0,
                inst: promoted,
            })],
        ));
        let multiplied = program.block_mut(0).append_inst(Inst::new(
            Opcode::FPMul16,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: lowered,
                }),
                Value::Inst(InstRef {
                    block: 0,
                    inst: max,
                }),
            ],
        ));
        let rounded = program.block_mut(0).append_inst(Inst::new(
            Opcode::FPRoundEven16,
            vec![Value::Inst(InstRef {
                block: 0,
                inst: multiplied,
            })],
        ));
        let clamped = program.block_mut(0).append_inst(Inst::new(
            Opcode::FPClamp16,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: rounded,
                }),
                Value::Inst(InstRef {
                    block: 0,
                    inst: min,
                }),
                Value::Inst(InstRef {
                    block: 0,
                    inst: max,
                }),
            ],
        ));
        program.block_mut(0).append_inst(Inst::new(
            Opcode::ConvertS32F16,
            vec![Value::Inst(InstRef {
                block: 0,
                inst: clamped,
            })],
        ));
        program.syntax_list = vec![ir::SyntaxNode::Block(0), ir::SyntaxNode::Return];
        crate::ir_opt::collect_shader_info_pass::collect_shader_info_pass(&mut program);
        assert!(program.info.uses_fp16);

        let profile = Profile::default();
        let runtime_info = RuntimeInfo::default();
        let mut ctx = context_with_capabilities(&program, &profile, &runtime_info);
        ctx.emit_program(&program);

        let emitted = ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .map(|inst| inst.class.opcode)
            .collect::<Vec<_>>();
        assert!(emitted.contains(&spirv::Op::FConvert));
        assert!(emitted.contains(&spirv::Op::FMul));
        assert!(emitted.contains(&spirv::Op::ExtInst));
        assert!(emitted.contains(&spirv::Op::ConvertFToS));
        assert!(emitted.contains(&spirv::Op::CompositeConstruct));
        assert!(emitted.contains(&spirv::Op::CompositeExtract));
        assert!(emitted.contains(&spirv::Op::CompositeInsert));
    }
}
