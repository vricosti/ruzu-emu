// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SPIR-V context get/set emission — maps to zuyu's
//! `backend/spirv/emit_spirv_context_get_set.cpp`.
//!
//! Handles constant buffer loads, attribute loads/stores, and other
//! context-related operations.

use super::spirv_emit_context::{InputGenericLoadOp, SpirvEmitContext, UniformDefinitionKind};
use crate::ir::types::ShaderStage;
use crate::ir::{self, Opcode};
use crate::runtime_info::AttributeType;
use rspirv::spirv::Word;

fn unreachable_instruction() -> ! {
    std::panic::panic_any(crate::exception::LogicError::new("Unreachable instruction"));
}

fn unimplemented_flag_instruction() -> ! {
    std::panic::panic_any(crate::exception::NotImplementedException::new(
        "SPIR-V Instruction",
    ));
}

pub fn emit_get_register(_ctx: &mut SpirvEmitContext) -> ! {
    unreachable_instruction()
}

pub fn emit_set_register(_ctx: &mut SpirvEmitContext) -> ! {
    unreachable_instruction()
}

pub fn emit_get_pred(_ctx: &mut SpirvEmitContext) -> ! {
    unreachable_instruction()
}

pub fn emit_set_pred(_ctx: &mut SpirvEmitContext) -> ! {
    unreachable_instruction()
}

pub fn emit_set_goto_variable(_ctx: &mut SpirvEmitContext) -> ! {
    unreachable_instruction()
}

pub fn emit_get_goto_variable(_ctx: &mut SpirvEmitContext) -> ! {
    unreachable_instruction()
}

pub fn emit_set_indirect_branch_variable(_ctx: &mut SpirvEmitContext) -> ! {
    unreachable_instruction()
}

pub fn emit_get_indirect_branch_variable(_ctx: &mut SpirvEmitContext) -> ! {
    unreachable_instruction()
}

pub fn emit_get_z_flag(_ctx: &mut SpirvEmitContext) -> ! {
    unimplemented_flag_instruction()
}

pub fn emit_get_s_flag(_ctx: &mut SpirvEmitContext) -> ! {
    unimplemented_flag_instruction()
}

pub fn emit_get_c_flag(_ctx: &mut SpirvEmitContext) -> ! {
    unimplemented_flag_instruction()
}

pub fn emit_get_o_flag(_ctx: &mut SpirvEmitContext) -> ! {
    unimplemented_flag_instruction()
}

pub fn emit_set_z_flag(_ctx: &mut SpirvEmitContext) -> ! {
    unimplemented_flag_instruction()
}

pub fn emit_set_s_flag(_ctx: &mut SpirvEmitContext) -> ! {
    unimplemented_flag_instruction()
}

pub fn emit_set_c_flag(_ctx: &mut SpirvEmitContext) -> ! {
    unimplemented_flag_instruction()
}

pub fn emit_set_o_flag(_ctx: &mut SpirvEmitContext) -> ! {
    unimplemented_flag_instruction()
}

/// Emit SetFragDepth.
pub fn emit_set_frag_depth(ctx: &mut SpirvEmitContext, value: Word) {
    let value = if ctx.runtime_info.convert_depth_mode && !ctx.profile.support_native_ndc {
        let half = ctx.constant_f32(0.5);
        ctx.builder
            .ext_inst(
                ctx.f32_type,
                None,
                ctx.glsl_ext,
                50, /* Fma */
                vec![
                    rspirv::dr::Operand::IdRef(value),
                    rspirv::dr::Operand::IdRef(half),
                    rspirv::dr::Operand::IdRef(half),
                ],
            )
            .unwrap()
    } else {
        value
    };
    ctx.builder
        .store(ctx.frag_depth, value, None, vec![])
        .unwrap();
}

/// Port of upstream `EmitSR_WScaleFactorXY`.
pub fn emit_sr_w_scale_factor_xy(ctx: &mut SpirvEmitContext) -> Word {
    log::warn!("(STUBBED) SR_WScaleFactorXY called");
    ctx.constant_u32(0x00ff_0000)
}

/// Port of upstream `EmitSR_WScaleFactorZ`.
pub fn emit_sr_w_scale_factor_z(ctx: &mut SpirvEmitContext) -> Word {
    log::warn!("(STUBBED) SR_WScaleFactorZ called");
    ctx.constant_u32(0x00ff_0000)
}

// ── IR-instruction dispatching helpers (called from spirv_emit_context) ───

fn cbuf_element_index(
    ctx: &mut SpirvEmitContext,
    offset: ir::Value,
    resolved_offset: Word,
    element_size: u32,
) -> Word {
    if let ir::Value::ImmU32(offset) = offset {
        return ctx.constant_u32(offset / element_size);
    }
    let shift = ctx.constant_u32(element_size.trailing_zeros());
    ctx.builder
        .shift_right_logical(ctx.u32_type, None, resolved_offset, shift)
        .unwrap()
}

fn load_cbuf_u32x4_element(
    ctx: &mut SpirvEmitContext,
    vector: Word,
    offset: ir::Value,
    resolved_offset: Word,
    index_offset: u32,
) -> Word {
    if let ir::Value::ImmU32(offset) = offset {
        return ctx
            .builder
            .composite_extract(
                ctx.u32_type,
                None,
                vector,
                vec![(offset / 4) % 4 + index_offset],
            )
            .unwrap();
    }
    let two = ctx.constant_u32(2);
    let word = ctx
        .builder
        .shift_right_logical(ctx.u32_type, None, resolved_offset, two)
        .unwrap();
    let three = ctx.constant_u32(3);
    let mut component = ctx
        .builder
        .bitwise_and(ctx.u32_type, None, word, three)
        .unwrap();
    if index_offset != 0 {
        let offset = ctx.constant_u32(index_offset);
        component = ctx
            .builder
            .i_add(ctx.u32_type, None, component, offset)
            .unwrap();
    }
    ctx.builder
        .vector_extract_dynamic(ctx.u32_type, None, vector, component)
        .unwrap()
}

fn get_cbuf(
    ctx: &mut SpirvEmitContext,
    result_type: Word,
    kind: UniformDefinitionKind,
    element_size: u32,
    binding: ir::Value,
    offset: ir::Value,
    indirect_func: Word,
) -> Word {
    let resolved_offset = ctx.resolve_value(&offset);
    let buffer_offset = cbuf_element_index(ctx, offset, resolved_offset, element_size);
    if !binding.is_immediate() {
        assert_ne!(indirect_func, 0, "missing indirect CBUF accessor {kind:?}");
        let binding = ctx.resolve_value(&binding);
        return ctx
            .builder
            .function_call(
                result_type,
                None,
                indirect_func,
                vec![binding, buffer_offset],
            )
            .unwrap();
    }

    let cbuf_index = binding.imm_u32();
    let definitions = ctx.cbufs.get(&cbuf_index).copied().unwrap_or_default();
    let cbuf = definitions.get(kind);
    let pointer_type = ctx.uniform_types.get(kind);
    assert_ne!(cbuf, 0, "missing CBUF {cbuf_index} view {kind:?}");
    assert_ne!(pointer_type, 0, "missing CBUF pointer type {kind:?}");
    let pointer = ctx
        .builder
        .access_chain(
            pointer_type,
            None,
            cbuf,
            vec![ctx.const_zero_u32, buffer_offset],
        )
        .unwrap();
    let value = ctx
        .builder
        .load(result_type, None, pointer, None, [])
        .unwrap();
    if offset.is_immediate() || !ctx.profile.has_broken_robust {
        return value;
    }

    let maximum = ctx.constant_u32(0xffff);
    let in_bounds = ctx
        .builder
        .u_less_than_equal(ctx.bool_type, None, buffer_offset, maximum)
        .unwrap();
    let zero = ctx.builder.constant_null(result_type);
    ctx.builder
        .select(result_type, None, in_bounds, value, zero)
        .unwrap()
}

fn get_cbuf_u32(ctx: &mut SpirvEmitContext, binding: ir::Value, offset: ir::Value) -> Word {
    get_cbuf(
        ctx,
        ctx.u32_type,
        UniformDefinitionKind::U32,
        4,
        binding,
        offset,
        ctx.load_const_func_u32,
    )
}

fn get_cbuf_u32x4(ctx: &mut SpirvEmitContext, binding: ir::Value, offset: ir::Value) -> Word {
    get_cbuf(
        ctx,
        ctx.u32_vec4_type,
        UniformDefinitionKind::U32x4,
        16,
        binding,
        offset,
        ctx.load_const_func_u32x4,
    )
}

/// Dispatch constant-buffer load IR instructions.
pub fn emit_get_cbuf(ctx: &mut SpirvEmitContext, inst: &ir::Inst, block_idx: u32, inst_idx: u32) {
    let binding = *inst.arg(0);
    let offset = *inst.arg(1);

    let id = match inst.opcode {
        Opcode::GetCbufU8
            if ctx.profile.support_descriptor_aliasing
                && ctx.profile.support_int8
                && ctx.profile.support_uniform_and_storage_buffer_8bit =>
        {
            let value = get_cbuf(
                ctx,
                ctx.u8_type,
                UniformDefinitionKind::U8,
                1,
                binding,
                offset,
                ctx.load_const_func_u8,
            );
            ctx.builder.u_convert(ctx.u32_type, None, value).unwrap()
        }
        Opcode::GetCbufS8
            if ctx.profile.support_descriptor_aliasing
                && ctx.profile.support_int8
                && ctx.profile.support_uniform_and_storage_buffer_8bit =>
        {
            let value = get_cbuf(
                ctx,
                ctx.i8_type,
                UniformDefinitionKind::I8,
                1,
                binding,
                offset,
                ctx.load_const_func_u8,
            );
            ctx.builder.s_convert(ctx.u32_type, None, value).unwrap()
        }
        Opcode::GetCbufU16
            if ctx.profile.support_descriptor_aliasing
                && ctx.profile.support_int16
                && ctx.profile.support_uniform_and_storage_buffer_16bit =>
        {
            let value = get_cbuf(
                ctx,
                ctx.u16_type,
                UniformDefinitionKind::U16,
                2,
                binding,
                offset,
                ctx.load_const_func_u16,
            );
            ctx.builder.u_convert(ctx.u32_type, None, value).unwrap()
        }
        Opcode::GetCbufS16
            if ctx.profile.support_descriptor_aliasing
                && ctx.profile.support_int16
                && ctx.profile.support_uniform_and_storage_buffer_16bit =>
        {
            let value = get_cbuf(
                ctx,
                ctx.i16_type,
                UniformDefinitionKind::I16,
                2,
                binding,
                offset,
                ctx.load_const_func_u16,
            );
            ctx.builder.s_convert(ctx.u32_type, None, value).unwrap()
        }
        Opcode::GetCbufU8 | Opcode::GetCbufS8 | Opcode::GetCbufU16 | Opcode::GetCbufS16 => {
            let word = if ctx.profile.support_descriptor_aliasing {
                get_cbuf_u32(ctx, binding, offset)
            } else {
                let vector = get_cbuf_u32x4(ctx, binding, offset);
                let resolved_offset = ctx.resolve_value(&offset);
                load_cbuf_u32x4_element(ctx, vector, offset, resolved_offset, 0)
            };
            let (width, signed) = match inst.opcode {
                Opcode::GetCbufU8 => (8, false),
                Opcode::GetCbufS8 => (8, true),
                Opcode::GetCbufU16 => (16, false),
                Opcode::GetCbufS16 => (16, true),
                _ => unreachable!(),
            };
            let bit_offset = if width == 8 {
                ctx.bit_offset_8(offset)
            } else {
                ctx.bit_offset_16(offset)
            };
            let width = ctx.constant_u32(width);
            if signed {
                ctx.builder
                    .bit_field_s_extract(ctx.u32_type, None, word, bit_offset, width)
                    .unwrap()
            } else {
                ctx.builder
                    .bit_field_u_extract(ctx.u32_type, None, word, bit_offset, width)
                    .unwrap()
            }
        }
        Opcode::GetCbufU32 if ctx.profile.support_descriptor_aliasing => {
            get_cbuf_u32(ctx, binding, offset)
        }
        Opcode::GetCbufF32 if ctx.profile.support_descriptor_aliasing => get_cbuf(
            ctx,
            ctx.f32_type,
            UniformDefinitionKind::F32,
            4,
            binding,
            offset,
            ctx.load_const_func_f32,
        ),
        Opcode::GetCbufU32x2 if ctx.profile.support_descriptor_aliasing => get_cbuf(
            ctx,
            ctx.u32_vec2_type,
            UniformDefinitionKind::U32x2,
            8,
            binding,
            offset,
            ctx.load_const_func_u32x2,
        ),
        Opcode::GetCbufU32 | Opcode::GetCbufF32 | Opcode::GetCbufU32x2 => {
            let vector = get_cbuf_u32x4(ctx, binding, offset);
            let resolved_offset = ctx.resolve_value(&offset);
            if inst.opcode == Opcode::GetCbufU32x2 {
                let first = load_cbuf_u32x4_element(ctx, vector, offset, resolved_offset, 0);
                let second = load_cbuf_u32x4_element(ctx, vector, offset, resolved_offset, 1);
                ctx.builder
                    .composite_construct(ctx.u32_vec2_type, None, vec![first, second])
                    .unwrap()
            } else {
                let word = load_cbuf_u32x4_element(ctx, vector, offset, resolved_offset, 0);
                if inst.opcode == Opcode::GetCbufF32 {
                    ctx.builder.bitcast(ctx.f32_type, None, word).unwrap()
                } else {
                    word
                }
            }
        }
        _ => unreachable!("non-CBUF opcode {:?}", inst.opcode),
    };
    ctx.set_value(block_idx, inst_idx, id);
}

/// Dispatch GetAttribute / GetAttributeU32 IR instructions.
pub fn emit_get_attribute_inst(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    block_idx: u32,
    inst_idx: u32,
) {
    let attr = inst.arg(0).attribute();
    let vertex = ctx.resolve_value(inst.arg(1));
    let id = if inst.opcode == Opcode::GetAttributeU32 {
        let id = emit_get_attribute_u32_value(ctx, attr);
        id
    } else if attr.is_position() {
        let comp = attr.position_element();
        let component = ctx.constant_u32(comp);
        let mut indices = input_invocation_indices(ctx, vertex);
        if ctx.need_input_position_indirect {
            indices.push(ctx.const_zero_u32);
        }
        indices.push(component);
        let pointer = ctx
            .builder
            .access_chain(ctx.input_f32_ptr, None, ctx.input_position, indices)
            .unwrap();
        ctx.builder
            .load(ctx.f32_type, None, pointer, None, [])
            .unwrap()
    } else if attr.is_generic() {
        let generic = ctx.input_generics[attr.generic_index() as usize];
        if generic.id == 0 {
            if attr.generic_element() == 3 {
                ctx.const_one_f32
            } else {
                ctx.const_zero_f32
            }
        } else {
            let component = ctx.constant_u32(attr.generic_element());
            let mut indices = input_invocation_indices(ctx, vertex);
            indices.push(component);
            let pointer = ctx
                .builder
                .access_chain(generic.pointer_type, None, generic.id, indices)
                .unwrap();
            let value = ctx
                .builder
                .load(generic.component_type, None, pointer, None, [])
                .unwrap();
            match generic.load_op {
                InputGenericLoadOp::None => value,
                InputGenericLoadOp::Bitcast => {
                    ctx.builder.bitcast(ctx.f32_type, None, value).unwrap()
                }
                InputGenericLoadOp::SToF => ctx
                    .builder
                    .convert_s_to_f(ctx.f32_type, None, value)
                    .unwrap(),
                InputGenericLoadOp::UToF => ctx
                    .builder
                    .convert_u_to_f(ctx.f32_type, None, value)
                    .unwrap(),
            }
        }
    } else {
        emit_get_attribute_f32_value(ctx, attr)
    };
    ctx.set_value(block_idx, inst_idx, id);
}

fn input_invocation_indices(ctx: &SpirvEmitContext, vertex: Word) -> Vec<Word> {
    if matches!(
        ctx.stage,
        ShaderStage::TessellationControl | ShaderStage::TessellationEval | ShaderStage::Geometry
    ) {
        vec![vertex]
    } else {
        Vec::new()
    }
}

fn emit_get_attribute_f32_value(
    ctx: &mut SpirvEmitContext,
    attr: crate::ir::value::Attribute,
) -> Word {
    use crate::ir::value::Attribute;

    match attr {
        Attribute::PRIMITIVE_ID => bitcast_u32_builtin_to_f32(ctx, ctx.primitive_id),
        Attribute::LAYER => bitcast_u32_builtin_to_f32(ctx, ctx.layer),
        Attribute::INSTANCE_ID => {
            if ctx.profile.support_vertex_instance_id {
                bitcast_u32_builtin_to_f32(ctx, ctx.instance_id)
            } else {
                let instance = load_u32_builtin(ctx, ctx.instance_index);
                let base = load_u32_builtin(ctx, ctx.base_instance);
                let value = ctx
                    .builder
                    .i_sub(ctx.u32_type, None, instance, base)
                    .unwrap();
                ctx.builder.bitcast(ctx.f32_type, None, value).unwrap()
            }
        }
        Attribute::VERTEX_ID => {
            if ctx.profile.support_vertex_instance_id {
                bitcast_u32_builtin_to_f32(ctx, ctx.vertex_id)
            } else {
                bitcast_u32_builtin_to_f32(ctx, ctx.vertex_index)
            }
        }
        Attribute::BASE_INSTANCE => bitcast_u32_builtin_to_f32(ctx, ctx.base_instance),
        Attribute::BASE_VERTEX => bitcast_u32_builtin_to_f32(ctx, ctx.base_vertex),
        Attribute::DRAW_ID => bitcast_u32_builtin_to_f32(ctx, ctx.draw_index),
        Attribute::FRONT_FACE => {
            let front = ctx
                .builder
                .load(ctx.bool_type, None, ctx.front_face, None, vec![])
                .unwrap();
            let true_value = ctx.builder.constant_bit32(ctx.u32_type, u32::MAX);
            let true_value = ctx.builder.bitcast(ctx.f32_type, None, true_value).unwrap();
            ctx.builder
                .select(ctx.f32_type, None, front, true_value, ctx.const_zero_f32)
                .unwrap()
        }
        Attribute::POINT_SPRITE_S => load_f32_vec_component(ctx, ctx.point_coord, 0, 2),
        Attribute::POINT_SPRITE_T => load_f32_vec_component(ctx, ctx.point_coord, 1, 2),
        Attribute::TESSELLATION_EVALUATION_POINT_U => {
            load_f32_vec_component(ctx, ctx.tess_coord, 0, 3)
        }
        Attribute::TESSELLATION_EVALUATION_POINT_V => {
            load_f32_vec_component(ctx, ctx.tess_coord, 1, 3)
        }
        _ => panic!("unsupported input attribute {attr}"),
    }
}

fn emit_get_attribute_u32_value(
    ctx: &mut SpirvEmitContext,
    attr: crate::ir::value::Attribute,
) -> Word {
    use crate::ir::value::Attribute;

    match attr {
        Attribute::PRIMITIVE_ID => load_u32_builtin(ctx, ctx.primitive_id),
        Attribute::INSTANCE_ID => {
            if ctx.profile.support_vertex_instance_id {
                load_u32_builtin(ctx, ctx.instance_id)
            } else {
                let instance = load_u32_builtin(ctx, ctx.instance_index);
                let base = load_u32_builtin(ctx, ctx.base_instance);
                ctx.builder
                    .i_sub(ctx.u32_type, None, instance, base)
                    .unwrap()
            }
        }
        Attribute::VERTEX_ID => {
            if ctx.profile.support_vertex_instance_id {
                load_u32_builtin(ctx, ctx.vertex_id)
            } else {
                load_u32_builtin(ctx, ctx.vertex_index)
            }
        }
        Attribute::BASE_INSTANCE => load_u32_builtin(ctx, ctx.base_instance),
        Attribute::BASE_VERTEX => load_u32_builtin(ctx, ctx.base_vertex),
        Attribute::DRAW_ID => load_u32_builtin(ctx, ctx.draw_index),
        _ => panic!("unsupported u32 input attribute {attr}"),
    }
}

fn load_u32_builtin(ctx: &mut SpirvEmitContext, var: Word) -> Word {
    if var == 0 {
        return ctx.const_zero_u32;
    }
    ctx.builder
        .load(ctx.u32_type, None, var, None, vec![])
        .unwrap()
}

fn bitcast_u32_builtin_to_f32(ctx: &mut SpirvEmitContext, var: Word) -> Word {
    let value = load_u32_builtin(ctx, var);
    ctx.builder.bitcast(ctx.f32_type, None, value).unwrap()
}

fn load_f32_vec_component(
    ctx: &mut SpirvEmitContext,
    var: Word,
    component: u32,
    component_count: u32,
) -> Word {
    if var == 0 {
        return ctx.const_zero_f32;
    }
    let pointer_type =
        ctx.builder
            .type_pointer(None, rspirv::spirv::StorageClass::Input, ctx.f32_type);
    let index = ctx.builder.constant_bit32(ctx.u32_type, component);
    let ptr = ctx
        .builder
        .access_chain(pointer_type, None, var, vec![index])
        .unwrap();
    debug_assert!(component < component_count);
    ctx.builder
        .load(ctx.f32_type, None, ptr, None, vec![])
        .unwrap()
}

fn output_access_chain(
    ctx: &mut SpirvEmitContext,
    pointer_type: Word,
    base: Word,
    mut indices: Vec<Word>,
) -> Word {
    if ctx.stage == ShaderStage::TessellationControl {
        let invocation = ctx
            .builder
            .load(ctx.u32_type, None, ctx.invocation_id, None, [])
            .unwrap();
        indices.insert(0, invocation);
    }
    ctx.builder
        .access_chain(pointer_type, None, base, indices)
        .unwrap()
}

struct OutAttr {
    pointer: Word,
    value_type: Option<Word>,
}

fn output_attr_pointer(
    ctx: &mut SpirvEmitContext,
    attr: crate::ir::value::Attribute,
) -> Option<OutAttr> {
    use crate::ir::value::Attribute;

    if attr.is_generic() {
        let element = attr.generic_element();
        let info = ctx.output_generics[attr.generic_index() as usize][element as usize];
        assert_ne!(info.id, 0, "missing generic output for {attr}");
        if info.num_components == 1 {
            return Some(OutAttr {
                pointer: info.id,
                value_type: None,
            });
        }
        let index = ctx.constant_u32(element - info.first_element);
        return Some(OutAttr {
            pointer: output_access_chain(ctx, ctx.output_f32_ptr, info.id, vec![index]),
            value_type: None,
        });
    }
    if attr == Attribute::POINT_SIZE {
        return Some(OutAttr {
            pointer: ctx.output_point_size,
            value_type: None,
        });
    }
    if attr.is_position() {
        let element = ctx.constant_u32(attr.position_element());
        return Some(OutAttr {
            pointer: output_access_chain(
                ctx,
                ctx.output_f32_ptr,
                ctx.output_position,
                vec![element],
            ),
            value_type: None,
        });
    }
    if attr.is_clip_distance() {
        let index = attr.clip_distance_index();
        if index >= ctx.profile.max_user_clip_distances {
            log::warn!(
                "Ignoring clip distance store {} >= {} supported",
                index,
                ctx.profile.max_user_clip_distances
            );
            return None;
        }
        let index = ctx.constant_u32(index);
        return Some(OutAttr {
            pointer: output_access_chain(ctx, ctx.output_f32_ptr, ctx.clip_distances, vec![index]),
            value_type: None,
        });
    }
    match attr {
        Attribute::LAYER => (ctx.profile.support_viewport_index_layer_non_geometry
            || ctx.stage == ShaderStage::Geometry)
            .then_some(OutAttr {
                pointer: ctx.layer,
                value_type: Some(ctx.u32_type),
            }),
        Attribute::VIEWPORT_INDEX => {
            if !ctx.profile.support_multi_viewport {
                log::warn!("Ignoring viewport index store on non-supporting driver");
                return None;
            }
            (ctx.profile.support_viewport_index_layer_non_geometry
                || ctx.stage == ShaderStage::Geometry)
                .then_some(OutAttr {
                    pointer: ctx.viewport_index,
                    value_type: Some(ctx.u32_type),
                })
        }
        Attribute::VIEWPORT_MASK if ctx.profile.support_viewport_mask => {
            let pointer = ctx
                .builder
                .access_chain(
                    ctx.output_u32_ptr,
                    None,
                    ctx.viewport_mask,
                    vec![ctx.const_zero_u32],
                )
                .unwrap();
            Some(OutAttr {
                pointer,
                value_type: Some(ctx.u32_type),
            })
        }
        Attribute::VIEWPORT_MASK => None,
        _ => panic!("unsupported output attribute {attr}"),
    }
}

/// Dispatch SetAttribute IR instructions.
pub fn emit_set_attribute_inst(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    _block_idx: u32,
    _inst_idx: u32,
) {
    let attr = inst.arg(0).attribute();
    let mut value = ctx.resolve_value(inst.arg(1));
    let Some(output) = output_attr_pointer(ctx, attr) else {
        return;
    };
    if let Some(value_type) = output.value_type {
        value = ctx.builder.bitcast(value_type, None, value).unwrap();
    }
    ctx.builder.store(output.pointer, value, None, []).unwrap();
}

/// Dispatch GetAttributeIndexed IR instructions.
pub fn emit_get_attribute_indexed_inst(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    block_idx: u32,
    inst_idx: u32,
) {
    assert_ne!(
        ctx.indexed_load_func, 0,
        "missing indexed attribute load function"
    );
    let offset = ctx.resolve_value(inst.arg(0));
    let mut arguments = vec![offset];
    if matches!(
        ctx.stage,
        ShaderStage::TessellationControl | ShaderStage::TessellationEval | ShaderStage::Geometry
    ) {
        arguments.push(ctx.resolve_value(inst.arg(1)));
    }
    let value = ctx
        .builder
        .function_call(ctx.f32_type, None, ctx.indexed_load_func, arguments)
        .unwrap();
    ctx.set_value(block_idx, inst_idx, value);
}

/// Dispatch SetAttributeIndexed IR instructions.
pub fn emit_set_attribute_indexed_inst(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    _block_idx: u32,
    _inst_idx: u32,
) {
    assert_ne!(
        ctx.indexed_store_func, 0,
        "missing indexed attribute store function"
    );
    let offset = ctx.resolve_value(inst.arg(0));
    let value = ctx.resolve_value(inst.arg(1));
    ctx.builder
        .function_call(
            ctx.void_type,
            None,
            ctx.indexed_store_func,
            vec![offset, value],
        )
        .unwrap();
}

/// Dispatch GetPatch IR instructions.
pub fn emit_get_patch_inst(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    block_idx: u32,
    inst_idx: u32,
) {
    let patch = inst.arg(0).patch();
    assert!(patch.is_generic(), "non-generic patch load {patch:?}");
    let element = ctx.constant_u32(patch.generic_element());
    let pointer_type = if ctx.stage == ShaderStage::TessellationControl {
        ctx.output_f32_ptr
    } else {
        ctx.input_f32_ptr
    };
    let pointer = ctx
        .builder
        .access_chain(
            pointer_type,
            None,
            ctx.patches[patch.generic_index() as usize],
            vec![element],
        )
        .unwrap();
    let value = ctx
        .builder
        .load(ctx.f32_type, None, pointer, None, [])
        .unwrap();
    ctx.set_value(block_idx, inst_idx, value);
}

/// Dispatch SetPatch IR instructions.
pub fn emit_set_patch_inst(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    _block_idx: u32,
    _inst_idx: u32,
) {
    use crate::ir::value::Patch;

    let patch = inst.arg(0).patch();
    let value = ctx.resolve_value(inst.arg(1));
    let (base, element) = if patch.is_generic() {
        (
            ctx.patches[patch.generic_index() as usize],
            patch.generic_element(),
        )
    } else {
        match patch {
            Patch::TESS_LOD_LEFT
            | Patch::TESS_LOD_TOP
            | Patch::TESS_LOD_RIGHT
            | Patch::TESS_LOD_BOTTOM => (ctx.output_tess_level_outer, patch.0),
            Patch::TESS_LOD_INTERIOR_U => (ctx.output_tess_level_inner, 0),
            Patch::TESS_LOD_INTERIOR_V => (ctx.output_tess_level_inner, 1),
            _ => panic!("unsupported patch output {patch:?}"),
        }
    };
    let element = ctx.constant_u32(element);
    let pointer = ctx
        .builder
        .access_chain(ctx.output_f32_ptr, None, base, vec![element])
        .unwrap();
    ctx.builder.store(pointer, value, None, []).unwrap();
}

/// Dispatch SetFragColor IR instructions.
pub fn emit_set_frag_color_inst(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    _block_idx: u32,
    _inst_idx: u32,
) {
    let rt = inst.arg(0).imm_u32();
    let comp = inst.arg(1).imm_u32();
    let val = ctx.resolve_value(inst.arg(2));

    let output_var = ctx.frag_color[rt as usize];
    let idx_const = ctx.constant_u32(comp);
    match ctx.runtime_info.frag_color_types[rt as usize] {
        AttributeType::UnsignedInt => {
            let ptr = ctx
                .builder
                .access_chain(ctx.output_u32_ptr, None, output_var, vec![idx_const])
                .unwrap();
            let value = ctx.builder.bitcast(ctx.u32_type, None, val).unwrap();
            ctx.builder.store(ptr, value, None, []).unwrap();
        }
        AttributeType::SignedInt => {
            let ptr = ctx
                .builder
                .access_chain(ctx.output_i32_ptr, None, output_var, vec![idx_const])
                .unwrap();
            let value = ctx.builder.bitcast(ctx.i32_type, None, val).unwrap();
            ctx.builder.store(ptr, value, None, []).unwrap();
        }
        _ => {
            let ptr = ctx
                .builder
                .access_chain(ctx.output_f32_ptr, None, output_var, vec![idx_const])
                .unwrap();
            ctx.builder.store(ptr, val, None, []).unwrap();
        }
    }
}

/// Dispatch SetSampleMask IR instructions.
pub fn emit_set_sample_mask_inst(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    _block_idx: u32,
    _inst_idx: u32,
) {
    let value = ctx.resolve_value(inst.arg(0));
    let pointer = ctx
        .builder
        .access_chain(
            ctx.output_u32_ptr,
            None,
            ctx.sample_mask,
            vec![ctx.const_zero_u32],
        )
        .unwrap();
    ctx.builder.store(pointer, value, None, []).unwrap();
}

/// Dispatch SetFragDepth IR instructions.
pub fn emit_set_frag_depth_inst(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    _block_idx: u32,
    _inst_idx: u32,
) {
    let value = ctx.resolve_value(inst.arg(0));
    emit_set_frag_depth(ctx, value);
}

// ── System value emission (matches upstream Emit* functions) ──────────────

/// Matches upstream `EmitWorkgroupId`.
pub fn emit_workgroup_id(ctx: &mut SpirvEmitContext) -> Word {
    ctx.builder
        .load(ctx.u32_vec3_type, None, ctx.workgroup_id, None, vec![])
        .unwrap()
}

/// Matches upstream `EmitLocalInvocationId`.
pub fn emit_local_invocation_id(ctx: &mut SpirvEmitContext) -> Word {
    ctx.builder
        .load(
            ctx.u32_vec3_type,
            None,
            ctx.local_invocation_id,
            None,
            vec![],
        )
        .unwrap()
}

/// Matches upstream `EmitInvocationId`.
pub fn emit_invocation_id(ctx: &mut SpirvEmitContext) -> Word {
    ctx.builder
        .load(ctx.u32_type, None, ctx.invocation_id, None, vec![])
        .unwrap()
}

/// Matches upstream `EmitInvocationInfo`.
pub fn emit_invocation_info(ctx: &mut SpirvEmitContext) -> Word {
    match ctx.stage {
        ShaderStage::TessellationControl | ShaderStage::TessellationEval => {
            let loaded = ctx
                .builder
                .load(ctx.u32_type, None, ctx.patch_vertices_in, None, vec![])
                .unwrap();
            let shift = ctx.builder.constant_bit32(ctx.u32_type, 16);
            ctx.builder
                .shift_left_logical(ctx.u32_type, None, loaded, shift)
                .unwrap()
        }
        ShaderStage::Geometry => {
            let vertices = ctx
                .builder
                .constant_bit32(ctx.u32_type, ctx.runtime_info.input_topology.vertices());
            let shift = ctx.builder.constant_bit32(ctx.u32_type, 16);
            ctx.builder
                .shift_left_logical(ctx.u32_type, None, vertices, shift)
                .unwrap()
        }
        _ => {
            log::warn!("(STUBBED) EmitInvocationInfo called for non-tessellation stage");
            ctx.builder.constant_bit32(ctx.u32_type, 0x00ff0000u32)
        }
    }
}

/// Matches upstream `EmitSampleId`.
pub fn emit_sample_id(ctx: &mut SpirvEmitContext) -> Word {
    ctx.builder
        .load(ctx.u32_type, None, ctx.sample_id, None, vec![])
        .unwrap()
}

/// Matches upstream `EmitIsHelperInvocation`.
pub fn emit_is_helper_invocation(ctx: &mut SpirvEmitContext) -> Word {
    ctx.builder
        .load(ctx.bool_type, None, ctx.is_helper_invocation, None, vec![])
        .unwrap()
}

/// Matches upstream `EmitYDirection`.
pub fn emit_y_direction(ctx: &mut SpirvEmitContext) -> Word {
    let value = if ctx.runtime_info.y_negate {
        -1.0f32
    } else {
        1.0f32
    };
    ctx.constant_f32(value)
}

/// Matches upstream `EmitResolutionDownFactor`.
pub fn emit_resolution_down_factor(ctx: &mut SpirvEmitContext) -> Word {
    if ctx.profile.unified_descriptor_binding {
        let pointer_type = ctx.builder.type_pointer(
            None,
            rspirv::spirv::StorageClass::PushConstant,
            ctx.f32_type,
        );
        let index = ctx
            .builder
            .constant_bit32(ctx.u32_type, ctx.rescaling_downfactor_member_index);
        let pointer = ctx
            .builder
            .access_chain(
                pointer_type,
                None,
                ctx.rescaling_push_constants,
                vec![index],
            )
            .unwrap();
        ctx.builder
            .load(ctx.f32_type, None, pointer, None, vec![])
            .unwrap()
    } else {
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
        ctx.builder
            .composite_extract(ctx.f32_type, None, composite, vec![2])
            .unwrap()
    }
}

/// Matches upstream `EmitRenderArea`.
pub fn emit_render_area(ctx: &mut SpirvEmitContext) -> Word {
    if ctx.profile.unified_descriptor_binding {
        let pointer_type = ctx.builder.type_pointer(
            None,
            rspirv::spirv::StorageClass::PushConstant,
            ctx.f32_vec4_type,
        );
        let index = ctx
            .builder
            .constant_bit32(ctx.u32_type, ctx.render_are_member_index);
        let pointer = ctx
            .builder
            .access_chain(
                pointer_type,
                None,
                ctx.render_area_push_constant,
                vec![index],
            )
            .unwrap();
        ctx.builder
            .load(ctx.f32_vec4_type, None, pointer, None, vec![])
            .unwrap()
    } else {
        panic!("EmitRenderArea: non-unified descriptor binding not implemented");
    }
}

/// Port of upstream `EmitLoadLocal`.
pub fn emit_load_local(ctx: &mut SpirvEmitContext, word_offset: Word) -> Word {
    let pointer = ctx
        .builder
        .access_chain(
            ctx.private_u32_ptr,
            None,
            ctx.local_memory,
            vec![word_offset],
        )
        .unwrap();
    ctx.builder
        .load(ctx.u32_type, None, pointer, None, vec![])
        .unwrap()
}

/// Port of upstream `EmitWriteLocal`.
pub fn emit_write_local(ctx: &mut SpirvEmitContext, word_offset: Word, value: Word) {
    let pointer = ctx
        .builder
        .access_chain(
            ctx.private_u32_ptr,
            None,
            ctx.local_memory,
            vec![word_offset],
        )
        .unwrap();
    ctx.builder.store(pointer, value, None, vec![]).unwrap();
}
