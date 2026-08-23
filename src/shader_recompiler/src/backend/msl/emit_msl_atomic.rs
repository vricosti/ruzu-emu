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

pub fn emit_storage_atomic_fp(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let binding = immediate_binding(inst)?;
    let word = context.storage_buffer_word_expression(inst_ref, binding, inst.arg(1), 0)?;
    let value = context.value_expression(inst.arg(2), inst_ref, 2)?;
    let (helper, result_type, expression) = match inst.opcode {
        Opcode::StorageAtomicAddF32 => (
            "spvAtomicAddF32",
            Type::F32,
            format!("spvAtomicAddF32(reinterpret_cast<device atomic_uint*>(&{word}), {value})"),
        ),
        Opcode::StorageAtomicAddF16x2 => (
            "spvAtomicAddF16x2",
            Type::U32,
            format!(
                "as_type<uint>(spvAtomicAddF16x2(reinterpret_cast<device atomic_uint*>(&{word}), {value}))"
            ),
        ),
        Opcode::StorageAtomicAddF32x2 => (
            "spvAtomicAddF32x2",
            Type::U32,
            format!(
                "as_type<uint>(half2(spvAtomicAddF32x2(reinterpret_cast<device atomic_uint*>(&{word}), {value})))"
            ),
        ),
        Opcode::StorageAtomicMinF16x2 => (
            "spvAtomicMinF16x2",
            Type::U32,
            format!(
                "as_type<uint>(spvAtomicMinF16x2(reinterpret_cast<device atomic_uint*>(&{word}), {value}))"
            ),
        ),
        Opcode::StorageAtomicMinF32x2 => (
            "spvAtomicMinF32x2",
            Type::U32,
            format!(
                "as_type<uint>(half2(spvAtomicMinF32x2(reinterpret_cast<device atomic_uint*>(&{word}), {value})))"
            ),
        ),
        Opcode::StorageAtomicMaxF16x2 => (
            "spvAtomicMaxF16x2",
            Type::U32,
            format!(
                "as_type<uint>(spvAtomicMaxF16x2(reinterpret_cast<device atomic_uint*>(&{word}), {value}))"
            ),
        ),
        Opcode::StorageAtomicMaxF32x2 => (
            "spvAtomicMaxF32x2",
            Type::U32,
            format!(
                "as_type<uint>(half2(spvAtomicMaxF32x2(reinterpret_cast<device atomic_uint*>(&{word}), {value})))"
            ),
        ),
        _ => unreachable!("not a floating-point storage atomic: {:?}", inst.opcode),
    };
    context.require_storage_fp_cas(helper);
    context.define(inst_ref, result_type, expression, false)
}

fn wide_operation_expression(opcode: Opcode, original: &str, value: &str) -> String {
    match opcode {
        Opcode::StorageAtomicIAdd64 | Opcode::StorageAtomicIAdd32x2 => {
            format!("({original}) + ({value})")
        }
        Opcode::StorageAtomicSMin64 => {
            format!("as_type<ulong>(min(as_type<long>({original}), as_type<long>({value})))")
        }
        Opcode::StorageAtomicSMin32x2 => {
            format!("as_type<uint2>(min(as_type<int2>({original}), as_type<int2>({value})))")
        }
        Opcode::StorageAtomicUMin64 | Opcode::StorageAtomicUMin32x2 => {
            format!("min({original}, {value})")
        }
        Opcode::StorageAtomicSMax64 => {
            format!("as_type<ulong>(max(as_type<long>({original}), as_type<long>({value})))")
        }
        Opcode::StorageAtomicSMax32x2 => {
            format!("as_type<uint2>(max(as_type<int2>({original}), as_type<int2>({value})))")
        }
        Opcode::StorageAtomicUMax64 | Opcode::StorageAtomicUMax32x2 => {
            format!("max({original}, {value})")
        }
        Opcode::StorageAtomicAnd64 | Opcode::StorageAtomicAnd32x2 => {
            format!("({original}) & ({value})")
        }
        Opcode::StorageAtomicOr64 | Opcode::StorageAtomicOr32x2 => {
            format!("({original}) | ({value})")
        }
        Opcode::StorageAtomicXor64 | Opcode::StorageAtomicXor32x2 => {
            format!("({original}) ^ ({value})")
        }
        Opcode::StorageAtomicExchange64 | Opcode::StorageAtomicExchange32x2 => value.to_owned(),
        _ => unreachable!("not a wide storage atomic fallback: {opcode:?}"),
    }
}

pub fn emit_shared_atomic_wide_fallback(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let offset = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let value = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let prefix = format!("spv_shared_wide_{}_{}", inst_ref.block, inst_ref.inst);
    let words = format!("{prefix}_words");
    let replacement = format!("{prefix}_replacement");
    context.emit_statement(&format!(
        "uint2 {words} = uint2(smem[((({offset}) >> 2u))], smem[((({offset}) >> 2u)) + 1u]);"
    ));
    let (result_type, original) = match inst.opcode {
        Opcode::SharedAtomicExchange64 => {
            let original = format!("{prefix}_original");
            context.emit_statement(&format!("ulong {original} = as_type<ulong>({words});"));
            context.emit_statement(&format!("uint2 {replacement} = as_type<uint2>({value});"));
            (Type::U64, original)
        }
        Opcode::SharedAtomicExchange32x2 => {
            context.emit_statement(&format!("uint2 {replacement} = {value};"));
            (Type::U32x2, words)
        }
        _ => unreachable!("not a wide shared exchange fallback: {:?}", inst.opcode),
    };
    context.emit_statement(&format!("smem[((({offset}) >> 2u))] = {replacement}.x;"));
    context.emit_statement(&format!(
        "smem[((({offset}) >> 2u)) + 1u] = {replacement}.y;"
    ));
    context.define(inst_ref, result_type, original, false)
}

pub fn emit_storage_atomic_wide_fallback(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    // The SPIR-V backend gates this fallback on descriptor aliasing because it
    // needs both u64 and u32x2 typed views of one descriptor. MSL storage
    // buffers are already emitted as raw device uint arrays, so the same
    // two-word fallback has no typed-descriptor aliasing prerequisite.
    let binding = immediate_binding(inst)?;
    let low = context.storage_buffer_word_expression(inst_ref, binding, inst.arg(1), 0)?;
    let high = context.storage_buffer_word_expression(inst_ref, binding, inst.arg(1), 1)?;
    let value = context.value_expression(inst.arg(2), inst_ref, 2)?;
    let prefix = format!("spv_storage_wide_{}_{}", inst_ref.block, inst_ref.inst);
    let words = format!("{prefix}_words");
    let original = format!("{prefix}_original");
    let result = format!("{prefix}_result");
    let result_words = format!("{prefix}_result_words");
    context.emit_statement(&format!("uint2 {words} = uint2({low}, {high});"));
    let (result_type, original_expression) = match inst.opcode {
        Opcode::StorageAtomicIAdd64
        | Opcode::StorageAtomicSMin64
        | Opcode::StorageAtomicUMin64
        | Opcode::StorageAtomicSMax64
        | Opcode::StorageAtomicUMax64
        | Opcode::StorageAtomicAnd64
        | Opcode::StorageAtomicOr64
        | Opcode::StorageAtomicXor64
        | Opcode::StorageAtomicExchange64 => {
            context.emit_statement(&format!("ulong {original} = as_type<ulong>({words});"));
            let expression = wide_operation_expression(inst.opcode, &original, &value);
            context.emit_statement(&format!("ulong {result} = {expression};"));
            (Type::U64, original)
        }
        Opcode::StorageAtomicIAdd32x2
        | Opcode::StorageAtomicSMin32x2
        | Opcode::StorageAtomicUMin32x2
        | Opcode::StorageAtomicSMax32x2
        | Opcode::StorageAtomicUMax32x2
        | Opcode::StorageAtomicAnd32x2
        | Opcode::StorageAtomicOr32x2
        | Opcode::StorageAtomicXor32x2
        | Opcode::StorageAtomicExchange32x2 => {
            let expression = wide_operation_expression(inst.opcode, &words, &value);
            context.emit_statement(&format!("uint2 {result} = {expression};"));
            (Type::U32x2, words.clone())
        }
        _ => unreachable!("not a wide storage atomic fallback: {:?}", inst.opcode),
    };
    context.emit_statement(&format!("uint2 {result_words} = as_type<uint2>({result});"));
    context.emit_statement(&format!("{low} = {result_words}.x;"));
    context.emit_statement(&format!("{high} = {result_words}.y;"));
    context.define(inst_ref, result_type, original_expression, false)
}
