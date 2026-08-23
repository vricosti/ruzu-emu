// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native-MSL storage memory operations.
//!
//! This file owns the MSL equivalents of Eden's
//! `backend/spirv/emit_spirv_memory.cpp` storage-buffer operations.

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
            expected: "storage-buffer binding",
        }),
    }
}

pub fn emit_load_storage(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &ir::Inst,
) -> Result<(), MslError> {
    let binding = immediate_binding(inst)?;
    let offset = inst.arg(1);
    let word = context.storage_buffer_word_expression(inst_ref, binding, offset, 0)?;
    match inst.opcode {
        Opcode::LoadStorageU8
        | Opcode::LoadStorageS8
        | Opcode::LoadStorageU16
        | Opcode::LoadStorageS16 => {
            let (width, signed) = match inst.opcode {
                Opcode::LoadStorageU8 => (8, false),
                Opcode::LoadStorageS8 => (8, true),
                Opcode::LoadStorageU16 => (16, false),
                Opcode::LoadStorageS16 => (16, true),
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
        Opcode::LoadStorage32 => context.define(inst_ref, ir::Type::U32, word, false),
        Opcode::LoadStorage64 => {
            let second = context.storage_buffer_word_expression(inst_ref, binding, offset, 1)?;
            context.define(
                inst_ref,
                ir::Type::U32x2,
                format!("uint2({word}, {second})"),
                false,
            )
        }
        Opcode::LoadStorage128 => {
            let words = (0..4)
                .map(|word_offset| {
                    context.storage_buffer_word_expression(inst_ref, binding, offset, word_offset)
                })
                .collect::<Result<Vec<_>, _>>()?;
            context.define(
                inst_ref,
                ir::Type::U32x4,
                format!(
                    "uint4({}, {}, {}, {})",
                    words[0], words[1], words[2], words[3]
                ),
                false,
            )
        }
        _ => unreachable!("non-storage load opcode {:?}", inst.opcode),
    }
}

pub fn emit_write_storage(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &ir::Inst,
) -> Result<(), MslError> {
    let binding = immediate_binding(inst)?;
    let offset = inst.arg(1);
    let value = context.value_expression(inst.arg(2), inst_ref, 2)?;
    match inst.opcode {
        Opcode::WriteStorageU8
        | Opcode::WriteStorageS8
        | Opcode::WriteStorageU16
        | Opcode::WriteStorageS16 => {
            let width = match inst.opcode {
                Opcode::WriteStorageU8 | Opcode::WriteStorageS8 => 8,
                Opcode::WriteStorageU16 | Opcode::WriteStorageS16 => 16,
                _ => unreachable!(),
            };
            let word = context.storage_buffer_word_expression(inst_ref, binding, offset, 0)?;
            let bit_offset = context.bit_offset_expression(inst_ref, offset, width)?;
            context.require_storage_subword_cas();
            context.emit_statement(&format!(
                "spvWriteStorageBits(&{word}, {value}, {bit_offset}, {width}u);"
            ));
        }
        Opcode::WriteStorage32 => {
            let word = context.storage_buffer_word_expression(inst_ref, binding, offset, 0)?;
            context.emit_statement(&format!("{word} = {value};"));
        }
        Opcode::WriteStorage64 | Opcode::WriteStorage128 => {
            let count = if inst.opcode == Opcode::WriteStorage64 {
                2
            } else {
                4
            };
            let components = ["x", "y", "z", "w"];
            for word_offset in 0..count {
                let word = context.storage_buffer_word_expression(
                    inst_ref,
                    binding,
                    offset,
                    word_offset,
                )?;
                context.emit_statement(&format!(
                    "{word} = {value}.{};",
                    components[word_offset as usize]
                ));
            }
        }
        _ => unreachable!("non-storage write opcode {:?}", inst.opcode),
    }
    Ok(())
}

pub fn emit_load_global(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &ir::Inst,
) -> Result<(), MslError> {
    let (function, result_type) = match inst.opcode {
        Opcode::LoadGlobalU8 => ("spvLoadGlobalU8", ir::Type::U32),
        Opcode::LoadGlobalS8 => ("spvLoadGlobalS8", ir::Type::U32),
        Opcode::LoadGlobalU16 => ("spvLoadGlobalU16", ir::Type::U32),
        Opcode::LoadGlobalS16 => ("spvLoadGlobalS16", ir::Type::U32),
        Opcode::LoadGlobal32 => ("spvLoadGlobal32", ir::Type::U32),
        Opcode::LoadGlobal64 => ("spvLoadGlobal64", ir::Type::U32x2),
        Opcode::LoadGlobal128 => ("spvLoadGlobal128", ir::Type::U32x4),
        _ => unreachable!("non-global load opcode {:?}", inst.opcode),
    };
    if matches!(
        inst.opcode,
        Opcode::LoadGlobalU8 | Opcode::LoadGlobalS8 | Opcode::LoadGlobalU16 | Opcode::LoadGlobalS16
    ) {
        context.require_global_subword_loads();
    }
    let address = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let expression = context.global_memory_call(function, &address, None);
    context.define(inst_ref, result_type, expression, false)
}

pub fn emit_write_global(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &ir::Inst,
) -> Result<(), MslError> {
    let function = match inst.opcode {
        Opcode::WriteGlobalU8 => "spvWriteGlobalU8",
        Opcode::WriteGlobalS8 => "spvWriteGlobalS8",
        Opcode::WriteGlobalU16 => "spvWriteGlobalU16",
        Opcode::WriteGlobalS16 => "spvWriteGlobalS16",
        Opcode::WriteGlobal32 => "spvWriteGlobal32",
        Opcode::WriteGlobal64 => "spvWriteGlobal64",
        Opcode::WriteGlobal128 => "spvWriteGlobal128",
        _ => unreachable!("non-global write opcode {:?}", inst.opcode),
    };
    if matches!(
        inst.opcode,
        Opcode::WriteGlobalU8
            | Opcode::WriteGlobalS8
            | Opcode::WriteGlobalU16
            | Opcode::WriteGlobalS16
    ) {
        context.require_global_subword_writes();
    }
    let address = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let value = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let call = context.global_memory_call(function, &address, Some(&value));
    context.emit_statement(&format!("{call};"));
    Ok(())
}
