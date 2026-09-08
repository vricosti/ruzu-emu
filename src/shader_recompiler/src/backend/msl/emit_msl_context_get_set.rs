// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native-MSL context reads and writes.
//!
//! This file owns the MSL equivalents of Eden's
//! `backend/spirv/emit_spirv_context_get_set.cpp` operations.

use crate::ir;
use crate::ir::opcodes::Opcode;
use crate::ir::value::{InstRef, Value};

use super::msl_emit_context::MslEmitContext;
use super::MslError;

/// Eden EmitSetAttribute: select the output interface and preserve raw integer
/// bit patterns when the IR value is carried as a float.
pub fn emit_set_attribute(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &ir::Inst,
) -> Result<(), MslError> {
    let Value::Attribute(attribute) = inst.arg(0) else {
        return Err(MslError::ExpectedImmediate {
            opcode: inst.opcode,
            arg: 0,
            expected: "attribute",
        });
    };
    if context.stage() == crate::stage::Stage::TessellationControl {
        if attribute.is_clip_distance() && attribute.clip_distance_index() >= context.clip_distance_count() {
            return Ok(());
        }
        let destination = context.tessellation_control_layout()?.output_expression(*attribute)?;
        let value = context.value_expression(inst.arg(1), inst_ref, 1)?;
        context.emit_statement(&format!("{destination} = {value};"));
        return Ok(());
    }
    // Eden's EmitSetAttribute ignores vertex: these writes update the current
    // output record, and GS EmitVertex captures that record separately.
    if attribute.is_generic() {
        return context.emit_set_generic(inst_ref, *attribute, inst.arg(1));
    }
    if attribute.is_position() {
        return context.emit_set_position(inst_ref, attribute.position_element(), inst.arg(1));
    }
    if *attribute == crate::ir::value::Attribute::POINT_SIZE {
        return context.emit_set_point_size(inst_ref, inst.arg(1));
    }
    if attribute.is_clip_distance() {
        return context.emit_set_clip_distance(inst_ref, attribute.clip_distance_index(), inst.arg(1));
    }
    if *attribute == crate::ir::value::Attribute::LAYER {
        return context.emit_set_layer(inst_ref, inst.arg(1));
    }
    if *attribute == crate::ir::value::Attribute::VIEWPORT_INDEX {
        return context.emit_set_viewport_index(inst_ref, inst.arg(1));
    }
    Err(MslError::UnsupportedAttribute(attribute.0))
}

/// Emit the non-aliasing `uint4` CBUF path used by the Metal profile.
pub fn emit_get_cbuf(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &ir::Inst,
) -> Result<(), MslError> {
    let binding = inst.arg(0);
    let offset = inst.arg(1);
    let word = context.constant_buffer_element_expression(inst_ref, binding, offset, 0)?;
    match inst.opcode {
        Opcode::GetCbufU8 | Opcode::GetCbufS8 | Opcode::GetCbufU16 | Opcode::GetCbufS16 => {
            let (width, signed) = match inst.opcode {
                Opcode::GetCbufU8 => (8, false),
                Opcode::GetCbufS8 => (8, true),
                Opcode::GetCbufU16 => (16, false),
                Opcode::GetCbufS16 => (16, true),
                _ => unreachable!(),
            };
            let bit_offset = context.bit_offset_expression(inst_ref, offset, width)?;
            let expression = if signed {
                format!("as_type<uint>(extract_bits(as_type<int>({word}), {bit_offset}, {width}u))")
            } else {
                format!("extract_bits({word}, {bit_offset}, {width}u)")
            };
            context.define(inst_ref, ir::Type::U32, expression, false)
        }
        Opcode::GetCbufU32 => context.define(inst_ref, ir::Type::U32, word, false),
        Opcode::GetCbufF32 => context.define(
            inst_ref,
            ir::Type::F32,
            format!("as_type<float>({word})"),
            false,
        ),
        Opcode::GetCbufU32x2 => {
            let second =
                context.constant_buffer_element_expression(inst_ref, binding, offset, 1)?;
            context.define(
                inst_ref,
                ir::Type::U32x2,
                format!("uint2({word}, {second})"),
                false,
            )
        }
        _ => unreachable!("non-CBUF opcode {:?}", inst.opcode),
    }
}

/// Emit Eden's `EmitWorkgroupId` through Metal's native compute built-in.
pub fn emit_workgroup_id(context: &mut MslEmitContext, inst_ref: InstRef) -> Result<(), MslError> {
    context.define(inst_ref, ir::Type::U32x3, "workgroup_id".to_owned(), false)
}

/// Emit Eden's `EmitLocalInvocationId` through Metal's native compute built-in.
pub fn emit_local_invocation_id(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
) -> Result<(), MslError> {
    context.define(
        inst_ref,
        ir::Type::U32x3,
        "local_invocation_id".to_owned(),
        false,
    )
}

/// InvocationInfo follows Eden: the current input vertex count in bits 16..31.
pub fn emit_invocation_info(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
) -> Result<(), MslError> {
    if matches!(context.stage(), crate::stage::Stage::TessellationControl | crate::stage::Stage::TessellationEval) {
        return context.define(inst_ref, ir::Type::U32, "patch_vertices << 16u".into(), false);
    }
    let vertices = context.geometry_input_vertices()?;
    context.define(
        inst_ref,
        ir::Type::U32,
        format!("{}u", vertices << 16),
        false,
    )
}

pub fn emit_invocation_id(context: &mut MslEmitContext, inst_ref: InstRef) -> Result<(), MslError> {
    if context.stage() == crate::stage::Stage::TessellationControl {
        return context.define(inst_ref, ir::Type::U32, "invocation_id".into(), false);
    }
    context.geometry_input_vertices()?;
    context.define(
        inst_ref,
        ir::Type::U32,
        "geometry_group.x".to_owned(),
        false,
    )
}

/// Emit Eden's `EmitSampleId` through Metal's fragment sample built-in.
pub fn emit_sample_id(context: &mut MslEmitContext, inst_ref: InstRef) -> Result<(), MslError> {
    context.define(inst_ref, ir::Type::U32, "sample_id".to_owned(), false)
}

/// Emit Eden's `EmitRenderArea` through the renderer-populated Metal push
/// constant buffer. The first 16 bytes match `RenderAreaLayout::render_area`.
pub fn emit_render_area(context: &mut MslEmitContext, inst_ref: InstRef) -> Result<(), MslError> {
    context.define(
        inst_ref,
        ir::Type::F32x4,
        context.render_area_expression().to_owned(),
        false,
    )
}

/// Emit Eden's `EmitResolutionDownFactor` from the renderer-populated
/// `RescalingLayout::down_factor` field at byte offset 24.
pub fn emit_resolution_down_factor(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
) -> Result<(), MslError> {
    context.define(
        inst_ref,
        ir::Type::F32,
        context.resolution_down_factor_expression().to_owned(),
        false,
    )
}

/// Emit Eden's `EmitYDirection` from the static pipeline Y-negate state.
pub fn emit_y_direction(context: &mut MslEmitContext, inst_ref: InstRef) -> Result<(), MslError> {
    context.define(
        inst_ref,
        ir::Type::F32,
        context.y_direction_expression().to_owned(),
        false,
    )
}

/// Emit Eden's `EmitIsHelperInvocation` while demote remains unsupported.
pub fn emit_is_helper_invocation(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
) -> Result<(), MslError> {
    context.define(
        inst_ref,
        ir::Type::U1,
        context.helper_invocation_expression().to_owned(),
        false,
    )
}

/// Emit Eden's `EmitLoadLocal`; the IR operand is already a 32-bit word index.
pub fn emit_load_local(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &ir::Inst,
) -> Result<(), MslError> {
    let word_offset = context.value_expression(inst.arg(0), inst_ref, 0)?;
    context.define(
        inst_ref,
        ir::Type::U32,
        format!("lmem[{word_offset}]"),
        false,
    )
}

/// Emit Eden's `EmitWriteLocal`; the IR operand is already a 32-bit word index.
pub fn emit_write_local(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &ir::Inst,
) -> Result<(), MslError> {
    let word_offset = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let value = context.value_expression(inst.arg(1), inst_ref, 1)?;
    context.emit_statement(&format!("lmem[{word_offset}] = {value};"));
    Ok(())
}

/// Emit Eden's generic `GetAttribute` path for vertex/fragment stage inputs.
pub fn emit_get_attribute(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &ir::Inst,
) -> Result<(), MslError> {
    let Value::Attribute(attribute) = inst.arg(0) else {
        return Err(MslError::ExpectedImmediate {
            opcode: inst.opcode,
            arg: 0,
            expected: "attribute",
        });
    };
    if context.stage() == crate::stage::Stage::TessellationControl {
        let vertex = context.value_expression(inst.arg(1), inst_ref, 1)?;
        let expression = context.tessellation_control_layout()?.input_expression(*attribute, &vertex)?;
        return context.define(inst_ref, ir::Type::F32, expression, false);
    }
    if context.stage() == crate::stage::Stage::TessellationEval {
        // Eden reads TessCoord/PrimitiveId directly: the IR vertex operand is
        // relevant only for per-control-point arrays, not these built-ins.
        let vertex = if attribute.is_generic() || attribute.is_position() {
            context.value_expression(inst.arg(1), inst_ref, 1)?
        } else { String::new() };
        let expression = context.tessellation_evaluation_layout()?.input_expression(*attribute, &vertex)?;
        return context.define(inst_ref, ir::Type::F32, expression, false);
    }
    if context.stage() == crate::stage::Stage::Geometry {
        let expression = if *attribute == crate::ir::value::Attribute::PRIMITIVE_ID {
            "as_type<float>(geometry_input.primitive_id)".to_owned()
        } else if attribute.is_generic() || attribute.is_position() {
            let vertex = context.value_expression(inst.arg(1), inst_ref, 1)?;
            context.geometry_input_expression(*attribute, &vertex)
        } else {
            return Err(MslError::UnsupportedAttribute(attribute.0));
        };
        return context.define(inst_ref, ir::Type::F32, expression, false);
    }
    if !matches!(inst.arg(1), Value::ImmU32(0)) {
        return Err(MslError::UnsupportedProgramFeature(
            "per-vertex input indexing",
        ));
    }
    let expression = if attribute.is_generic() {
        context.generic_input_expression(*attribute)
    } else {
        match *attribute {
            crate::ir::value::Attribute::PRIMITIVE_ID => "as_type<float>(primitive_id)".to_owned(),
            crate::ir::value::Attribute::LAYER => "as_type<float>(layer)".to_owned(),
            attribute if attribute.is_position() => {
                let swizzle = ["x", "y", "z", "w"][attribute.position_element() as usize];
                format!("fragment_position.{swizzle}")
            }
            crate::ir::value::Attribute::INSTANCE_ID => {
                if context.support_vertex_instance_id() {
                    "as_type<float>(instance_id)".to_owned()
                } else {
                    "as_type<float>(instance_index - base_instance)".to_owned()
                }
            }
            crate::ir::value::Attribute::VERTEX_ID => {
                if context.support_vertex_instance_id() {
                    "as_type<float>(vertex_id)".to_owned()
                } else {
                    "as_type<float>(vertex_index)".to_owned()
                }
            }
            crate::ir::value::Attribute::BASE_INSTANCE => {
                "as_type<float>(base_instance)".to_owned()
            }
            crate::ir::value::Attribute::BASE_VERTEX => "as_type<float>(base_vertex)".to_owned(),
            crate::ir::value::Attribute::FRONT_FACE => {
                "as_type<float>(front_face ? 0xFFFFFFFFu : 0u)".to_owned()
            }
            crate::ir::value::Attribute::POINT_SPRITE_S => "point_coord.x".to_owned(),
            crate::ir::value::Attribute::POINT_SPRITE_T => "point_coord.y".to_owned(),
            _ => return Err(MslError::UnsupportedAttribute(attribute.0)),
        }
    };
    context.define(inst_ref, ir::Type::F32, expression, false)
}

/// Patch variables belong to the complete patch, unlike invocation-owned
/// per-vertex outputs. Match Eden's generic loads and outer/inner stores.
pub fn emit_patch(context: &mut MslEmitContext, inst_ref: InstRef, inst: &ir::Inst) -> Result<(), MslError> {
    let Value::Patch(patch) = inst.arg(0) else {
        return Err(MslError::ExpectedImmediate { opcode: inst.opcode, arg: 0, expected: "patch" });
    };
    let writing = inst.opcode == Opcode::SetPatch;
    if context.stage() == crate::stage::Stage::TessellationEval {
        if writing {
            return Err(MslError::UnsupportedProgramFeature("patch store outside tessellation control"));
        }
        let expression = context.tessellation_evaluation_layout()?.patch_expression(*patch)?;
        return context.define(inst_ref, ir::Type::F32, expression, false);
    }
    let expression = context.tessellation_control_layout()?.patch_expression(*patch, writing)?;
    if writing {
        let value = context.value_expression(inst.arg(1), inst_ref, 1)?;
        context.emit_statement(&format!("{expression} = {value};"));
        Ok(())
    } else {
        context.define(inst_ref, ir::Type::F32, expression, false)
    }
}

/// Emit Eden's integer system-value attribute path without the float bitcast.
pub fn emit_get_attribute_u32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &ir::Inst,
) -> Result<(), MslError> {
    let Value::Attribute(attribute) = inst.arg(0) else {
        return Err(MslError::ExpectedImmediate {
            opcode: inst.opcode,
            arg: 0,
            expected: "attribute",
        });
    };
    if matches!(context.stage(), crate::stage::Stage::TessellationControl | crate::stage::Stage::TessellationEval) && *attribute == crate::ir::value::Attribute::PRIMITIVE_ID {
        return context.define(inst_ref, ir::Type::U32, "patch_id".into(), false);
    }
    if context.stage() == crate::stage::Stage::Geometry
        && *attribute == crate::ir::value::Attribute::PRIMITIVE_ID
    {
        return context.define(
            inst_ref,
            ir::Type::U32,
            "geometry_input.primitive_id".to_owned(),
            false,
        );
    }
    if !matches!(inst.arg(1), Value::ImmU32(0)) {
        return Err(MslError::UnsupportedProgramFeature(
            "per-vertex input indexing",
        ));
    }
    let expression = match *attribute {
        crate::ir::value::Attribute::PRIMITIVE_ID => "primitive_id".to_owned(),
        crate::ir::value::Attribute::INSTANCE_ID => {
            if context.support_vertex_instance_id() {
                "instance_id".to_owned()
            } else {
                "instance_index - base_instance".to_owned()
            }
        }
        crate::ir::value::Attribute::VERTEX_ID => {
            if context.support_vertex_instance_id() {
                "vertex_id".to_owned()
            } else {
                "vertex_index".to_owned()
            }
        }
        crate::ir::value::Attribute::BASE_INSTANCE => "base_instance".to_owned(),
        crate::ir::value::Attribute::BASE_VERTEX => "base_vertex".to_owned(),
        _ => return Err(MslError::UnsupportedAttribute(attribute.0)),
    };
    context.define(inst_ref, ir::Type::U32, expression, false)
}
