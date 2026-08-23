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

use super::msl_emit_context::{MslEmitContext, MslGlobalMemoryResource};
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
        Opcode::SharedAtomicIAdd32 | Opcode::StorageAtomicIAdd32 | Opcode::GlobalAtomicIAdd32 => {
            AtomicOperation::IAdd
        }
        Opcode::SharedAtomicSMin32 | Opcode::StorageAtomicSMin32 | Opcode::GlobalAtomicSMin32 => {
            AtomicOperation::SMin
        }
        Opcode::SharedAtomicUMin32 | Opcode::StorageAtomicUMin32 | Opcode::GlobalAtomicUMin32 => {
            AtomicOperation::UMin
        }
        Opcode::SharedAtomicSMax32 | Opcode::StorageAtomicSMax32 | Opcode::GlobalAtomicSMax32 => {
            AtomicOperation::SMax
        }
        Opcode::SharedAtomicUMax32 | Opcode::StorageAtomicUMax32 | Opcode::GlobalAtomicUMax32 => {
            AtomicOperation::UMax
        }
        Opcode::SharedAtomicInc32 | Opcode::StorageAtomicInc32 | Opcode::GlobalAtomicInc32 => {
            AtomicOperation::Inc
        }
        Opcode::SharedAtomicDec32 | Opcode::StorageAtomicDec32 | Opcode::GlobalAtomicDec32 => {
            AtomicOperation::Dec
        }
        Opcode::SharedAtomicAnd32 | Opcode::StorageAtomicAnd32 | Opcode::GlobalAtomicAnd32 => {
            AtomicOperation::And
        }
        Opcode::SharedAtomicOr32 | Opcode::StorageAtomicOr32 | Opcode::GlobalAtomicOr32 => {
            AtomicOperation::Or
        }
        Opcode::SharedAtomicXor32 | Opcode::StorageAtomicXor32 | Opcode::GlobalAtomicXor32 => {
            AtomicOperation::Xor
        }
        Opcode::SharedAtomicExchange32
        | Opcode::StorageAtomicExchange32
        | Opcode::GlobalAtomicExchange32 => AtomicOperation::Exchange,
        _ => unreachable!("not a 32-bit memory atomic: {opcode:?}"),
    }
}

fn atomic_expression(operation: AtomicOperation, pointer: &str, value: &str) -> String {
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
            format!("spvAtomicInc({pointer}, {value})")
        }
        AtomicOperation::Dec => {
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
        _ => atomic_expression(operation, pointer, value),
    }
}

fn require_atomic_helpers(context: &mut MslEmitContext, operation: AtomicOperation) {
    if matches!(operation, AtomicOperation::Inc | AtomicOperation::Dec) {
        context.require_atomic_inc_dec_cas();
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
    let operation = operation(inst.opcode);
    require_atomic_helpers(context, operation);
    let expression = unsigned_atomic_expression(operation, &pointer, "threadgroup", &value);
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
    let operation = operation(inst.opcode);
    require_atomic_helpers(context, operation);
    let expression = unsigned_atomic_expression(operation, &pointer, "device", &value);
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
    context.require_fp_cas(helper);
    context.define(inst_ref, result_type, expression, false)
}

fn wide_operation_expression(opcode: Opcode, original: &str, value: &str) -> String {
    match opcode {
        Opcode::StorageAtomicIAdd64
        | Opcode::StorageAtomicIAdd32x2
        | Opcode::GlobalAtomicIAdd64
        | Opcode::GlobalAtomicIAdd32x2 => {
            format!("({original}) + ({value})")
        }
        Opcode::StorageAtomicSMin64 | Opcode::GlobalAtomicSMin64 => {
            format!("as_type<ulong>(min(as_type<long>({original}), as_type<long>({value})))")
        }
        Opcode::StorageAtomicSMin32x2 | Opcode::GlobalAtomicSMin32x2 => {
            format!("as_type<uint2>(min(as_type<int2>({original}), as_type<int2>({value})))")
        }
        Opcode::StorageAtomicUMin64
        | Opcode::StorageAtomicUMin32x2
        | Opcode::GlobalAtomicUMin64
        | Opcode::GlobalAtomicUMin32x2 => {
            format!("min({original}, {value})")
        }
        Opcode::StorageAtomicSMax64 | Opcode::GlobalAtomicSMax64 => {
            format!("as_type<ulong>(max(as_type<long>({original}), as_type<long>({value})))")
        }
        Opcode::StorageAtomicSMax32x2 | Opcode::GlobalAtomicSMax32x2 => {
            format!("as_type<uint2>(max(as_type<int2>({original}), as_type<int2>({value})))")
        }
        Opcode::StorageAtomicUMax64
        | Opcode::StorageAtomicUMax32x2
        | Opcode::GlobalAtomicUMax64
        | Opcode::GlobalAtomicUMax32x2 => {
            format!("max({original}, {value})")
        }
        Opcode::StorageAtomicAnd64
        | Opcode::StorageAtomicAnd32x2
        | Opcode::GlobalAtomicAnd64
        | Opcode::GlobalAtomicAnd32x2 => {
            format!("({original}) & ({value})")
        }
        Opcode::StorageAtomicOr64
        | Opcode::StorageAtomicOr32x2
        | Opcode::GlobalAtomicOr64
        | Opcode::GlobalAtomicOr32x2 => {
            format!("({original}) | ({value})")
        }
        Opcode::StorageAtomicXor64
        | Opcode::StorageAtomicXor32x2
        | Opcode::GlobalAtomicXor64
        | Opcode::GlobalAtomicXor32x2 => {
            format!("({original}) ^ ({value})")
        }
        Opcode::StorageAtomicExchange64
        | Opcode::StorageAtomicExchange32x2
        | Opcode::GlobalAtomicExchange64
        | Opcode::GlobalAtomicExchange32x2 => value.to_owned(),
        _ => unreachable!("not a wide memory atomic fallback: {opcode:?}"),
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

fn is_global_atomic_u32(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::GlobalAtomicIAdd32
            | Opcode::GlobalAtomicSMin32
            | Opcode::GlobalAtomicUMin32
            | Opcode::GlobalAtomicSMax32
            | Opcode::GlobalAtomicUMax32
            | Opcode::GlobalAtomicInc32
            | Opcode::GlobalAtomicDec32
            | Opcode::GlobalAtomicAnd32
            | Opcode::GlobalAtomicOr32
            | Opcode::GlobalAtomicXor32
            | Opcode::GlobalAtomicExchange32
    )
}

fn is_global_atomic_u64(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::GlobalAtomicIAdd64
            | Opcode::GlobalAtomicSMin64
            | Opcode::GlobalAtomicUMin64
            | Opcode::GlobalAtomicSMax64
            | Opcode::GlobalAtomicUMax64
            | Opcode::GlobalAtomicAnd64
            | Opcode::GlobalAtomicOr64
            | Opcode::GlobalAtomicXor64
            | Opcode::GlobalAtomicExchange64
    )
}

fn is_global_atomic_u32x2(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::GlobalAtomicIAdd32x2
            | Opcode::GlobalAtomicSMin32x2
            | Opcode::GlobalAtomicUMin32x2
            | Opcode::GlobalAtomicSMax32x2
            | Opcode::GlobalAtomicUMax32x2
            | Opcode::GlobalAtomicAnd32x2
            | Opcode::GlobalAtomicOr32x2
            | Opcode::GlobalAtomicXor32x2
            | Opcode::GlobalAtomicExchange32x2
    )
}

fn is_global_atomic_fp(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::GlobalAtomicAddF32
            | Opcode::GlobalAtomicAddF16x2
            | Opcode::GlobalAtomicAddF32x2
            | Opcode::GlobalAtomicMinF16x2
            | Opcode::GlobalAtomicMinF32x2
            | Opcode::GlobalAtomicMaxF16x2
            | Opcode::GlobalAtomicMaxF32x2
    )
}

fn global_atomic_name(opcode: Opcode) -> String {
    format!("spv{}", opcode.name())
}

fn global_atomic_result_type(opcode: Opcode) -> Type {
    if is_global_atomic_u64(opcode) {
        Type::U64
    } else if is_global_atomic_u32x2(opcode) {
        Type::U32x2
    } else if opcode == Opcode::GlobalAtomicAddF32 {
        Type::F32
    } else {
        Type::U32
    }
}

fn global_atomic_value_type(opcode: Opcode) -> &'static str {
    if is_global_atomic_u32(opcode) {
        "uint"
    } else if is_global_atomic_u64(opcode) {
        "ulong"
    } else if is_global_atomic_u32x2(opcode) {
        "uint2"
    } else {
        match opcode {
            Opcode::GlobalAtomicAddF32 => "float",
            Opcode::GlobalAtomicAddF16x2
            | Opcode::GlobalAtomicMinF16x2
            | Opcode::GlobalAtomicMaxF16x2 => "half2",
            Opcode::GlobalAtomicAddF32x2
            | Opcode::GlobalAtomicMinF32x2
            | Opcode::GlobalAtomicMaxF32x2 => "float2",
            _ => unreachable!("not a global atomic: {opcode:?}"),
        }
    }
}

pub fn emit_global_atomic(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let address = context.value_expression(inst.arg(0), inst_ref, 0)?;
    let address = if is_global_atomic_u32x2(inst.opcode) {
        format!("as_type<ulong>({address})")
    } else {
        address
    };
    let value = context.value_expression(inst.arg(1), inst_ref, 1)?;
    if is_global_atomic_u32(inst.opcode) {
        require_atomic_helpers(context, operation(inst.opcode));
    }
    if is_global_atomic_fp(inst.opcode) {
        let helper = match inst.opcode {
            Opcode::GlobalAtomicAddF32 => "spvAtomicAddF32",
            Opcode::GlobalAtomicAddF16x2 => "spvAtomicAddF16x2",
            Opcode::GlobalAtomicAddF32x2 => "spvAtomicAddF32x2",
            Opcode::GlobalAtomicMinF16x2 => "spvAtomicMinF16x2",
            Opcode::GlobalAtomicMinF32x2 => "spvAtomicMinF32x2",
            Opcode::GlobalAtomicMaxF16x2 => "spvAtomicMaxF16x2",
            Opcode::GlobalAtomicMaxF32x2 => "spvAtomicMaxF32x2",
            _ => unreachable!(),
        };
        context.require_fp_cas(helper);
    }
    context.require_global_atomic_helper(inst.opcode);
    let expression =
        context.global_memory_call(&global_atomic_name(inst.opcode), &address, Some(&value));
    context.define(
        inst_ref,
        global_atomic_result_type(inst.opcode),
        expression,
        false,
    )
}

fn global_atomic_parameters(resources: &[MslGlobalMemoryResource], value_type: &str) -> String {
    let mut parameters = vec!["ulong address".to_owned(), format!("{value_type} value")];
    for index in 0..resources.len() {
        parameters.push(format!("constant uint4* global_cbuf{index}"));
        parameters.push(format!("device uint* global_ssbo{index}"));
    }
    parameters.join(", ")
}

fn define_global_route(
    source: &mut String,
    resource: &MslGlobalMemoryResource,
    resource_index: usize,
    alignment: u64,
    shift: u32,
) {
    let cbuf = format!("global_cbuf{resource_index}");
    let low = MslEmitContext::global_cbuf_word(&cbuf, resource.cbuf_offset);
    let high = MslEmitContext::global_cbuf_word(&cbuf, resource.cbuf_offset + 4);
    let size = MslEmitContext::global_cbuf_word(&cbuf, resource.cbuf_offset + 8);
    let alignment_mask = !alignment.wrapping_sub(1);
    source.push_str("    {\n");
    source.push_str(&format!(
        "        const ulong ssbo_address = as_type<ulong>(uint2({low}, {high})) & 0x{alignment_mask:016X}ul;\n"
    ));
    source.push_str(&format!(
        "        const ulong ssbo_end = ssbo_address + ulong({size});\n"
    ));
    source.push_str("        if (address >= ssbo_address && address < ssbo_end) {\n");
    source.push_str(&format!(
        "            const uint element = uint(address - ssbo_address) >> {shift}u;\n"
    ));
}

fn end_global_route(source: &mut String) {
    source.push_str("        }\n    }\n");
}

fn define_global_atomic_u32(
    source: &mut String,
    resources: &[MslGlobalMemoryResource],
    alignment: u64,
    opcode: Opcode,
) {
    let name = global_atomic_name(opcode);
    source.push_str(&format!(
        "inline uint {name}({}) {{\n",
        global_atomic_parameters(resources, "uint")
    ));
    for (index, resource) in resources.iter().enumerate() {
        define_global_route(source, resource, index, alignment, 2);
        source.push_str(&format!(
            "            device atomic_uint* pointer = reinterpret_cast<device atomic_uint*>(&global_ssbo{index}[element]);\n"
        ));
        let expression =
            unsigned_atomic_expression(operation(opcode), "pointer", "device", "value");
        source.push_str(&format!("            return {expression};\n"));
        end_global_route(source);
    }
    source.push_str("    return 0u;\n}\n\n");
}

fn define_global_atomic_wide(
    source: &mut String,
    resources: &[MslGlobalMemoryResource],
    alignment: u64,
    opcode: Opcode,
) {
    let vector = is_global_atomic_u32x2(opcode);
    let value_type = if vector { "uint2" } else { "ulong" };
    let name = global_atomic_name(opcode);
    source.push_str(&format!(
        "inline {value_type} {name}({}) {{\n",
        global_atomic_parameters(resources, value_type)
    ));
    for (index, resource) in resources.iter().enumerate() {
        define_global_route(source, resource, index, alignment, 3);
        source.push_str(&format!(
            "            const uint base_word = element * 2u;\n            uint2 original_words = uint2(global_ssbo{index}[base_word], global_ssbo{index}[base_word + 1u]);\n"
        ));
        let original = if vector {
            "original_words"
        } else {
            source.push_str("            ulong original = as_type<ulong>(original_words);\n");
            "original"
        };
        let expression = wide_operation_expression(opcode, original, "value");
        source.push_str(&format!(
            "            {value_type} result = {expression};\n            uint2 result_words = as_type<uint2>(result);\n            global_ssbo{index}[base_word] = result_words.x;\n            global_ssbo{index}[base_word + 1u] = result_words.y;\n            return {original};\n"
        ));
        end_global_route(source);
    }
    source.push_str(&format!("    return {value_type}(0);\n}}\n\n"));
}

fn global_fp_helper(opcode: Opcode) -> (&'static str, &'static str) {
    match opcode {
        Opcode::GlobalAtomicAddF32 => ("float", "spvAtomicAddF32(pointer, value)"),
        Opcode::GlobalAtomicAddF16x2 => {
            ("uint", "as_type<uint>(spvAtomicAddF16x2(pointer, value))")
        }
        Opcode::GlobalAtomicAddF32x2 => (
            "uint",
            "as_type<uint>(half2(spvAtomicAddF32x2(pointer, value)))",
        ),
        Opcode::GlobalAtomicMinF16x2 => {
            ("uint", "as_type<uint>(spvAtomicMinF16x2(pointer, value))")
        }
        Opcode::GlobalAtomicMinF32x2 => (
            "uint",
            "as_type<uint>(half2(spvAtomicMinF32x2(pointer, value)))",
        ),
        Opcode::GlobalAtomicMaxF16x2 => {
            ("uint", "as_type<uint>(spvAtomicMaxF16x2(pointer, value))")
        }
        Opcode::GlobalAtomicMaxF32x2 => (
            "uint",
            "as_type<uint>(half2(spvAtomicMaxF32x2(pointer, value)))",
        ),
        _ => unreachable!("not a floating-point global atomic: {opcode:?}"),
    }
}

fn define_global_atomic_fp(
    source: &mut String,
    resources: &[MslGlobalMemoryResource],
    alignment: u64,
    opcode: Opcode,
) {
    let (result_type, expression) = global_fp_helper(opcode);
    let value_type = global_atomic_value_type(opcode);
    let name = global_atomic_name(opcode);
    source.push_str(&format!(
        "inline {result_type} {name}({}) {{\n",
        global_atomic_parameters(resources, value_type)
    ));
    for (index, resource) in resources.iter().enumerate() {
        define_global_route(source, resource, index, alignment, 2);
        source.push_str(&format!(
            "            device atomic_uint* pointer = reinterpret_cast<device atomic_uint*>(&global_ssbo{index}[element]);\n            return {expression};\n"
        ));
        end_global_route(source);
    }
    let zero = if result_type == "float" { "0.0f" } else { "0u" };
    source.push_str(&format!("    return {zero};\n}}\n\n"));
}

pub(super) fn define_global_atomic_functions(
    source: &mut String,
    resources: &[MslGlobalMemoryResource],
    alignment: u64,
    opcodes: &[Opcode],
) {
    for &opcode in opcodes {
        if is_global_atomic_u32(opcode) {
            define_global_atomic_u32(source, resources, alignment, opcode);
        } else if is_global_atomic_u64(opcode) || is_global_atomic_u32x2(opcode) {
            define_global_atomic_wide(source, resources, alignment, opcode);
        } else if is_global_atomic_fp(opcode) {
            define_global_atomic_fp(source, resources, alignment, opcode);
        } else {
            unreachable!("not a global atomic helper: {opcode:?}");
        }
    }
}
