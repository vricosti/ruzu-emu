// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native-MSL shared-memory emission.
//!
//! This follows Eden's `backend/spirv/emit_spirv_shared_memory.cpp` fallback
//! layout: one `threadgroup uint` array, byte offsets converted to word
//! indices, and atomic compare/exchange for subword stores.

use crate::ir::instruction::Inst;
use crate::ir::opcodes::Opcode;
use crate::ir::types::Type;
use crate::ir::value::InstRef;

use super::msl_emit_context::MslEmitContext;
use super::MslError;

fn word_index(
    context: &MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<String, MslError> {
    let offset = context.value_expression(inst.arg(0), inst_ref, 0)?;
    Ok(format!("(({offset}) >> 2u)"))
}

fn bit_offset(
    context: &MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    width: u32,
) -> Result<String, MslError> {
    let offset = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let mask = match width {
        8 => 24,
        16 => 16,
        _ => unreachable!("shared subword width must be 8 or 16"),
    };
    Ok(format!("((({offset}) << 3u) & {mask}u)"))
}

pub fn emit_load_shared(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let index = word_index(context, inst_ref, inst)?;
    let word = format!("smem[{index}]");
    let (ty, expression) = match inst.opcode {
        Opcode::LoadSharedU8 => {
            let bit = bit_offset(context, inst_ref, inst, 8)?;
            (Type::U32, format!("extract_bits({word}, {bit}, 8u)"))
        }
        Opcode::LoadSharedS8 => {
            let bit = bit_offset(context, inst_ref, inst, 8)?;
            (
                Type::U32,
                format!("as_type<uint>(extract_bits(as_type<int>({word}), {bit}, 8u))"),
            )
        }
        Opcode::LoadSharedU16 => {
            let bit = bit_offset(context, inst_ref, inst, 16)?;
            (Type::U32, format!("extract_bits({word}, {bit}, 16u)"))
        }
        Opcode::LoadSharedS16 => {
            let bit = bit_offset(context, inst_ref, inst, 16)?;
            (
                Type::U32,
                format!("as_type<uint>(extract_bits(as_type<int>({word}), {bit}, 16u))"),
            )
        }
        Opcode::LoadSharedU32 => (Type::U32, word),
        Opcode::LoadSharedU64 => (Type::U32x2, format!("uint2({word}, smem[({index}) + 1u])")),
        Opcode::LoadSharedU128 => (
            Type::U32x4,
            format!(
                "uint4({word}, smem[({index}) + 1u], smem[({index}) + 2u], smem[({index}) + 3u])"
            ),
        ),
        _ => unreachable!("not a shared-memory load: {:?}", inst.opcode),
    };
    context.define(inst_ref, ty, expression, false)
}

pub fn emit_write_shared(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let index = word_index(context, inst_ref, inst)?;
    let value = context.value_expression(inst.arg(1), inst_ref, 1)?;
    match inst.opcode {
        Opcode::WriteSharedU8 | Opcode::WriteSharedU16 => {
            let width = if inst.opcode == Opcode::WriteSharedU8 {
                8
            } else {
                16
            };
            let bit = bit_offset(context, inst_ref, inst, width)?;
            context.require_shared_subword_cas();
            context.emit_statement(&format!(
                "spvWriteSharedBits(&smem[{index}], {value}, {bit}, {width}u);"
            ));
        }
        Opcode::WriteSharedU32 => {
            context.emit_statement(&format!("smem[{index}] = {value};"));
        }
        Opcode::WriteSharedU64 => {
            context.emit_statement(&format!("smem[{index}] = ({value}).x;"));
            context.emit_statement(&format!("smem[({index}) + 1u] = ({value}).y;"));
        }
        Opcode::WriteSharedU128 => {
            for (component, swizzle) in ["x", "y", "z", "w"].into_iter().enumerate() {
                context.emit_statement(&format!(
                    "smem[({index}) + {component}u] = ({value}).{swizzle};"
                ));
            }
        }
        _ => unreachable!("not a shared-memory store: {:?}", inst.opcode),
    }
    Ok(())
}
