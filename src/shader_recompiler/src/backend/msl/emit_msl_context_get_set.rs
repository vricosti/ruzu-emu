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

fn immediate_binding(inst: &ir::Inst) -> Result<u32, MslError> {
    match inst.arg(0) {
        Value::ImmU32(binding) => Ok(*binding),
        _ => Err(MslError::ExpectedImmediate {
            opcode: inst.opcode,
            arg: 0,
            expected: "constant-buffer binding",
        }),
    }
}

/// Emit the non-aliasing `uint4` CBUF path used by the Metal profile.
pub fn emit_get_cbuf(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &ir::Inst,
) -> Result<(), MslError> {
    let binding = immediate_binding(inst)?;
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

/// Emit Eden's `EmitSampleId` through Metal's fragment sample built-in.
pub fn emit_sample_id(context: &mut MslEmitContext, inst_ref: InstRef) -> Result<(), MslError> {
    context.define(inst_ref, ir::Type::U32, "sample_id".to_owned(), false)
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
