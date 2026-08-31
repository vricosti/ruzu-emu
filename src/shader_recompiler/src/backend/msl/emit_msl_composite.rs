// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! MSL vector-composite emission.
//!
//! This file owns the native-MSL counterparts of Eden's
//! `backend/spirv/emit_spirv_composite.cpp` operations.

use crate::ir::instruction::Inst;
use crate::ir::types::Type;
use crate::ir::value::{InstRef, Value};

use super::msl_emit_context::MslEmitContext;
use super::MslError;

fn component_count(ty: Type) -> Result<usize, MslError> {
    match ty {
        Type::U32x2 | Type::F16x2 | Type::F32x2 => Ok(2),
        Type::U32x3 | Type::F16x3 | Type::F32x3 => Ok(3),
        Type::U32x4 | Type::F16x4 | Type::F32x4 => Ok(4),
        _ => Err(MslError::UnsupportedType(ty)),
    }
}

pub fn emit_construct(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let ty = inst.return_type();
    let components = (0..component_count(ty)?)
        .map(|arg| context.value_expression(inst.arg(arg), inst_ref, arg as u32))
        .collect::<Result<Vec<_>, _>>()?;
    context.define(
        inst_ref,
        ty,
        format!(
            "{}({})",
            MslEmitContext::type_name(ty)?,
            components.join(", ")
        ),
        false,
    )
}

pub fn emit_extract(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let composite = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let Value::ImmU32(index) = inst.arg(1) else {
        return Err(MslError::ExpectedImmediate {
            opcode: inst.opcode,
            arg: 1,
            expected: "u32 composite index",
        });
    };
    context.define(
        inst_ref,
        inst.return_type(),
        format!("({composite})[{index}u]"),
        false,
    )
}

pub fn emit_insert(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let ty = inst.return_type();
    let composite = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let object = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let Value::ImmU32(index) = inst.arg(2) else {
        return Err(MslError::ExpectedImmediate {
            opcode: inst.opcode,
            arg: 2,
            expected: "u32 composite index",
        });
    };
    let count = component_count(ty)?;
    if *index as usize >= count {
        return Err(MslError::UnsupportedProgramFeature(
            "out-of-range composite insertion",
        ));
    }
    let components = (0..count)
        .map(|component| {
            if component == *index as usize {
                object.clone()
            } else {
                format!("({composite})[{component}u]")
            }
        })
        .collect::<Vec<_>>();
    context.define(
        inst_ref,
        ty,
        format!(
            "{}({})",
            MslEmitContext::type_name(ty)?,
            components.join(", ")
        ),
        false,
    )
}
