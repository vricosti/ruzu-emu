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
