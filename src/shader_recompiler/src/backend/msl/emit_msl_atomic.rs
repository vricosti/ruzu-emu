// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native-MSL shared and storage-buffer atomic emission.
//!
//! This owns the MSL equivalents of Eden's 32-bit operations in
//! `backend/spirv/emit_spirv_atomic.cpp`. Relaxed ordering matches Eden's
//! zero SPIR-V memory-semantics operand.

use crate::ir::instruction::Inst;
use crate::ir::opcodes::Opcode;
use crate::ir::types::Type;
use crate::ir::value::{InstRef, Value};

use super::msl_emit_context::MslEmitContext;
use super::MslError;

#[derive(Clone, Copy)]
enum AtomicOperation {
    IAdd,
    SMin,
    UMin,
    SMax,
    UMax,
    Inc,
    Dec,
    And,
    Or,
    Xor,
    Exchange,
}

fn operation(opcode: Opcode) -> AtomicOperation {
    match opcode {
        Opcode::SharedAtomicIAdd32 | Opcode::StorageAtomicIAdd32 => AtomicOperation::IAdd,
        Opcode::SharedAtomicSMin32 | Opcode::StorageAtomicSMin32 => AtomicOperation::SMin,
        Opcode::SharedAtomicUMin32 | Opcode::StorageAtomicUMin32 => AtomicOperation::UMin,
        Opcode::SharedAtomicSMax32 | Opcode::StorageAtomicSMax32 => AtomicOperation::SMax,
        Opcode::SharedAtomicUMax32 | Opcode::StorageAtomicUMax32 => AtomicOperation::UMax,
        Opcode::SharedAtomicInc32 | Opcode::StorageAtomicInc32 => AtomicOperation::Inc,
        Opcode::SharedAtomicDec32 | Opcode::StorageAtomicDec32 => AtomicOperation::Dec,
        Opcode::SharedAtomicAnd32 | Opcode::StorageAtomicAnd32 => AtomicOperation::And,
        Opcode::SharedAtomicOr32 | Opcode::StorageAtomicOr32 => AtomicOperation::Or,
        Opcode::SharedAtomicXor32 | Opcode::StorageAtomicXor32 => AtomicOperation::Xor,
        Opcode::SharedAtomicExchange32 | Opcode::StorageAtomicExchange32 => {
            AtomicOperation::Exchange
        }
        _ => unreachable!("not a 32-bit shared/storage atomic: {opcode:?}"),
    }
}

fn atomic_expression(
    context: &mut MslEmitContext,
    operation: AtomicOperation,
    pointer: &str,
    value: &str,
) -> String {
    match operation {
        AtomicOperation::IAdd => {
            format!("atomic_fetch_add_explicit({pointer}, {value}, memory_order_relaxed)")
        }
        AtomicOperation::SMin | AtomicOperation::SMax => {
            unreachable!("signed min/max require an atomic_int pointer")
        }
        AtomicOperation::UMin => {
            format!("atomic_fetch_min_explicit({pointer}, {value}, memory_order_relaxed)")
        }
        AtomicOperation::UMax => {
            format!("atomic_fetch_max_explicit({pointer}, {value}, memory_order_relaxed)")
        }
        AtomicOperation::Inc => {
            context.require_atomic_inc_dec_cas();
            format!("spvAtomicInc({pointer}, {value})")
        }
        AtomicOperation::Dec => {
            context.require_atomic_inc_dec_cas();
            format!("spvAtomicDec({pointer}, {value})")
        }
        AtomicOperation::And => {
            format!("atomic_fetch_and_explicit({pointer}, {value}, memory_order_relaxed)")
        }
        AtomicOperation::Or => {
            format!("atomic_fetch_or_explicit({pointer}, {value}, memory_order_relaxed)")
        }
        AtomicOperation::Xor => {
            format!("atomic_fetch_xor_explicit({pointer}, {value}, memory_order_relaxed)")
        }
        AtomicOperation::Exchange => {
            format!("atomic_exchange_explicit({pointer}, {value}, memory_order_relaxed)")
        }
    }
}

fn signed_pointer(pointer: &str, address_space: &str) -> String {
    format!("reinterpret_cast<{address_space} atomic_int*>({pointer})")
}

fn unsigned_atomic_expression(
    context: &mut MslEmitContext,
    operation: AtomicOperation,
    pointer: &str,
    signed: &str,
    value: &str,
) -> String {
    match operation {
        AtomicOperation::SMin => format!(
            "as_type<uint>(atomic_fetch_min_explicit({}, as_type<int>({value}), memory_order_relaxed))",
            signed_pointer(pointer, signed)
        ),
        AtomicOperation::SMax => format!(
            "as_type<uint>(atomic_fetch_max_explicit({}, as_type<int>({value}), memory_order_relaxed))",
            signed_pointer(pointer, signed)
        ),
        _ => atomic_expression(context, operation, pointer, value),
    }
}

pub fn emit_shared_atomic(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let offset = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let value = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let pointer = format!(
        "reinterpret_cast<threadgroup atomic_uint*>(&smem[(({}) >> 2u)])",
        offset
    );
    let expression = unsigned_atomic_expression(
        context,
        operation(inst.opcode),
        &pointer,
        "threadgroup",
        &value,
    );
    context.define(inst_ref, Type::U32, expression, false)
}

fn immediate_binding(inst: &Inst) -> Result<u32, MslError> {
    match inst.arg(0) {
        Value::ImmU32(binding) => Ok(*binding),
        _ => Err(MslError::ExpectedImmediate {
            opcode: inst.opcode,
            arg: 0,
            expected: "storage-buffer binding",
        }),
    }
}

pub fn emit_storage_atomic(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let binding = immediate_binding(inst)?;
    let word = context.storage_buffer_word_expression(inst_ref, binding, inst.arg(1), 0)?;
    let value = context.value_expression(inst.arg(2), inst_ref, 2)?;
    let pointer = format!("reinterpret_cast<device atomic_uint*>(&{word})");
    let expression =
        unsigned_atomic_expression(context, operation(inst.opcode), &pointer, "device", &value);
    context.define(inst_ref, Type::U32, expression, false)
}
