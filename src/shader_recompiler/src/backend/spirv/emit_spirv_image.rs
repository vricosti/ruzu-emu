// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SPIR-V image/texture emission — maps to upstream
//! `backend/spirv/emit_spirv_image.cpp`.
//!
//! Handles texture sampling, image loads, and texture queries.

use super::spirv_emit_context::SpirvEmitContext;
use crate::ir;
use crate::profile::Profile;
use rspirv::spirv::Word;

/// Port of upstream `NonUniformKind`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NonUniformKind {
    SampledImage,
    StorageImage,
    UniformTexelBuffer,
    StorageTexelBuffer,
}

/// Port of upstream `IsNonUniformSupported`.
fn is_non_uniform_supported(profile: &Profile, kind: NonUniformKind) -> bool {
    match kind {
        NonUniformKind::SampledImage => profile.support_sampled_image_array_nonuniform_indexing,
        NonUniformKind::StorageImage => profile.support_storage_image_array_nonuniform_indexing,
        NonUniformKind::UniformTexelBuffer => {
            profile.support_uniform_texel_buffer_array_nonuniform_indexing
        }
        NonUniformKind::StorageTexelBuffer => {
            profile.support_storage_texel_buffer_array_nonuniform_indexing
        }
    }
}

/// Port of upstream `DecorateNonUniform`.
fn decorate_non_uniform(ctx: &mut SpirvEmitContext, object: Word) {
    if !ctx.non_uniform_ids.insert(object) {
        return;
    }
    ctx.builder
        .decorate(object, rspirv::spirv::Decoration::NonUniform, vec![]);
}

/// Port of upstream `MarkNonUniform`.
fn mark_non_uniform(
    ctx: &mut SpirvEmitContext,
    idx: Word,
    index: ir::Value,
    kind: NonUniformKind,
) -> bool {
    if index.is_immediate() || !is_non_uniform_supported(&ctx.profile, kind) {
        return false;
    }
    decorate_non_uniform(ctx, idx);
    match kind {
        NonUniformKind::SampledImage => ctx.uses_nonuniform_sampled_image = true,
        NonUniformKind::StorageImage => ctx.uses_nonuniform_storage_image = true,
        NonUniformKind::UniformTexelBuffer => ctx.uses_nonuniform_uniform_texel_buffer = true,
        NonUniformKind::StorageTexelBuffer => ctx.uses_nonuniform_storage_texel_buffer = true,
    }
    true
}

fn emit_is_scaled(ctx: &mut SpirvEmitContext, index: ir::Value, member_index: u32) -> Word {
    let ir::Value::ImmU32(index) = index else {
        panic!("Non-constant texture rescaling");
    };
    let word_index = index / 32;
    let bit_mask = ctx.constant_u32(1u32 << (index % 32));
    let mask = if ctx.profile.unified_descriptor_binding {
        let pointer_type = ctx.builder.type_pointer(
            None,
            rspirv::spirv::StorageClass::PushConstant,
            ctx.u32_type,
        );
        let member = ctx.constant_u32(member_index);
        let word = ctx.constant_u32(word_index);
        let pointer = ctx
            .builder
            .access_chain(
                pointer_type,
                None,
                ctx.rescaling_push_constants,
                vec![member, word],
            )
            .unwrap();
        ctx.builder
            .load(ctx.u32_type, None, pointer, None, vec![])
            .unwrap()
    } else {
        if word_index != 0 {
            return ctx.builder.constant_false(ctx.bool_type);
        }
        let composite = ctx
            .builder
            .load(
                ctx.f32_vec4_type,
                None,
                ctx.rescaling_uniform_constant,
                None,
                vec![],
            )
            .unwrap();
        let component = ctx
            .builder
            .composite_extract(ctx.f32_type, None, composite, vec![member_index])
            .unwrap();
        ctx.builder.bitcast(ctx.u32_type, None, component).unwrap()
    };
    let tested = ctx
        .builder
        .bitwise_and(ctx.u32_type, None, mask, bit_mask)
        .unwrap();
    ctx.builder
        .i_not_equal(ctx.bool_type, None, tested, ctx.const_zero_u32)
        .unwrap()
}

/// Port of upstream `EmitIsTextureScaled`.
pub fn emit_is_texture_scaled(ctx: &mut SpirvEmitContext, index: ir::Value) -> Word {
    emit_is_scaled(ctx, index, 0)
}

/// Port of upstream `EmitIsImageScaled`.
pub fn emit_is_image_scaled(ctx: &mut SpirvEmitContext, index: ir::Value) -> Word {
    emit_is_scaled(ctx, index, 1)
}

/// Emit ImageSampleImplicitLod (TEX/TEXS with implicit LOD).
///
/// Matches upstream `EmitImageSampleImplicitLod`.
pub fn emit_image_sample_implicit_lod(
    ctx: &mut SpirvEmitContext,
    _handle: Word,
    _coords: Word,
) -> Word {
    log::trace!("SPIR-V: emit_image_sample_implicit_lod");
    ctx.builder.undef(ctx.f32_vec4_type, None)
}

/// Emit ImageSampleExplicitLod (TXL).
pub fn emit_image_sample_explicit_lod(
    ctx: &mut SpirvEmitContext,
    _handle: Word,
    _coords: Word,
    _lod: Word,
) -> Word {
    log::trace!("SPIR-V: emit_image_sample_explicit_lod");
    ctx.builder.undef(ctx.f32_vec4_type, None)
}

/// Emit ImageSampleDrefImplicitLod (shadow TEX).
pub fn emit_image_sample_dref_implicit_lod(
    ctx: &mut SpirvEmitContext,
    _handle: Word,
    _coords: Word,
    _dref: Word,
) -> Word {
    log::trace!("SPIR-V: emit_image_sample_dref_implicit_lod");
    ctx.builder.undef(ctx.f32_type, None)
}

/// Emit ImageSampleDrefExplicitLod (shadow TXL).
pub fn emit_image_sample_dref_explicit_lod(
    ctx: &mut SpirvEmitContext,
    _handle: Word,
    _coords: Word,
    _dref: Word,
    _lod: Word,
) -> Word {
    log::trace!("SPIR-V: emit_image_sample_dref_explicit_lod");
    ctx.builder.undef(ctx.f32_type, None)
}

/// Emit ImageFetch (TLD — texel fetch).
pub fn emit_image_fetch(
    ctx: &mut SpirvEmitContext,
    _handle: Word,
    _coords: Word,
    _lod: Word,
) -> Word {
    log::trace!("SPIR-V: emit_image_fetch");
    ctx.builder.undef(ctx.f32_vec4_type, None)
}

/// Emit ImageGather (TLD4).
pub fn emit_image_gather(
    ctx: &mut SpirvEmitContext,
    _handle: Word,
    _coords: Word,
    _component: Word,
) -> Word {
    log::trace!("SPIR-V: emit_image_gather");
    ctx.builder.undef(ctx.f32_vec4_type, None)
}

/// Emit ImageGatherDref (TLD4 with depth comparison).
pub fn emit_image_gather_dref(
    ctx: &mut SpirvEmitContext,
    _handle: Word,
    _coords: Word,
    _dref: Word,
) -> Word {
    log::trace!("SPIR-V: emit_image_gather_dref");
    ctx.builder.undef(ctx.f32_vec4_type, None)
}

/// Emit ImageQueryDimensions (TXQ).
pub fn emit_image_query_dimensions(ctx: &mut SpirvEmitContext, _handle: Word, _lod: Word) -> Word {
    log::trace!("SPIR-V: emit_image_query_dimensions");
    ctx.builder.undef(ctx.u32_vec4_type, None)
}

// ── IR-instruction dispatching helpers (called from spirv_emit_context) ───

use crate::ir::program::Program;
use crate::ir::types::TextureInstInfo;
use crate::ir::value::Value;
use crate::ir::Opcode;
use rspirv::dr::Operand;
use rspirv::spirv;

struct ImageOperands {
    mask: spirv::ImageOperands,
    operands: Vec<Operand>,
}

impl Default for ImageOperands {
    fn default() -> Self {
        Self {
            mask: spirv::ImageOperands::NONE,
            operands: Vec::new(),
        }
    }
}

impl ImageOperands {
    fn for_sample(
        ctx: &mut SpirvEmitContext,
        program: &Program,
        has_bias: bool,
        has_lod: bool,
        has_lod_clamp: bool,
        lod: Word,
        offset: Value,
    ) -> Self {
        let mut operands = Self::default();
        if has_bias {
            let bias = if has_lod_clamp {
                ctx.builder
                    .composite_extract(ctx.f32_type, None, lod, vec![0])
                    .unwrap()
            } else {
                lod
            };
            operands.add(spirv::ImageOperands::BIAS, bias);
        }
        if has_lod {
            let lod_value = if has_lod_clamp {
                ctx.builder
                    .composite_extract(ctx.f32_type, None, lod, vec![0])
                    .unwrap()
            } else {
                lod
            };
            operands.add(spirv::ImageOperands::LOD, lod_value);
        }
        operands.add_offset(ctx, program, offset, false);
        if has_lod_clamp {
            let lod_clamp = if has_bias {
                ctx.builder
                    .composite_extract(ctx.f32_type, None, lod, vec![1])
                    .unwrap()
            } else {
                lod
            };
            operands.add(spirv::ImageOperands::MIN_LOD, lod_clamp);
        }
        operands
    }

    fn add_offset(
        &mut self,
        ctx: &mut SpirvEmitContext,
        program: &Program,
        offset: Value,
        runtime_offset_allowed: bool,
    ) {
        let offset = resolve_ir_value(program, offset);
        if matches!(offset, Value::Void) {
            return;
        }
        if let Some(components) = immediate_offset_components(program, offset) {
            let offset_id = if components.len() == 1 {
                ctx.constant_i32(components[0] as i32)
            } else {
                let component_ids = components
                    .iter()
                    .map(|&value| ctx.constant_i32(value as i32))
                    .collect::<Vec<_>>();
                let offset_type = ctx
                    .builder
                    .type_vector(ctx.i32_type, components.len() as u32);
                ctx.builder.constant_composite(offset_type, component_ids)
            };
            self.add(spirv::ImageOperands::CONST_OFFSET, offset_id);
        } else if runtime_offset_allowed {
            let offset_id = ctx.resolve_value(&offset);
            self.add(spirv::ImageOperands::OFFSET, offset_id);
        }
    }

    /// Port of upstream `ImageOperands(ctx, offset, offset2)` used by TLD4.
    fn for_gather(
        ctx: &mut SpirvEmitContext,
        program: &Program,
        offset: Value,
        offset2: Value,
    ) -> Self {
        let offset = resolve_ir_value(program, offset);
        let offset2 = resolve_ir_value(program, offset2);
        let mut operands = Self::default();
        if matches!(offset2, Value::Void) {
            operands.add_offset(ctx, program, offset, true);
            return operands;
        }

        let (Value::Inst(offset_ref), Value::Inst(offset2_ref)) = (offset, offset2) else {
            panic!("SPIR-V: invalid PTP arguments");
        };
        let offset_inst = program.block(offset_ref.block).inst(offset_ref.inst);
        let offset2_inst = program.block(offset2_ref.block).inst(offset2_ref.inst);
        if !offset_inst
            .args
            .iter()
            .all(|&arg| resolve_ir_value(program, arg).is_immediate())
            || !offset2_inst
                .args
                .iter()
                .all(|&arg| resolve_ir_value(program, arg).is_immediate())
        {
            log::warn!("SPIR-V: not all arguments in PTP are immediate, ignoring");
            return operands;
        }
        if offset_inst.opcode != Opcode::CompositeConstructU32x4
            || offset2_inst.opcode != offset_inst.opcode
        {
            panic!("SPIR-V: invalid PTP arguments");
        }
        let first = immediate_offset_components(program, Value::Inst(offset_ref))
            .expect("validated PTP immediate vector");
        let second = immediate_offset_components(program, Value::Inst(offset2_ref))
            .expect("validated PTP immediate vector");

        let pairs = [
            [first[0], first[1]],
            [first[2], first[3]],
            [second[0], second[1]],
            [second[2], second[3]],
        ];
        let i32_vec2_type = ctx.builder.type_vector(ctx.i32_type, 2);
        let offsets = pairs
            .into_iter()
            .map(|pair| {
                let x = ctx.constant_i32(pair[0] as i32);
                let y = ctx.constant_i32(pair[1] as i32);
                ctx.builder.constant_composite(i32_vec2_type, vec![x, y])
            })
            .collect::<Vec<_>>();
        let count = ctx.constant_u32(4);
        let array_type = ctx.builder.type_array(i32_vec2_type, count);
        let offsets = ctx.builder.constant_composite(array_type, offsets);
        operands.add(spirv::ImageOperands::CONST_OFFSETS, offsets);
        operands
    }

    fn for_gradient(
        ctx: &mut SpirvEmitContext,
        program: &Program,
        info: TextureInstInfo,
        derivatives: Word,
        second_derivatives_or_offset: Value,
        lod_clamp: Value,
    ) -> Self {
        let mut operands = Self::default();
        let (derivatives_x, derivatives_y, offset) = if info.num_derivatives == 3 {
            let second_derivatives = ctx.resolve_value(&second_derivatives_or_offset);
            let derivatives_x = [
                ctx.builder
                    .composite_extract(ctx.f32_type, None, derivatives, vec![0])
                    .unwrap(),
                ctx.builder
                    .composite_extract(ctx.f32_type, None, derivatives, vec![2])
                    .unwrap(),
                ctx.builder
                    .composite_extract(ctx.f32_type, None, second_derivatives, vec![0])
                    .unwrap(),
            ];
            let derivatives_y = [
                ctx.builder
                    .composite_extract(ctx.f32_type, None, derivatives, vec![1])
                    .unwrap(),
                ctx.builder
                    .composite_extract(ctx.f32_type, None, derivatives, vec![3])
                    .unwrap(),
                ctx.builder
                    .composite_extract(ctx.f32_type, None, second_derivatives, vec![1])
                    .unwrap(),
            ];
            let derivatives_x = ctx
                .builder
                .composite_construct(ctx.f32_vec3_type, None, derivatives_x)
                .unwrap();
            let derivatives_y = ctx
                .builder
                .composite_construct(ctx.f32_vec3_type, None, derivatives_y)
                .unwrap();
            (derivatives_x, derivatives_y, Value::Void)
        } else {
            let count = info.num_derivatives as usize;
            assert!((1..=2).contains(&count), "SPIR-V: invalid derivative count");
            let mut derivatives_x = Vec::with_capacity(count);
            let mut derivatives_y = Vec::with_capacity(count);
            for index in 0..count as u32 {
                derivatives_x.push(
                    ctx.builder
                        .composite_extract(ctx.f32_type, None, derivatives, vec![index * 2])
                        .unwrap(),
                );
                derivatives_y.push(
                    ctx.builder
                        .composite_extract(ctx.f32_type, None, derivatives, vec![index * 2 + 1])
                        .unwrap(),
                );
            }
            let (derivatives_x, derivatives_y) = if count == 1 {
                (derivatives_x[0], derivatives_y[0])
            } else {
                (
                    ctx.builder
                        .composite_construct(ctx.f32_vec2_type, None, derivatives_x)
                        .unwrap(),
                    ctx.builder
                        .composite_construct(ctx.f32_vec2_type, None, derivatives_y)
                        .unwrap(),
                )
            };
            (derivatives_x, derivatives_y, second_derivatives_or_offset)
        };
        operands.add_pair(spirv::ImageOperands::GRAD, derivatives_x, derivatives_y);
        operands.add_offset(ctx, program, offset, false);
        if info.has_lod_clamp {
            operands.add(spirv::ImageOperands::MIN_LOD, ctx.resolve_value(&lod_clamp));
        }
        operands
    }

    fn add(&mut self, mask: spirv::ImageOperands, value: Word) {
        self.mask |= mask;
        self.operands.push(Operand::IdRef(value));
    }

    fn add_pair(&mut self, mask: spirv::ImageOperands, first: Word, second: Word) {
        self.mask |= mask;
        self.operands.push(Operand::IdRef(first));
        self.operands.push(Operand::IdRef(second));
    }

    fn mask_optional(&self) -> Option<spirv::ImageOperands> {
        (!self.mask.is_empty()).then_some(self.mask)
    }
}

fn resolve_ir_value(program: &Program, mut value: Value) -> Value {
    while let Value::Inst(inst_ref) = value {
        let inst = program.block(inst_ref.block).inst(inst_ref.inst);
        if inst.opcode != Opcode::Identity || inst.args.is_empty() {
            break;
        }
        value = inst.args[0];
    }
    value
}

fn immediate_offset_components(program: &Program, offset: Value) -> Option<Vec<u32>> {
    match resolve_ir_value(program, offset) {
        Value::ImmU32(value) => Some(vec![value]),
        Value::Inst(inst_ref) => {
            let inst = program.block(inst_ref.block).inst(inst_ref.inst);
            let component_count = match inst.opcode {
                Opcode::CompositeConstructU32x2 => 2,
                Opcode::CompositeConstructU32x3 => 3,
                Opcode::CompositeConstructU32x4 => 4,
                _ => return None,
            };
            inst.args
                .iter()
                .take(component_count)
                .map(|&arg| match resolve_ir_value(program, arg) {
                    Value::ImmU32(value) => Some(value),
                    _ => None,
                })
                .collect()
        }
        _ => None,
    }
}

/// Port of upstream `AddOffsetToCoordinates`.
fn add_offset_to_coordinates(
    ctx: &mut SpirvEmitContext,
    info: TextureInstInfo,
    coords: Word,
    offset: &Value,
) -> Word {
    if offset.is_void() {
        return coords;
    }

    let texture_type = crate::shader_info::TextureType::from_u8(info.texture_type);
    let mut offset = ctx.resolve_value(offset);
    let result_type = match texture_type {
        crate::shader_info::TextureType::Buffer | crate::shader_info::TextureType::Color1D => {
            ctx.u32_type
        }
        crate::shader_info::TextureType::ColorArray1D => {
            offset = ctx
                .builder
                .composite_construct(ctx.u32_vec2_type, None, vec![offset, ctx.const_zero_u32])
                .unwrap();
            ctx.u32_vec2_type
        }
        crate::shader_info::TextureType::Color2D | crate::shader_info::TextureType::Color2DRect => {
            ctx.u32_vec2_type
        }
        crate::shader_info::TextureType::ColorArray2D => {
            let x = ctx
                .builder
                .composite_extract(ctx.u32_type, None, offset, vec![0])
                .unwrap();
            let y = ctx
                .builder
                .composite_extract(ctx.u32_type, None, offset, vec![1])
                .unwrap();
            offset = ctx
                .builder
                .composite_construct(ctx.u32_vec3_type, None, vec![x, y, ctx.const_zero_u32])
                .unwrap();
            ctx.u32_vec3_type
        }
        crate::shader_info::TextureType::Color3D => ctx.u32_vec3_type,
        crate::shader_info::TextureType::ColorCube
        | crate::shader_info::TextureType::ColorArrayCube => return coords,
    };
    ctx.builder
        .i_add(result_type, None, coords, offset)
        .unwrap()
}

fn decorate_sample(ctx: &mut SpirvEmitContext, info: TextureInstInfo, sample: Word) -> Word {
    if info.relaxed_precision {
        ctx.builder
            .decorate(sample, spirv::Decoration::RelaxedPrecision, vec![]);
    }
    sample
}

/// Port of upstream `ImageGatherSubpixelOffset`.
fn image_gather_subpixel_offset(
    ctx: &mut SpirvEmitContext,
    info: TextureInstInfo,
    image: Word,
    coords: Word,
) -> Word {
    let dimension = match crate::shader_info::TextureType::from_u8(info.texture_type) {
        crate::shader_info::TextureType::Color2D | crate::shader_info::TextureType::Color2DRect => {
            2
        }
        crate::shader_info::TextureType::ColorArray2D
        | crate::shader_info::TextureType::ColorCube => 3,
        _ => return coords,
    };
    let (u32_type, f32_type) = if dimension == 2 {
        (ctx.u32_vec2_type, ctx.f32_vec2_type)
    } else {
        (ctx.u32_vec3_type, ctx.f32_vec3_type)
    };
    let image_size = ctx
        .builder
        .image_query_size_lod(u32_type, None, image, ctx.const_zero_u32)
        .unwrap();
    let image_size = ctx
        .builder
        .convert_u_to_f(f32_type, None, image_size)
        .unwrap();
    let nudge = ctx.constant_f32(2.0_f32.powi(-9));
    let offset = if dimension == 2 {
        ctx.builder
            .composite_construct(f32_type, None, vec![nudge, nudge])
            .unwrap()
    } else {
        ctx.builder
            .composite_construct(f32_type, None, vec![nudge, nudge, ctx.const_zero_f32])
            .unwrap()
    };
    let offset = ctx
        .builder
        .f_div(f32_type, None, offset, image_size)
        .unwrap();
    ctx.builder.f_add(f32_type, None, coords, offset).unwrap()
}

/// Port of upstream `Texture`: load a combined image sampler, including
/// descriptor-array indexing when `count > 1`.
fn texture(ctx: &mut SpirvEmitContext, info: TextureInstInfo, index: Value) -> Word {
    let def = *ctx
        .textures
        .get(info.descriptor_index as usize)
        .expect("SPIR-V: missing texture descriptor");
    if def.count > 1 {
        let idx = ctx.resolve_value(&index);
        let non_uniform = mark_non_uniform(ctx, idx, index, NonUniformKind::SampledImage);
        let pointer = ctx
            .builder
            .access_chain(def.pointer_type, None, def.id, vec![idx])
            .unwrap();
        let object = ctx
            .builder
            .load(def.sampled_type, None, pointer, None, vec![])
            .unwrap();
        if non_uniform {
            decorate_non_uniform(ctx, pointer);
            decorate_non_uniform(ctx, object);
        }
        object
    } else {
        ctx.builder
            .load(def.sampled_type, None, def.id, None, vec![])
            .unwrap()
    }
}

fn is_texture_integer(ctx: &SpirvEmitContext, info: TextureInstInfo) -> bool {
    crate::shader_info::TextureType::from_u8(info.texture_type)
        != crate::shader_info::TextureType::Buffer
        && ctx
            .textures
            .get(info.descriptor_index as usize)
            .expect("SPIR-V: missing texture descriptor")
            .is_integer
}

/// Port of upstream `TextureImage`: load a texel buffer or extract the image
/// from a combined sampler.
fn texture_image(ctx: &mut SpirvEmitContext, info: TextureInstInfo, index: Value) -> Word {
    if crate::shader_info::TextureType::from_u8(info.texture_type)
        == crate::shader_info::TextureType::Buffer
    {
        let def = *ctx
            .texture_buffers
            .get(info.descriptor_index as usize)
            .expect("SPIR-V: missing texture-buffer descriptor");
        if def.count > 1 {
            let idx = ctx.resolve_value(&index);
            let non_uniform = mark_non_uniform(ctx, idx, index, NonUniformKind::UniformTexelBuffer);
            let ptr = ctx
                .builder
                .access_chain(ctx.image_buffer_type, None, def.id, vec![idx])
                .unwrap();
            let object = ctx
                .builder
                .load(ctx.image_buffer_type, None, ptr, None, vec![])
                .unwrap();
            if non_uniform {
                decorate_non_uniform(ctx, ptr);
                decorate_non_uniform(ctx, object);
            }
            return object;
        }
        return ctx
            .builder
            .load(ctx.image_buffer_type, None, def.id, None, vec![])
            .unwrap();
    }

    let def = *ctx
        .textures
        .get(info.descriptor_index as usize)
        .expect("SPIR-V: missing texture descriptor");
    if def.count > 1 {
        let idx = ctx.resolve_value(&index);
        let non_uniform = mark_non_uniform(ctx, idx, index, NonUniformKind::SampledImage);
        let ptr = ctx
            .builder
            .access_chain(def.pointer_type, None, def.id, vec![idx])
            .unwrap();
        let object = ctx
            .builder
            .load(def.sampled_type, None, ptr, None, vec![])
            .unwrap();
        let image = ctx.builder.image(def.image_type, None, object).unwrap();
        if non_uniform {
            decorate_non_uniform(ctx, ptr);
            decorate_non_uniform(ctx, object);
            decorate_non_uniform(ctx, image);
        }
        return image;
    }
    let sampled_image = ctx
        .builder
        .load(def.sampled_type, None, def.id, None, vec![])
        .unwrap();
    ctx.builder
        .image(def.image_type, None, sampled_image)
        .unwrap()
}

/// Port of upstream `Image`: load a storage image and preserve whether its
/// sampled component type is integer.
fn image(ctx: &mut SpirvEmitContext, info: TextureInstInfo, index: Value) -> (Word, bool) {
    let texture_type = crate::shader_info::TextureType::from_u8(info.texture_type);
    let is_buffer = texture_type == crate::shader_info::TextureType::Buffer;
    let (id, image_type, pointer_type, count, is_integer) = if is_buffer {
        let def = *ctx
            .image_buffers
            .get(info.descriptor_index as usize)
            .expect("SPIR-V: missing image-buffer descriptor");
        (
            def.id,
            def.image_type,
            def.pointer_type,
            def.count,
            def.is_integer,
        )
    } else {
        let def = *ctx
            .images
            .get(info.descriptor_index as usize)
            .expect("SPIR-V: missing image descriptor");
        (
            def.id,
            def.image_type,
            def.pointer_type,
            def.count,
            def.is_integer,
        )
    };
    if count > 1 {
        let kind = if is_buffer {
            NonUniformKind::StorageTexelBuffer
        } else {
            NonUniformKind::StorageImage
        };
        let idx = ctx.resolve_value(&index);
        let non_uniform = mark_non_uniform(ctx, idx, index, kind);
        let ptr = ctx
            .builder
            .access_chain(pointer_type, None, id, vec![idx])
            .unwrap();
        let image = ctx
            .builder
            .load(image_type, None, ptr, None, vec![])
            .unwrap();
        if non_uniform {
            decorate_non_uniform(ctx, ptr);
            decorate_non_uniform(ctx, image);
        }
        return (image, is_integer);
    }
    let image = ctx
        .builder
        .load(image_type, None, id, None, vec![])
        .unwrap();
    (image, is_integer)
}

fn image_read(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    result_type: Word,
    image: Word,
    coords: Word,
) -> Word {
    let sample = if let Some(sparse) = inst.get_associated_pseudo(Opcode::GetSparseFromOp) {
        let struct_type = ctx.builder.type_struct(vec![ctx.u32_type, result_type]);
        let sparse_result = ctx
            .builder
            .image_sparse_read(struct_type, None, image, coords, None, vec![])
            .unwrap();
        let resident_code = ctx
            .builder
            .composite_extract(ctx.u32_type, None, sparse_result, vec![0])
            .unwrap();
        let resident = ctx
            .builder
            .image_sparse_texels_resident(ctx.bool_type, None, resident_code)
            .unwrap();
        ctx.set_value(sparse.block, sparse.inst, resident);
        sparse_result
    } else {
        ctx.builder
            .image_read(result_type, None, image, coords, None, vec![])
            .unwrap()
    };
    let sample = decorate_sample(ctx, TextureInstInfo::from_u32(inst.flags), sample);
    if inst
        .get_associated_pseudo(Opcode::GetSparseFromOp)
        .is_some()
    {
        ctx.builder
            .composite_extract(result_type, None, sample, vec![1])
            .unwrap()
    } else {
        sample
    }
}

/// Dispatch ImageSampleImplicitLod / ImageSampleExplicitLod IR instructions.
pub fn emit_image_sample(
    ctx: &mut SpirvEmitContext,
    program: &Program,
    inst: &ir::Inst,
    block_idx: u32,
    inst_idx: u32,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    let coord = ctx.resolve_value(inst.arg(1));
    let is_integer = is_texture_integer(ctx, info);
    let result_type = if is_integer {
        ctx.u32_vec4_type
    } else {
        ctx.f32_vec4_type
    };

    let sampled_image = texture(ctx, info, *inst.arg(0));
    let explicit_lod = inst.opcode == Opcode::ImageSampleExplicitLod;
    let mut id = if explicit_lod {
        let lod = ctx.resolve_value(inst.arg(2));
        let operands =
            ImageOperands::for_sample(ctx, program, false, true, false, lod, *inst.arg(3));
        ctx.builder
            .image_sample_explicit_lod(
                result_type,
                None,
                sampled_image,
                coord,
                operands.mask,
                operands.operands,
            )
            .unwrap()
    } else if ctx.stage == crate::stage::Stage::Fragment {
        let bias_lc = if info.has_bias || info.has_lod_clamp {
            ctx.resolve_value(inst.arg(2))
        } else {
            0
        };
        let operands = ImageOperands::for_sample(
            ctx,
            program,
            info.has_bias,
            false,
            info.has_lod_clamp,
            bias_lc,
            *inst.arg(3),
        );
        ctx.builder
            .image_sample_implicit_lod(
                result_type,
                None,
                sampled_image,
                coord,
                operands.mask_optional(),
                operands.operands,
            )
            .unwrap()
    } else {
        let lod = ctx.constant_f32(0.0);
        let operands = ImageOperands::for_sample(
            ctx,
            program,
            false,
            true,
            info.has_lod_clamp,
            lod,
            *inst.arg(3),
        );
        ctx.builder
            .image_sample_explicit_lod(
                result_type,
                None,
                sampled_image,
                coord,
                operands.mask,
                operands.operands,
            )
            .unwrap()
    };

    #[cfg(target_os = "android")]
    if explicit_lod && !is_integer && *common::settings::values().fix_bloom_effects.get_value() {
        let factor = ctx.constant_f32(0.98);
        id = ctx
            .builder
            .vector_times_scalar(ctx.f32_vec4_type, None, id, factor)
            .unwrap();
    }
    if is_integer {
        id = ctx.builder.bitcast(ctx.f32_vec4_type, None, id).unwrap();
    }
    let id = decorate_sample(ctx, info, id);
    ctx.set_value(block_idx, inst_idx, id);
}

/// Dispatch ImageSampleDrefImplicitLod / ImageSampleDrefExplicitLod IR instructions.
pub fn emit_image_sample_dref(
    ctx: &mut SpirvEmitContext,
    program: &Program,
    inst: &ir::Inst,
    block_idx: u32,
    inst_idx: u32,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    let coord = ctx.resolve_value(inst.arg(1));
    let dref = ctx.resolve_value(inst.arg(2));

    let sampled_image = texture(ctx, info, *inst.arg(0));
    let id = if inst.opcode == Opcode::ImageSampleDrefExplicitLod {
        let lod = ctx.resolve_value(inst.arg(3));
        let operands =
            ImageOperands::for_sample(ctx, program, false, true, false, lod, *inst.arg(4));
        ctx.builder
            .image_sample_dref_explicit_lod(
                ctx.f32_type,
                None,
                sampled_image,
                coord,
                dref,
                operands.mask,
                operands.operands,
            )
            .unwrap()
    } else if ctx.stage == crate::stage::Stage::Fragment {
        let bias_lc = if info.has_bias || info.has_lod_clamp {
            ctx.resolve_value(inst.arg(3))
        } else {
            0
        };
        let operands = ImageOperands::for_sample(
            ctx,
            program,
            info.has_bias,
            false,
            info.has_lod_clamp,
            bias_lc,
            *inst.arg(4),
        );
        ctx.builder
            .image_sample_dref_implicit_lod(
                ctx.f32_type,
                None,
                sampled_image,
                coord,
                dref,
                operands.mask_optional(),
                operands.operands,
            )
            .unwrap()
    } else {
        let lod = ctx.constant_f32(0.0);
        let operands =
            ImageOperands::for_sample(ctx, program, false, true, false, lod, *inst.arg(4));
        ctx.builder
            .image_sample_dref_explicit_lod(
                ctx.f32_type,
                None,
                sampled_image,
                coord,
                dref,
                operands.mask,
                operands.operands,
            )
            .unwrap()
    };

    let id = decorate_sample(ctx, info, id);
    ctx.set_value(block_idx, inst_idx, id);
}

/// Dispatch ImageFetch IR instructions.
pub fn emit_image_fetch_inst(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    block_idx: u32,
    inst_idx: u32,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    let is_integer = is_texture_integer(ctx, info);
    let result_type = if is_integer {
        ctx.u32_vec4_type
    } else {
        ctx.f32_vec4_type
    };
    let coords = ctx.resolve_value(inst.arg(1));
    let coords = add_offset_to_coordinates(ctx, info, coords, inst.arg(2));

    let image = texture_image(ctx, info, *inst.arg(0));
    let is_buffer = crate::shader_info::TextureType::from_u8(info.texture_type)
        == crate::shader_info::TextureType::Buffer;
    let mut lod = (!is_buffer && !inst.arg(3).is_void()).then(|| ctx.resolve_value(inst.arg(3)));
    let sample = (!inst.arg(4).is_void()).then(|| ctx.resolve_value(inst.arg(4)));
    if sample.is_some() {
        lod = None;
    }
    let mut operand_mask = spirv::ImageOperands::NONE;
    let mut operand_ids = Vec::with_capacity(2);
    if let Some(lod) = lod {
        operand_mask |= spirv::ImageOperands::LOD;
        operand_ids.push(Operand::IdRef(lod));
    }
    if let Some(sample) = sample {
        operand_mask |= spirv::ImageOperands::SAMPLE;
        operand_ids.push(Operand::IdRef(sample));
    }
    let id = ctx
        .builder
        .image_fetch(
            result_type,
            None,
            image,
            coords,
            (!operand_mask.is_empty()).then_some(operand_mask),
            operand_ids,
        )
        .unwrap();
    let id = if is_integer {
        ctx.builder.bitcast(ctx.f32_vec4_type, None, id).unwrap()
    } else {
        id
    };

    ctx.set_value(block_idx, inst_idx, id);
}

/// Dispatch ImageQueryDimensions IR instructions.
pub fn emit_image_query(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    block_idx: u32,
    inst_idx: u32,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    let image = texture_image(ctx, info, *inst.arg(0));
    let texture_type = crate::shader_info::TextureType::from_u8(info.texture_type);
    let is_buffer = texture_type == crate::shader_info::TextureType::Buffer;
    let lod = if inst.args.len() > 1 {
        ctx.resolve_value(inst.arg(1))
    } else {
        ctx.const_zero_u32
    };
    let skip_mips = inst.args.get(2).map(Value::imm_u1).unwrap_or(false);
    let mips = if skip_mips {
        ctx.const_zero_u32
    } else {
        ctx.builder
            .image_query_levels(ctx.u32_type, None, image)
            .unwrap()
    };
    let is_msaa = !is_buffer
        && ctx
            .textures
            .get(info.descriptor_index as usize)
            .is_some_and(|def| def.is_multisample);
    let uses_lod = !is_msaa && !is_buffer;
    let mut query = |result_type| {
        if uses_lod {
            ctx.builder
                .image_query_size_lod(result_type, None, image, lod)
                .unwrap()
        } else {
            ctx.builder
                .image_query_size(result_type, None, image)
                .unwrap()
        }
    };
    let zero = ctx.const_zero_u32;
    let constituents = match texture_type {
        crate::shader_info::TextureType::Color1D | crate::shader_info::TextureType::Buffer => {
            vec![query(ctx.u32_type), zero, zero, mips]
        }
        crate::shader_info::TextureType::ColorArray1D
        | crate::shader_info::TextureType::Color2D
        | crate::shader_info::TextureType::ColorCube
        | crate::shader_info::TextureType::Color2DRect => {
            vec![query(ctx.u32_vec2_type), zero, mips]
        }
        crate::shader_info::TextureType::ColorArray2D
        | crate::shader_info::TextureType::Color3D
        | crate::shader_info::TextureType::ColorArrayCube => {
            vec![query(ctx.u32_vec3_type), mips]
        }
    };
    let id = ctx
        .builder
        .composite_construct(ctx.u32_vec4_type, None, constituents)
        .unwrap();

    ctx.set_value(block_idx, inst_idx, id);
}

/// Port of upstream `EmitImageQueryLod` (TMML).
pub fn emit_image_query_lod(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    block_idx: u32,
    inst_idx: u32,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    let coords = ctx.resolve_value(inst.arg(1));
    let sampler = texture(ctx, info, *inst.arg(0));
    let lod = ctx
        .builder
        .image_query_lod(ctx.f32_vec2_type, None, sampler, coords)
        .unwrap();
    let id = ctx
        .builder
        .composite_construct(
            ctx.f32_vec4_type,
            None,
            vec![lod, ctx.const_zero_f32, ctx.const_zero_f32],
        )
        .unwrap();
    ctx.set_value(block_idx, inst_idx, id);
}

/// Port of upstream `EmitImageGradient` (TXD).
pub fn emit_image_gradient_inst(
    ctx: &mut SpirvEmitContext,
    program: &Program,
    inst: &ir::Inst,
    block_idx: u32,
    inst_idx: u32,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    let is_integer = is_texture_integer(ctx, info);
    let sample_type = if is_integer {
        ctx.u32_vec4_type
    } else {
        ctx.f32_vec4_type
    };
    let coords = ctx.resolve_value(inst.arg(1));
    let derivatives = ctx.resolve_value(inst.arg(2));
    let operands =
        ImageOperands::for_gradient(ctx, program, info, derivatives, *inst.arg(3), *inst.arg(4));
    let sampler = texture(ctx, info, *inst.arg(0));
    let sparse = inst.get_associated_pseudo(Opcode::GetSparseFromOp);
    let id = if let Some(sparse_ref) = sparse {
        let result_type = ctx.builder.type_struct(vec![ctx.u32_type, sample_type]);
        let sample = ctx
            .builder
            .image_sparse_sample_explicit_lod(
                result_type,
                None,
                sampler,
                coords,
                operands.mask,
                operands.operands,
            )
            .unwrap();
        let sample = decorate_sample(ctx, info, sample);
        let resident_code = ctx
            .builder
            .composite_extract(ctx.u32_type, None, sample, vec![0])
            .unwrap();
        let resident = ctx
            .builder
            .image_sparse_texels_resident(ctx.bool_type, None, resident_code)
            .unwrap();
        ctx.set_value(sparse_ref.block, sparse_ref.inst, resident);
        ctx.builder
            .composite_extract(sample_type, None, sample, vec![1])
            .unwrap()
    } else {
        let sample = ctx
            .builder
            .image_sample_explicit_lod(
                sample_type,
                None,
                sampler,
                coords,
                operands.mask,
                operands.operands,
            )
            .unwrap();
        decorate_sample(ctx, info, sample)
    };
    let id = if is_integer {
        ctx.builder.bitcast(ctx.f32_vec4_type, None, id).unwrap()
    } else {
        id
    };
    ctx.set_value(block_idx, inst_idx, id);
}

/// Dispatch ImageGather / ImageGatherDref IR instructions.
pub fn emit_image_gather_inst(
    ctx: &mut SpirvEmitContext,
    program: &Program,
    inst: &ir::Inst,
    block_idx: u32,
    inst_idx: u32,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    let is_integer = is_texture_integer(ctx, info);
    let sample_type = if is_integer {
        ctx.u32_vec4_type
    } else {
        ctx.f32_vec4_type
    };
    let mut coord = ctx.resolve_value(inst.arg(1));

    let sampled_image = texture(ctx, info, *inst.arg(0));
    let operands = ImageOperands::for_gather(ctx, program, *inst.arg(2), *inst.arg(3));
    if ctx.profile.need_gather_subpixel_offset {
        let image = texture_image(ctx, info, *inst.arg(0));
        coord = image_gather_subpixel_offset(ctx, info, image, coord);
    }
    let dref = (inst.opcode == Opcode::ImageGatherDref).then(|| ctx.resolve_value(inst.arg(4)));
    let component = ctx
        .builder
        .constant_bit32(ctx.u32_type, info.gather_component as u32);
    let sparse = inst.get_associated_pseudo(Opcode::GetSparseFromOp);
    let id = if let Some(sparse_ref) = sparse {
        let result_type = ctx.builder.type_struct(vec![ctx.u32_type, sample_type]);
        let sample = if let Some(dref) = dref {
            ctx.builder
                .image_sparse_dref_gather(
                    result_type,
                    None,
                    sampled_image,
                    coord,
                    dref,
                    operands.mask_optional(),
                    operands.operands,
                )
                .unwrap()
        } else {
            ctx.builder
                .image_sparse_gather(
                    result_type,
                    None,
                    sampled_image,
                    coord,
                    component,
                    operands.mask_optional(),
                    operands.operands,
                )
                .unwrap()
        };
        let sample = decorate_sample(ctx, info, sample);
        let resident_code = ctx
            .builder
            .composite_extract(ctx.u32_type, None, sample, vec![0])
            .unwrap();
        let resident = ctx
            .builder
            .image_sparse_texels_resident(ctx.bool_type, None, resident_code)
            .unwrap();
        ctx.set_value(sparse_ref.block, sparse_ref.inst, resident);
        ctx.builder
            .composite_extract(sample_type, None, sample, vec![1])
            .unwrap()
    } else if let Some(dref) = dref {
        let sample = ctx
            .builder
            .image_dref_gather(
                sample_type,
                None,
                sampled_image,
                coord,
                dref,
                operands.mask_optional(),
                operands.operands,
            )
            .unwrap();
        decorate_sample(ctx, info, sample)
    } else {
        let sample = ctx
            .builder
            .image_gather(
                sample_type,
                None,
                sampled_image,
                coord,
                component,
                operands.mask_optional(),
                operands.operands,
            )
            .unwrap();
        decorate_sample(ctx, info, sample)
    };
    let id = if is_integer {
        ctx.builder.bitcast(ctx.f32_vec4_type, None, id).unwrap()
    } else {
        id
    };

    ctx.set_value(block_idx, inst_idx, id);
}

/// Port of upstream `EmitImageRead`.
pub fn emit_image_read_inst(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    block_idx: u32,
    inst_idx: u32,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    if crate::shader_info::ImageFormat::from_u8(info.image_format)
        == crate::shader_info::ImageFormat::Typeless
        && !ctx.profile.support_typeless_image_loads
    {
        log::warn!("SPIR-V: typeless image read not supported by host");
        let color = ctx.builder.constant_null(ctx.u32_vec4_type);
        ctx.set_value(block_idx, inst_idx, color);
        return;
    }
    let coords = ctx.resolve_value(inst.arg(1));
    let (image, is_integer) = image(ctx, info, *inst.arg(0));
    let result_type = if is_integer {
        ctx.u32_vec4_type
    } else {
        ctx.f32_vec4_type
    };
    let mut color = image_read(ctx, inst, result_type, image, coords);
    if !is_integer {
        color = ctx.builder.bitcast(ctx.u32_vec4_type, None, color).unwrap();
    }
    ctx.set_value(block_idx, inst_idx, color);
}

/// Port of upstream `EmitImageWrite`.
pub fn emit_image_write_inst(ctx: &mut SpirvEmitContext, inst: &ir::Inst) {
    let info = TextureInstInfo::from_u32(inst.flags);
    let coords = ctx.resolve_value(inst.arg(1));
    let mut color = ctx.resolve_value(inst.arg(2));
    let (image, is_integer) = image(ctx, info, *inst.arg(0));
    if !is_integer {
        color = ctx.builder.bitcast(ctx.f32_vec4_type, None, color).unwrap();
    }
    ctx.builder
        .image_write(image, coords, color, None, vec![])
        .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::basic_block::Block;
    use crate::ir::instruction::Inst;
    use crate::ir::types::{ShaderStage, TextureInstInfo};
    use crate::ir::value::InstRef;
    use crate::ir::SyntaxNode;
    use crate::profile::Profile;
    use crate::runtime_info::RuntimeInfo;
    use crate::shader_info::{ImageDescriptor, ImageFormat, TextureDescriptor, TextureType};
    use rspirv::binary::Assemble;

    fn validate_with_external_tool(ctx: SpirvEmitContext, name: &str) {
        let Some(validator) = std::env::var_os("RUZU_SPIRV_VAL") else {
            return;
        };
        let words = ctx.builder.module().assemble();
        let path = std::env::temp_dir().join(format!(
            "ruzu-{name}-{}-{}.spv",
            std::process::id(),
            words.len()
        ));
        let mut bytes = Vec::with_capacity(words.len() * 4);
        for word in words {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        std::fs::write(&path, bytes).unwrap();
        let output = std::process::Command::new(validator)
            .arg("--target-env")
            .arg("vulkan1.2")
            .arg(&path)
            .output()
            .unwrap();
        let _ = std::fs::remove_file(path);
        assert!(
            output.status.success(),
            "spirv-val failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn constant_sample_offset_follows_identity_values() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        block.append_inst(Inst::new(Opcode::Identity, vec![Value::ImmU32(1)]));
        block.append_inst(Inst::new(Opcode::Identity, vec![Value::ImmU32(u32::MAX)]));
        block.append_inst(Inst::new(
            Opcode::CompositeConstructU32x2,
            vec![
                Value::Inst(InstRef { block: 0, inst: 0 }),
                Value::Inst(InstRef { block: 0, inst: 1 }),
            ],
        ));

        assert_eq!(
            immediate_offset_components(&program, Value::Inst(InstRef { block: 0, inst: 2 })),
            Some(vec![1, u32::MAX])
        );
    }

    #[test]
    fn array_2d_fetch_adds_offset_without_modifying_layer() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.info.texture_descriptors.push(TextureDescriptor {
            texture_type: TextureType::ColorArray2D,
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
        });
        let info = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::ColorArray2D as u8,
            ..TextureInstInfo::default()
        };
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        let coords = block.append_inst(Inst::new(
            Opcode::CompositeConstructU32x3,
            vec![Value::ImmU32(10), Value::ImmU32(20), Value::ImmU32(3)],
        ));
        let offset = block.append_inst(Inst::new(
            Opcode::CompositeConstructU32x2,
            vec![Value::ImmU32(1), Value::ImmU32(2)],
        ));
        block.append_inst(Inst::with_flags(
            Opcode::ImageFetch,
            vec![
                Value::ImmU32(0),
                Value::Inst(InstRef {
                    block: 0,
                    inst: coords,
                }),
                Value::Inst(InstRef {
                    block: 0,
                    inst: offset,
                }),
                Value::ImmU32(0),
                Value::Void,
            ],
            info.to_u32(),
        ));
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];

        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());
        ctx.emit_program(&program);

        let offset_id = ctx.values[&(0, offset)];
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
            .expect("array fetch offset must be added to the coordinates");
        let Operand::IdRef(expanded_offset_id) = add.operands[1] else {
            panic!("IAdd offset must be an id");
        };
        let expanded_offset = instructions
            .iter()
            .find(|inst| inst.result_id == Some(expanded_offset_id))
            .expect("expanded array offset must be defined");
        assert_eq!(expanded_offset.class.opcode, spirv::Op::CompositeConstruct);

        for component in &expanded_offset.operands[..2] {
            let Operand::IdRef(component_id) = component else {
                panic!("expanded offset component must be an id");
            };
            let extract = instructions
                .iter()
                .find(|inst| inst.result_id == Some(*component_id))
                .expect("expanded offset component must be extracted");
            assert_eq!(extract.class.opcode, spirv::Op::CompositeExtract);
            assert_eq!(extract.operands[0], Operand::IdRef(offset_id));
        }
    }

    fn image_gather_context(profile: Profile, with_ptp_offsets: bool) -> SpirvEmitContext {
        let mut program = Program::new(ShaderStage::Fragment);
        program.info.texture_descriptors.push(TextureDescriptor {
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
        });
        let info = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::Color2D as u8,
            ..TextureInstInfo::default()
        };
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        let coords = block.append_inst(Inst::new(
            Opcode::CompositeConstructF32x2,
            vec![Value::ImmF32(0.25), Value::ImmF32(0.75)],
        ));
        let (offset, offset2) = if with_ptp_offsets {
            let first = block.append_inst(Inst::new(
                Opcode::CompositeConstructU32x4,
                vec![
                    Value::ImmU32(0),
                    Value::ImmU32(1),
                    Value::ImmU32(2),
                    Value::ImmU32(3),
                ],
            ));
            let second = block.append_inst(Inst::new(
                Opcode::CompositeConstructU32x4,
                vec![
                    Value::ImmU32(4),
                    Value::ImmU32(5),
                    Value::ImmU32(6),
                    Value::ImmU32(7),
                ],
            ));
            (
                Value::Inst(InstRef {
                    block: 0,
                    inst: first,
                }),
                Value::Inst(InstRef {
                    block: 0,
                    inst: second,
                }),
            )
        } else {
            (Value::Void, Value::Void)
        };
        block.append_inst(Inst::with_flags(
            Opcode::ImageGather,
            vec![
                Value::Void,
                Value::Inst(InstRef {
                    block: 0,
                    inst: coords,
                }),
                offset,
                offset2,
            ],
            info.to_u32(),
        ));
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];

        let mut ctx = SpirvEmitContext::new(&program, &profile, &RuntimeInfo::default());
        ctx.emit_program(&program);
        ctx
    }

    /// Builds a fragment shader sampling texture slot 0 of a descriptor array
    /// through a dynamically computed index, which is exactly the case upstream
    /// guards with `MarkNonUniform`.
    fn dynamic_texture_index_context(profile: Profile) -> SpirvEmitContext {
        let mut program = Program::new(ShaderStage::Fragment);
        program.info.texture_descriptors.push(TextureDescriptor {
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
            count: 4,
            size_shift: 0,
        });
        let info = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::Color2D as u8,
            ..TextureInstInfo::default()
        };
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        let coords = block.append_inst(Inst::new(
            Opcode::CompositeConstructF32x2,
            vec![Value::ImmF32(0.25), Value::ImmF32(0.75)],
        ));
        // A non-immediate index: upstream only decorates NonUniform in this case.
        let index = block.append_inst(Inst::new(
            Opcode::BitwiseAnd32,
            vec![Value::ImmU32(7), Value::ImmU32(3)],
        ));
        block.append_inst(Inst::with_flags(
            Opcode::ImageGather,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: index,
                }),
                Value::Inst(InstRef {
                    block: 0,
                    inst: coords,
                }),
                Value::Void,
                Value::Void,
            ],
            info.to_u32(),
        ));
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];

        let mut ctx = SpirvEmitContext::new(&program, &profile, &RuntimeInfo::default());
        ctx.emit_program(&program);
        ctx
    }

    fn non_uniform_decorated_ids(ctx: &SpirvEmitContext) -> usize {
        ctx.builder
            .module_ref()
            .annotations
            .iter()
            .filter(|instruction| {
                instruction.class.opcode == spirv::Op::Decorate
                    && instruction.operands.iter().any(|operand| {
                        matches!(operand, Operand::Decoration(spirv::Decoration::NonUniform))
                    })
            })
            .count()
    }

    #[test]
    fn dynamic_texture_index_decorates_non_uniform_when_supported() {
        let ctx = dynamic_texture_index_context(Profile {
            supported_spirv: 0x0001_0300,
            support_sampled_image_array_nonuniform_indexing: true,
            ..Profile::default()
        });
        // Upstream decorates the index, the access chain pointer and the loaded
        // object: three distinct ids.
        assert_eq!(non_uniform_decorated_ids(&ctx), 3);
        assert!(ctx.uses_nonuniform_sampled_image);
        validate_with_external_tool(ctx, "non-uniform-sampled-image");
    }

    #[test]
    fn dynamic_texture_index_skips_non_uniform_without_profile_support() {
        let ctx = dynamic_texture_index_context(Profile::default());
        assert_eq!(non_uniform_decorated_ids(&ctx), 0);
        assert!(!ctx.uses_nonuniform_sampled_image);
        assert!(ctx.non_uniform_ids.is_empty());
    }

    #[test]
    fn image_gather_applies_upstream_subpixel_offset_when_profile_requires_it() {
        let ctx = image_gather_context(
            Profile {
                need_gather_subpixel_offset: true,
                ..Profile::default()
            },
            false,
        );
        let opcodes = ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .map(|inst| inst.class.opcode)
            .collect::<Vec<_>>();
        assert!(opcodes.contains(&spirv::Op::ImageQuerySizeLod));
        assert!(opcodes.contains(&spirv::Op::ConvertUToF));
        assert!(opcodes.contains(&spirv::Op::FDiv));
        assert!(opcodes.contains(&spirv::Op::FAdd));
        assert!(opcodes.contains(&spirv::Op::ImageGather));
        validate_with_external_tool(ctx, "image-gather-subpixel-offset");
    }

    #[test]
    fn image_gather_preserves_ptp_const_offsets() {
        let ctx = image_gather_context(Profile::default(), true);
        let gather = ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .find(|inst| inst.class.opcode == spirv::Op::ImageGather)
            .expect("gather instruction must be emitted");
        assert!(gather.operands.iter().any(|operand| {
            matches!(
                operand,
                Operand::ImageOperands(mask)
                    if mask.contains(spirv::ImageOperands::CONST_OFFSETS)
            )
        }));
        validate_with_external_tool(ctx, "image-gather-ptp-offsets");
    }

    #[test]
    #[should_panic(expected = "SPIR-V: missing texture descriptor")]
    fn image_gather_rejects_missing_descriptor_like_upstream_at() {
        let mut program = Program::new(ShaderStage::Fragment);
        let info = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::Color2D as u8,
            ..TextureInstInfo::default()
        };
        program.blocks.push(Block::new());
        program.block_mut(0).append_inst(Inst::with_flags(
            Opcode::ImageGather,
            vec![Value::Void, Value::ImmF32(0.5), Value::Void, Value::Void],
            info.to_u32(),
        ));
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];

        let profile = Profile::default();
        let runtime_info = RuntimeInfo::default();
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        ctx.emit_program(&program);
    }

    #[test]
    fn image_query_lod_defines_vec4_result_for_component_extract() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.info.texture_descriptors.push(TextureDescriptor {
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
        });
        let info = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::Color2D as u8,
            ..TextureInstInfo::default()
        };
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        let coords = block.append_inst(Inst::new(
            Opcode::CompositeConstructF32x2,
            vec![Value::ImmF32(0.25), Value::ImmF32(0.75)],
        ));
        let query = block.append_inst(Inst::with_flags(
            Opcode::ImageQueryLod,
            vec![
                Value::Void,
                Value::Inst(InstRef {
                    block: 0,
                    inst: coords,
                }),
            ],
            info.to_u32(),
        ));
        block.append_inst(Inst::new(
            Opcode::CompositeExtractF32x4,
            vec![
                Value::Inst(InstRef {
                    block: 0,
                    inst: query,
                }),
                Value::ImmU32(0),
            ],
        ));
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];

        let profile = Profile::default();
        let runtime_info = RuntimeInfo::default();
        let mut ctx = SpirvEmitContext::new(&program, &profile, &runtime_info);
        ctx.emit_program(&program);

        let opcodes = ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .map(|inst| inst.class.opcode)
            .collect::<Vec<_>>();
        assert!(opcodes.contains(&spirv::Op::ImageQueryLod));
        assert!(opcodes.contains(&spirv::Op::CompositeConstruct));
        assert!(opcodes.contains(&spirv::Op::CompositeExtract));
        validate_with_external_tool(ctx, "image-query-lod");
    }

    #[test]
    fn image_gradient_emits_explicit_lod_with_grad_operands() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.info.texture_descriptors.push(TextureDescriptor {
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
        });
        let info = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::Color2D as u8,
            num_derivatives: 2,
            ..TextureInstInfo::default()
        };
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        let coords = block.append_inst(Inst::new(
            Opcode::CompositeConstructF32x2,
            vec![Value::ImmF32(0.25), Value::ImmF32(0.75)],
        ));
        let derivatives = block.append_inst(Inst::new(
            Opcode::CompositeConstructF32x4,
            vec![
                Value::ImmF32(0.125),
                Value::ImmF32(0.0),
                Value::ImmF32(0.0),
                Value::ImmF32(0.125),
            ],
        ));
        block.append_inst(Inst::with_flags(
            Opcode::ImageGradient,
            vec![
                Value::Void,
                Value::Inst(InstRef {
                    block: 0,
                    inst: coords,
                }),
                Value::Inst(InstRef {
                    block: 0,
                    inst: derivatives,
                }),
                Value::Void,
                Value::Void,
            ],
            info.to_u32(),
        ));
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];

        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());
        ctx.emit_program(&program);

        let gradient = ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .find(|inst| inst.class.opcode == spirv::Op::ImageSampleExplicitLod)
            .expect("gradient sample must be emitted");
        assert!(gradient.operands.iter().any(|operand| {
            matches!(
                operand,
                Operand::ImageOperands(mask) if mask.contains(spirv::ImageOperands::GRAD)
            )
        }));
        validate_with_external_tool(ctx, "image-gradient");
    }

    #[test]
    fn integer_texture_sample_uses_u32_result_then_bitcasts_like_upstream() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.info.texture_descriptors.push(TextureDescriptor {
            texture_type: TextureType::Color2D,
            is_depth: false,
            is_multisample: false,
            is_integer: true,
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
            texture_type: TextureType::Color2D as u8,
            is_integer: true,
            ..TextureInstInfo::default()
        };
        program.blocks.push(Block::new());
        let block = program.block_mut(0);
        let coords = block.append_inst(Inst::new(
            Opcode::CompositeConstructF32x2,
            vec![Value::ImmF32(0.25), Value::ImmF32(0.75)],
        ));
        block.append_inst(Inst::with_flags(
            Opcode::ImageSampleExplicitLod,
            vec![
                Value::Void,
                Value::Inst(InstRef {
                    block: 0,
                    inst: coords,
                }),
                Value::ImmF32(0.0),
                Value::Void,
            ],
            info.to_u32(),
        ));
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];

        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());
        let u32_vec4_type = ctx.u32_vec4_type;
        ctx.emit_program(&program);

        let instructions = ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .collect::<Vec<_>>();
        let sample = instructions
            .iter()
            .find(|inst| inst.class.opcode == spirv::Op::ImageSampleExplicitLod)
            .expect("integer sample must be emitted");
        assert_eq!(sample.result_type, Some(u32_vec4_type));
        assert!(instructions
            .iter()
            .any(|inst| inst.class.opcode == spirv::Op::Bitcast));
        validate_with_external_tool(ctx, "integer-texture-sample");
    }

    #[test]
    #[should_panic(expected = "reached the backend before indexing")]
    fn preindexed_image_opcode_is_not_silently_dropped() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        program
            .block_mut(0)
            .append_inst(Inst::new(Opcode::BoundImageGradient, vec![Value::Void; 5]));
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];

        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());
        ctx.emit_program(&program);
    }

    #[test]
    fn storage_image_read_write_emit_upstream_operations() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        program.info.image_descriptors.push(ImageDescriptor {
            texture_type: TextureType::Color2D,
            format: ImageFormat::R32G32B32A32Uint,
            is_written: true,
            is_read: true,
            is_integer: true,
            cbuf_index: 0,
            cbuf_offset: 0,
            count: 1,
            size_shift: 0,
        });
        let info = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::Color2D as u8,
            image_format: ImageFormat::R32G32B32A32Uint as u8,
            ..TextureInstInfo::default()
        };
        let block = program.block_mut(0);
        let coords = block.append_inst(Inst::new(
            Opcode::CompositeConstructU32x2,
            vec![Value::ImmU32(1), Value::ImmU32(2)],
        ));
        let color = block.append_inst(Inst::new(
            Opcode::CompositeConstructU32x4,
            vec![
                Value::ImmU32(3),
                Value::ImmU32(4),
                Value::ImmU32(5),
                Value::ImmU32(6),
            ],
        ));
        block.append_inst(Inst::with_flags(
            Opcode::ImageRead,
            vec![
                Value::ImmU32(0),
                Value::Inst(InstRef {
                    block: 0,
                    inst: coords,
                }),
            ],
            info.to_u32(),
        ));
        block.append_inst(Inst::with_flags(
            Opcode::ImageWrite,
            vec![
                Value::ImmU32(0),
                Value::Inst(InstRef {
                    block: 0,
                    inst: coords,
                }),
                Value::Inst(InstRef {
                    block: 0,
                    inst: color,
                }),
            ],
            info.to_u32(),
        ));
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];

        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());
        ctx.emit_program(&program);
        let opcodes = ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .map(|inst| inst.class.opcode)
            .collect::<Vec<_>>();
        assert!(opcodes.contains(&spirv::Op::ImageRead));
        assert!(opcodes.contains(&spirv::Op::ImageWrite));
        assert!(!opcodes.contains(&spirv::Op::Undef));
        validate_with_external_tool(ctx, "storage-image-read-write");
    }
}
