// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SPIR-V atomic operation emission — maps to upstream
//! `backend/spirv/emit_spirv_atomic.cpp`.
//!
//! Handles atomic operations on shared memory and storage buffers (SSBOs).

use super::spirv_emit_context::{SpirvEmitContext, StorageDefinitionKind};
use crate::ir::{self, Opcode};
use rspirv::spirv::{self, Word};

/// Get device scope and relaxed semantics for atomic operations.
///
/// Matches upstream `AtomicArgs(EmitContext&)`.
fn atomic_args(ctx: &mut SpirvEmitContext) -> (Word, Word) {
    let scope = ctx.constant_u32(spirv::Scope::Device as u32);
    let semantics = ctx.const_zero_u32;
    (scope, semantics)
}

fn shared_pointer(ctx: &mut SpirvEmitContext, offset: Word) -> Word {
    shared_pointer_at(ctx, offset, 0)
}

fn shared_pointer_at(ctx: &mut SpirvEmitContext, offset: Word, index_offset: u32) -> Word {
    let shift = ctx.constant_u32(2);
    let mut index = ctx
        .builder
        .shift_right_arithmetic(ctx.u32_type, None, offset, shift)
        .unwrap();
    if index_offset > 0 {
        let index_offset = ctx.constant_u32(index_offset);
        index = ctx
            .builder
            .i_add(ctx.u32_type, None, index, index_offset)
            .unwrap();
    }
    let indices = if ctx.uses_explicit_workgroup_layout {
        vec![ctx.const_zero_u32, index]
    } else {
        vec![index]
    };
    ctx.builder
        .access_chain(ctx.shared_u32, None, ctx.shared_memory_u32, indices)
        .unwrap()
}

pub fn emit_shared_atomic_iadd_32(ctx: &mut SpirvEmitContext, offset: Word, value: Word) -> Word {
    let pointer = shared_pointer(ctx, offset);
    let (scope, semantics) = atomic_args(ctx);
    ctx.builder
        .atomic_i_add(ctx.u32_type, None, pointer, scope, semantics, value)
        .unwrap()
}

pub fn emit_shared_atomic_smin_32(ctx: &mut SpirvEmitContext, offset: Word, value: Word) -> Word {
    let pointer = shared_pointer(ctx, offset);
    let (scope, semantics) = atomic_args(ctx);
    ctx.builder
        .atomic_s_min(ctx.u32_type, None, pointer, scope, semantics, value)
        .unwrap()
}

pub fn emit_shared_atomic_smax_32(ctx: &mut SpirvEmitContext, offset: Word, value: Word) -> Word {
    let pointer = shared_pointer(ctx, offset);
    let (scope, semantics) = atomic_args(ctx);
    ctx.builder
        .atomic_s_max(ctx.u32_type, None, pointer, scope, semantics, value)
        .unwrap()
}

pub fn emit_shared_atomic_umin_32(ctx: &mut SpirvEmitContext, offset: Word, value: Word) -> Word {
    let pointer = shared_pointer(ctx, offset);
    let (scope, semantics) = atomic_args(ctx);
    ctx.builder
        .atomic_u_min(ctx.u32_type, None, pointer, scope, semantics, value)
        .unwrap()
}

pub fn emit_shared_atomic_umax_32(ctx: &mut SpirvEmitContext, offset: Word, value: Word) -> Word {
    let pointer = shared_pointer(ctx, offset);
    let (scope, semantics) = atomic_args(ctx);
    ctx.builder
        .atomic_u_max(ctx.u32_type, None, pointer, scope, semantics, value)
        .unwrap()
}

pub fn emit_shared_atomic_inc_32(ctx: &mut SpirvEmitContext, offset: Word, value: Word) -> Word {
    let shift = ctx.constant_u32(2);
    let index = ctx
        .builder
        .shift_right_arithmetic(ctx.u32_type, None, offset, shift)
        .unwrap();
    ctx.builder
        .function_call(
            ctx.u32_type,
            None,
            ctx.increment_cas_shared,
            vec![index, value],
        )
        .unwrap()
}

pub fn emit_shared_atomic_dec_32(ctx: &mut SpirvEmitContext, offset: Word, value: Word) -> Word {
    let shift = ctx.constant_u32(2);
    let index = ctx
        .builder
        .shift_right_arithmetic(ctx.u32_type, None, offset, shift)
        .unwrap();
    ctx.builder
        .function_call(
            ctx.u32_type,
            None,
            ctx.decrement_cas_shared,
            vec![index, value],
        )
        .unwrap()
}

pub fn emit_shared_atomic_and_32(ctx: &mut SpirvEmitContext, offset: Word, value: Word) -> Word {
    let pointer = shared_pointer(ctx, offset);
    let (scope, semantics) = atomic_args(ctx);
    ctx.builder
        .atomic_and(ctx.u32_type, None, pointer, scope, semantics, value)
        .unwrap()
}

pub fn emit_shared_atomic_or_32(ctx: &mut SpirvEmitContext, offset: Word, value: Word) -> Word {
    let pointer = shared_pointer(ctx, offset);
    let (scope, semantics) = atomic_args(ctx);
    ctx.builder
        .atomic_or(ctx.u32_type, None, pointer, scope, semantics, value)
        .unwrap()
}

pub fn emit_shared_atomic_xor_32(ctx: &mut SpirvEmitContext, offset: Word, value: Word) -> Word {
    let pointer = shared_pointer(ctx, offset);
    let (scope, semantics) = atomic_args(ctx);
    ctx.builder
        .atomic_xor(ctx.u32_type, None, pointer, scope, semantics, value)
        .unwrap()
}

pub fn emit_shared_atomic_exchange_32(
    ctx: &mut SpirvEmitContext,
    offset: Word,
    value: Word,
) -> Word {
    let pointer = shared_pointer(ctx, offset);
    let (scope, semantics) = atomic_args(ctx);
    ctx.builder
        .atomic_exchange(ctx.u32_type, None, pointer, scope, semantics, value)
        .unwrap()
}

pub fn emit_shared_atomic_exchange_64(
    ctx: &mut SpirvEmitContext,
    offset: Word,
    value: Word,
) -> Word {
    if ctx.profile.support_shared_int64_atomics && ctx.uses_explicit_workgroup_layout {
        let shift = ctx.constant_u32(3);
        let index = ctx
            .builder
            .shift_right_arithmetic(ctx.u32_type, None, offset, shift)
            .unwrap();
        let pointer = ctx
            .builder
            .access_chain(
                ctx.shared_u64,
                None,
                ctx.shared_memory_u64,
                vec![ctx.const_zero_u32, index],
            )
            .unwrap();
        let (scope, semantics) = atomic_args(ctx);
        return ctx
            .builder
            .atomic_exchange(ctx.u64_type, None, pointer, scope, semantics, value)
            .unwrap();
    }

    log::warn!("SPIR-V: int64 shared atomics unsupported; using non-atomic fallback");
    let pointer_1 = shared_pointer_at(ctx, offset, 0);
    let pointer_2 = shared_pointer_at(ctx, offset, 1);
    let value_1 = ctx
        .builder
        .load(ctx.u32_type, None, pointer_1, None, [])
        .unwrap();
    let value_2 = ctx
        .builder
        .load(ctx.u32_type, None, pointer_2, None, [])
        .unwrap();
    let new_vector = ctx.builder.bitcast(ctx.u32_vec2_type, None, value).unwrap();
    let new_value_1 = ctx
        .builder
        .composite_extract(ctx.u32_type, None, new_vector, [0])
        .unwrap();
    let new_value_2 = ctx
        .builder
        .composite_extract(ctx.u32_type, None, new_vector, [1])
        .unwrap();
    ctx.builder.store(pointer_1, new_value_1, None, []).unwrap();
    ctx.builder.store(pointer_2, new_value_2, None, []).unwrap();
    let original_vector = ctx
        .builder
        .composite_construct(ctx.u32_vec2_type, None, [value_1, value_2])
        .unwrap();
    ctx.builder
        .bitcast(ctx.u64_type, None, original_vector)
        .unwrap()
}

pub fn emit_shared_atomic_exchange_32x2(
    ctx: &mut SpirvEmitContext,
    offset: Word,
    value: Word,
) -> Word {
    log::warn!("SPIR-V: int64 shared atomics unsupported; using non-atomic fallback");
    let pointer_1 = shared_pointer_at(ctx, offset, 0);
    let pointer_2 = shared_pointer_at(ctx, offset, 1);
    let value_1 = ctx
        .builder
        .load(ctx.u32_type, None, pointer_1, None, [])
        .unwrap();
    let value_2 = ctx
        .builder
        .load(ctx.u32_type, None, pointer_2, None, [])
        .unwrap();
    let new_value_1 = ctx
        .builder
        .composite_extract(ctx.u32_type, None, value, [0])
        .unwrap();
    let new_value_2 = ctx
        .builder
        .composite_extract(ctx.u32_type, None, value, [1])
        .unwrap();
    ctx.builder.store(pointer_1, new_value_1, None, []).unwrap();
    ctx.builder.store(pointer_2, new_value_2, None, []).unwrap();
    ctx.builder
        .composite_construct(ctx.u32_vec2_type, None, [value_1, value_2])
        .unwrap()
}

pub fn emit_shared_atomic(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    block_idx: u32,
    inst_idx: u32,
) {
    let offset = ctx.resolve_value(inst.arg(0));
    let value = ctx.resolve_value(inst.arg(1));
    let result = match inst.opcode {
        Opcode::SharedAtomicIAdd32 => emit_shared_atomic_iadd_32(ctx, offset, value),
        Opcode::SharedAtomicSMin32 => emit_shared_atomic_smin_32(ctx, offset, value),
        Opcode::SharedAtomicUMin32 => emit_shared_atomic_umin_32(ctx, offset, value),
        Opcode::SharedAtomicSMax32 => emit_shared_atomic_smax_32(ctx, offset, value),
        Opcode::SharedAtomicUMax32 => emit_shared_atomic_umax_32(ctx, offset, value),
        Opcode::SharedAtomicInc32 => emit_shared_atomic_inc_32(ctx, offset, value),
        Opcode::SharedAtomicDec32 => emit_shared_atomic_dec_32(ctx, offset, value),
        Opcode::SharedAtomicAnd32 => emit_shared_atomic_and_32(ctx, offset, value),
        Opcode::SharedAtomicOr32 => emit_shared_atomic_or_32(ctx, offset, value),
        Opcode::SharedAtomicXor32 => emit_shared_atomic_xor_32(ctx, offset, value),
        Opcode::SharedAtomicExchange32 => emit_shared_atomic_exchange_32(ctx, offset, value),
        Opcode::SharedAtomicExchange64 => emit_shared_atomic_exchange_64(ctx, offset, value),
        Opcode::SharedAtomicExchange32x2 => emit_shared_atomic_exchange_32x2(ctx, offset, value),
        _ => unreachable!("not a shared atomic: {:?}", inst.opcode),
    };
    ctx.set_value(block_idx, inst_idx, result);
}

fn storage_index(ctx: &mut SpirvEmitContext, offset: ir::Value, element_size: u32) -> Word {
    if offset.is_immediate() {
        return ctx.constant_u32(offset.imm_u32() / element_size);
    }
    let index = ctx.resolve_value(&offset);
    let shift = element_size.trailing_zeros();
    if shift == 0 {
        return index;
    }
    let shift = ctx.constant_u32(shift);
    ctx.builder
        .shift_right_logical(ctx.u32_type, None, index, shift)
        .unwrap()
}

fn storage_buffer(ctx: &SpirvEmitContext, binding: ir::Value, kind: StorageDefinitionKind) -> Word {
    assert!(
        binding.is_immediate(),
        "dynamic storage buffer indexing is not implemented"
    );
    let buffer = ctx
        .ssbos
        .get(&binding.imm_u32())
        .copied()
        .unwrap_or_default()
        .get(kind);
    assert_ne!(buffer, 0, "missing {kind:?} SSBO view");
    buffer
}

fn storage_pointer_typed(
    ctx: &mut SpirvEmitContext,
    kind: StorageDefinitionKind,
    binding: ir::Value,
    offset: ir::Value,
    element_size: u32,
) -> Word {
    let ssbo = storage_buffer(ctx, binding, kind);
    let type_definition = ctx.storage_types.get(kind);
    assert_ne!(
        type_definition.element, 0,
        "missing {kind:?} SSBO pointer type"
    );
    let index = storage_index(ctx, offset, element_size);
    ctx.builder
        .access_chain(
            type_definition.element,
            None,
            ssbo,
            vec![ctx.const_zero_u32, index],
        )
        .unwrap()
}

fn storage_pointer(ctx: &mut SpirvEmitContext, binding: ir::Value, offset: ir::Value) -> Word {
    storage_pointer_typed(
        ctx,
        StorageDefinitionKind::U32,
        binding,
        offset,
        size_of::<u32>() as u32,
    )
}

#[derive(Clone, Copy)]
enum StorageAtomicOp {
    IAdd,
    SMin,
    UMin,
    SMax,
    UMax,
    And,
    Or,
    Xor,
    Exchange,
}

fn storage_atomic_u32(
    ctx: &mut SpirvEmitContext,
    binding: ir::Value,
    offset: ir::Value,
    value: Word,
    operation: StorageAtomicOp,
) -> Word {
    let pointer = storage_pointer(ctx, binding, offset);
    let (scope, semantics) = atomic_args(ctx);
    match operation {
        StorageAtomicOp::IAdd => {
            ctx.builder
                .atomic_i_add(ctx.u32_type, None, pointer, scope, semantics, value)
        }
        StorageAtomicOp::SMin => {
            ctx.builder
                .atomic_s_min(ctx.u32_type, None, pointer, scope, semantics, value)
        }
        StorageAtomicOp::UMin => {
            ctx.builder
                .atomic_u_min(ctx.u32_type, None, pointer, scope, semantics, value)
        }
        StorageAtomicOp::SMax => {
            ctx.builder
                .atomic_s_max(ctx.u32_type, None, pointer, scope, semantics, value)
        }
        StorageAtomicOp::UMax => {
            ctx.builder
                .atomic_u_max(ctx.u32_type, None, pointer, scope, semantics, value)
        }
        StorageAtomicOp::And => {
            ctx.builder
                .atomic_and(ctx.u32_type, None, pointer, scope, semantics, value)
        }
        StorageAtomicOp::Or => {
            ctx.builder
                .atomic_or(ctx.u32_type, None, pointer, scope, semantics, value)
        }
        StorageAtomicOp::Xor => {
            ctx.builder
                .atomic_xor(ctx.u32_type, None, pointer, scope, semantics, value)
        }
        StorageAtomicOp::Exchange => {
            ctx.builder
                .atomic_exchange(ctx.u32_type, None, pointer, scope, semantics, value)
        }
    }
    .unwrap()
}

pub fn emit_storage_atomic_iadd_32(
    ctx: &mut SpirvEmitContext,
    binding: ir::Value,
    offset: ir::Value,
    value: Word,
) -> Word {
    storage_atomic_u32(ctx, binding, offset, value, StorageAtomicOp::IAdd)
}

pub fn emit_storage_atomic_smin_32(
    ctx: &mut SpirvEmitContext,
    binding: ir::Value,
    offset: ir::Value,
    value: Word,
) -> Word {
    storage_atomic_u32(ctx, binding, offset, value, StorageAtomicOp::SMin)
}

pub fn emit_storage_atomic_umin_32(
    ctx: &mut SpirvEmitContext,
    binding: ir::Value,
    offset: ir::Value,
    value: Word,
) -> Word {
    storage_atomic_u32(ctx, binding, offset, value, StorageAtomicOp::UMin)
}

pub fn emit_storage_atomic_smax_32(
    ctx: &mut SpirvEmitContext,
    binding: ir::Value,
    offset: ir::Value,
    value: Word,
) -> Word {
    storage_atomic_u32(ctx, binding, offset, value, StorageAtomicOp::SMax)
}

pub fn emit_storage_atomic_umax_32(
    ctx: &mut SpirvEmitContext,
    binding: ir::Value,
    offset: ir::Value,
    value: Word,
) -> Word {
    storage_atomic_u32(ctx, binding, offset, value, StorageAtomicOp::UMax)
}

pub fn emit_storage_atomic_inc_32(
    ctx: &mut SpirvEmitContext,
    binding: ir::Value,
    offset: ir::Value,
    value: Word,
) -> Word {
    let ssbo = storage_buffer(ctx, binding, StorageDefinitionKind::U32);
    let index = storage_index(ctx, offset, size_of::<u32>() as u32);
    ctx.builder
        .function_call(
            ctx.u32_type,
            None,
            ctx.increment_cas_ssbo,
            vec![index, value, ssbo],
        )
        .unwrap()
}

pub fn emit_storage_atomic_dec_32(
    ctx: &mut SpirvEmitContext,
    binding: ir::Value,
    offset: ir::Value,
    value: Word,
) -> Word {
    let ssbo = storage_buffer(ctx, binding, StorageDefinitionKind::U32);
    let index = storage_index(ctx, offset, size_of::<u32>() as u32);
    ctx.builder
        .function_call(
            ctx.u32_type,
            None,
            ctx.decrement_cas_ssbo,
            vec![index, value, ssbo],
        )
        .unwrap()
}

pub fn emit_storage_atomic_and_32(
    ctx: &mut SpirvEmitContext,
    binding: ir::Value,
    offset: ir::Value,
    value: Word,
) -> Word {
    storage_atomic_u32(ctx, binding, offset, value, StorageAtomicOp::And)
}

pub fn emit_storage_atomic_or_32(
    ctx: &mut SpirvEmitContext,
    binding: ir::Value,
    offset: ir::Value,
    value: Word,
) -> Word {
    storage_atomic_u32(ctx, binding, offset, value, StorageAtomicOp::Or)
}

pub fn emit_storage_atomic_xor_32(
    ctx: &mut SpirvEmitContext,
    binding: ir::Value,
    offset: ir::Value,
    value: Word,
) -> Word {
    storage_atomic_u32(ctx, binding, offset, value, StorageAtomicOp::Xor)
}

pub fn emit_storage_atomic_exchange_32(
    ctx: &mut SpirvEmitContext,
    binding: ir::Value,
    offset: ir::Value,
    value: Word,
) -> Word {
    storage_atomic_u32(ctx, binding, offset, value, StorageAtomicOp::Exchange)
}

fn integer_min_max(
    ctx: &mut SpirvEmitContext,
    result_type: Word,
    lhs: Word,
    rhs: Word,
    signed: bool,
    maximum: bool,
) -> Word {
    let instruction = match (signed, maximum) {
        (false, false) => 38,
        (true, false) => 39,
        (false, true) => 41,
        (true, true) => 42,
    };
    ctx.builder
        .ext_inst(
            result_type,
            None,
            ctx.glsl_ext,
            instruction,
            vec![
                rspirv::dr::Operand::IdRef(lhs),
                rspirv::dr::Operand::IdRef(rhs),
            ],
        )
        .unwrap()
}

fn non_atomic_integer_operation(
    ctx: &mut SpirvEmitContext,
    result_type: Word,
    value: Word,
    original: Word,
    operation: StorageAtomicOp,
) -> Word {
    match operation {
        StorageAtomicOp::IAdd => ctx
            .builder
            .i_add(result_type, None, value, original)
            .unwrap(),
        StorageAtomicOp::SMin => integer_min_max(ctx, result_type, value, original, true, false),
        StorageAtomicOp::UMin => integer_min_max(ctx, result_type, value, original, false, false),
        StorageAtomicOp::SMax => integer_min_max(ctx, result_type, value, original, true, true),
        StorageAtomicOp::UMax => integer_min_max(ctx, result_type, value, original, false, true),
        StorageAtomicOp::And => ctx
            .builder
            .bitwise_and(result_type, None, value, original)
            .unwrap(),
        StorageAtomicOp::Or => ctx
            .builder
            .bitwise_or(result_type, None, value, original)
            .unwrap(),
        StorageAtomicOp::Xor => ctx
            .builder
            .bitwise_xor(result_type, None, value, original)
            .unwrap(),
        StorageAtomicOp::Exchange => value,
    }
}

fn storage_atomic_u64(
    ctx: &mut SpirvEmitContext,
    binding: ir::Value,
    offset: ir::Value,
    value: Word,
    operation: StorageAtomicOp,
) -> Word {
    if !ctx.profile.support_descriptor_aliasing {
        log::warn!("SPIR-V: descriptor aliasing unsupported; ignoring int64 storage atomic");
        return ctx.builder.constant_bit64(ctx.u64_type, 0);
    }
    if ctx.profile.support_int64_atomics {
        let pointer = storage_pointer_typed(
            ctx,
            StorageDefinitionKind::U64,
            binding,
            offset,
            size_of::<u64>() as u32,
        );
        let (scope, semantics) = atomic_args(ctx);
        return match operation {
            StorageAtomicOp::IAdd => {
                ctx.builder
                    .atomic_i_add(ctx.u64_type, None, pointer, scope, semantics, value)
            }
            StorageAtomicOp::SMin => {
                ctx.builder
                    .atomic_s_min(ctx.u64_type, None, pointer, scope, semantics, value)
            }
            StorageAtomicOp::UMin => {
                ctx.builder
                    .atomic_u_min(ctx.u64_type, None, pointer, scope, semantics, value)
            }
            StorageAtomicOp::SMax => {
                ctx.builder
                    .atomic_s_max(ctx.u64_type, None, pointer, scope, semantics, value)
            }
            StorageAtomicOp::UMax => {
                ctx.builder
                    .atomic_u_max(ctx.u64_type, None, pointer, scope, semantics, value)
            }
            StorageAtomicOp::And => {
                ctx.builder
                    .atomic_and(ctx.u64_type, None, pointer, scope, semantics, value)
            }
            StorageAtomicOp::Or => {
                ctx.builder
                    .atomic_or(ctx.u64_type, None, pointer, scope, semantics, value)
            }
            StorageAtomicOp::Xor => {
                ctx.builder
                    .atomic_xor(ctx.u64_type, None, pointer, scope, semantics, value)
            }
            StorageAtomicOp::Exchange => {
                ctx.builder
                    .atomic_exchange(ctx.u64_type, None, pointer, scope, semantics, value)
            }
        }
        .unwrap();
    }

    log::warn!("SPIR-V: int64 atomics unsupported; using non-atomic fallback");
    let pointer = storage_pointer_typed(
        ctx,
        StorageDefinitionKind::U32x2,
        binding,
        offset,
        size_of::<u64>() as u32,
    );
    let original_words = ctx
        .builder
        .load(ctx.u32_vec2_type, None, pointer, None, [])
        .unwrap();
    let original = ctx
        .builder
        .bitcast(ctx.u64_type, None, original_words)
        .unwrap();
    let result = non_atomic_integer_operation(ctx, ctx.u64_type, value, original, operation);
    let result_words = ctx
        .builder
        .bitcast(ctx.u32_vec2_type, None, result)
        .unwrap();
    ctx.builder.store(pointer, result_words, None, []).unwrap();
    original
}

fn storage_atomic_u32x2(
    ctx: &mut SpirvEmitContext,
    binding: ir::Value,
    offset: ir::Value,
    value: Word,
    operation: StorageAtomicOp,
) -> Word {
    if !ctx.profile.support_descriptor_aliasing {
        log::warn!("SPIR-V: descriptor aliasing unsupported; ignoring U32x2 storage atomic");
        return ctx.builder.constant_composite(
            ctx.u32_vec2_type,
            vec![ctx.const_zero_u32, ctx.const_zero_u32],
        );
    }
    log::warn!("SPIR-V: int64 atomics unsupported; using non-atomic fallback");
    let pointer = storage_pointer_typed(
        ctx,
        StorageDefinitionKind::U32x2,
        binding,
        offset,
        size_of::<u64>() as u32,
    );
    let original = ctx
        .builder
        .load(ctx.u32_vec2_type, None, pointer, None, [])
        .unwrap();
    let result = non_atomic_integer_operation(ctx, ctx.u32_vec2_type, value, original, operation);
    ctx.builder.store(pointer, result, None, []).unwrap();
    original
}

fn emit_storage_cas_call(
    ctx: &mut SpirvEmitContext,
    binding: ir::Value,
    offset: ir::Value,
    value: Word,
    function: Word,
    result_type: Word,
) -> Word {
    let ssbo = storage_buffer(ctx, binding, StorageDefinitionKind::U32);
    let index = storage_index(ctx, offset, size_of::<u32>() as u32);
    ctx.builder
        .function_call(result_type, None, function, vec![index, value, ssbo])
        .unwrap()
}

fn pack_half_2x16(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .ext_inst(
            ctx.u32_type,
            None,
            ctx.glsl_ext,
            58,
            vec![rspirv::dr::Operand::IdRef(value)],
        )
        .unwrap()
}

/// Ruzu extension; never fall back to non-atomic loads/stores for CAS.
fn storage_compare_exchange(
    ctx: &mut SpirvEmitContext,
    binding: ir::Value,
    offset: ir::Value,
    compare: Word,
    replacement: Word,
    wide: bool,
) -> Word {
    let (pointer, ty) = if wide {
        if !ctx.profile.support_int64_atomics || !ctx.profile.support_descriptor_aliasing {
            std::panic::panic_any(crate::exception::NotImplementedException::new(
                "64-bit storage CAS without native int64 atomics and descriptor aliasing",
            ));
        }
        (
            storage_pointer_typed(ctx, StorageDefinitionKind::U64, binding, offset, 8),
            ctx.u64_type,
        )
    } else {
        (storage_pointer(ctx, binding, offset), ctx.u32_type)
    };
    let (scope, semantics) = atomic_args(ctx);
    ctx.builder
        .atomic_compare_exchange(
            ty,
            None,
            pointer,
            scope,
            semantics,
            semantics,
            replacement,
            compare,
        )
        .unwrap()
}

pub fn emit_storage_atomic(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    block_idx: u32,
    inst_idx: u32,
) {
    let binding = *inst.arg(0);
    let offset = *inst.arg(1);
    let value = ctx.resolve_value(inst.arg(2));
    let result = match inst.opcode {
        Opcode::StorageAtomicCompareExchange32 | Opcode::StorageAtomicCompareExchange64 => {
            let replacement = ctx.resolve_value(inst.arg(3));
            storage_compare_exchange(
                ctx,
                binding,
                offset,
                value,
                replacement,
                inst.opcode == Opcode::StorageAtomicCompareExchange64,
            )
        }
        Opcode::StorageAtomicIAdd32 => emit_storage_atomic_iadd_32(ctx, binding, offset, value),
        Opcode::StorageAtomicSMin32 => emit_storage_atomic_smin_32(ctx, binding, offset, value),
        Opcode::StorageAtomicUMin32 => emit_storage_atomic_umin_32(ctx, binding, offset, value),
        Opcode::StorageAtomicSMax32 => emit_storage_atomic_smax_32(ctx, binding, offset, value),
        Opcode::StorageAtomicUMax32 => emit_storage_atomic_umax_32(ctx, binding, offset, value),
        Opcode::StorageAtomicInc32 => emit_storage_atomic_inc_32(ctx, binding, offset, value),
        Opcode::StorageAtomicDec32 => emit_storage_atomic_dec_32(ctx, binding, offset, value),
        Opcode::StorageAtomicAnd32 => emit_storage_atomic_and_32(ctx, binding, offset, value),
        Opcode::StorageAtomicOr32 => emit_storage_atomic_or_32(ctx, binding, offset, value),
        Opcode::StorageAtomicXor32 => emit_storage_atomic_xor_32(ctx, binding, offset, value),
        Opcode::StorageAtomicExchange32 => {
            emit_storage_atomic_exchange_32(ctx, binding, offset, value)
        }
        Opcode::StorageAtomicIAdd64 => {
            storage_atomic_u64(ctx, binding, offset, value, StorageAtomicOp::IAdd)
        }
        Opcode::StorageAtomicSMin64 => {
            storage_atomic_u64(ctx, binding, offset, value, StorageAtomicOp::SMin)
        }
        Opcode::StorageAtomicUMin64 => {
            storage_atomic_u64(ctx, binding, offset, value, StorageAtomicOp::UMin)
        }
        Opcode::StorageAtomicSMax64 => {
            storage_atomic_u64(ctx, binding, offset, value, StorageAtomicOp::SMax)
        }
        Opcode::StorageAtomicUMax64 => {
            storage_atomic_u64(ctx, binding, offset, value, StorageAtomicOp::UMax)
        }
        Opcode::StorageAtomicAnd64 => {
            storage_atomic_u64(ctx, binding, offset, value, StorageAtomicOp::And)
        }
        Opcode::StorageAtomicOr64 => {
            storage_atomic_u64(ctx, binding, offset, value, StorageAtomicOp::Or)
        }
        Opcode::StorageAtomicXor64 => {
            storage_atomic_u64(ctx, binding, offset, value, StorageAtomicOp::Xor)
        }
        Opcode::StorageAtomicExchange64 => {
            storage_atomic_u64(ctx, binding, offset, value, StorageAtomicOp::Exchange)
        }
        Opcode::StorageAtomicIAdd32x2 => {
            storage_atomic_u32x2(ctx, binding, offset, value, StorageAtomicOp::IAdd)
        }
        Opcode::StorageAtomicSMin32x2 => {
            storage_atomic_u32x2(ctx, binding, offset, value, StorageAtomicOp::SMin)
        }
        Opcode::StorageAtomicUMin32x2 => {
            storage_atomic_u32x2(ctx, binding, offset, value, StorageAtomicOp::UMin)
        }
        Opcode::StorageAtomicSMax32x2 => {
            storage_atomic_u32x2(ctx, binding, offset, value, StorageAtomicOp::SMax)
        }
        Opcode::StorageAtomicUMax32x2 => {
            storage_atomic_u32x2(ctx, binding, offset, value, StorageAtomicOp::UMax)
        }
        Opcode::StorageAtomicAnd32x2 => {
            storage_atomic_u32x2(ctx, binding, offset, value, StorageAtomicOp::And)
        }
        Opcode::StorageAtomicOr32x2 => {
            storage_atomic_u32x2(ctx, binding, offset, value, StorageAtomicOp::Or)
        }
        Opcode::StorageAtomicXor32x2 => {
            storage_atomic_u32x2(ctx, binding, offset, value, StorageAtomicOp::Xor)
        }
        Opcode::StorageAtomicExchange32x2 => {
            storage_atomic_u32x2(ctx, binding, offset, value, StorageAtomicOp::Exchange)
        }
        Opcode::StorageAtomicAddF32 => {
            let function = ctx.f32_add_cas;
            let result_type = ctx.f32_type;
            emit_storage_cas_call(ctx, binding, offset, value, function, result_type)
        }
        Opcode::StorageAtomicAddF16x2 => {
            let function = ctx.f16x2_add_cas;
            let result_type = ctx.f16_vec2_type;
            let result = emit_storage_cas_call(ctx, binding, offset, value, function, result_type);
            ctx.builder.bitcast(ctx.u32_type, None, result).unwrap()
        }
        Opcode::StorageAtomicAddF32x2 => {
            let function = ctx.f32x2_add_cas;
            let result_type = ctx.f32_vec2_type;
            let result = emit_storage_cas_call(ctx, binding, offset, value, function, result_type);
            pack_half_2x16(ctx, result)
        }
        Opcode::StorageAtomicMinF16x2 => {
            let function = ctx.f16x2_min_cas;
            let result_type = ctx.f16_vec2_type;
            let result = emit_storage_cas_call(ctx, binding, offset, value, function, result_type);
            ctx.builder.bitcast(ctx.u32_type, None, result).unwrap()
        }
        Opcode::StorageAtomicMinF32x2 => {
            let function = ctx.f32x2_min_cas;
            let result_type = ctx.f32_vec2_type;
            let result = emit_storage_cas_call(ctx, binding, offset, value, function, result_type);
            pack_half_2x16(ctx, result)
        }
        Opcode::StorageAtomicMaxF16x2 => {
            let function = ctx.f16x2_max_cas;
            let result_type = ctx.f16_vec2_type;
            let result = emit_storage_cas_call(ctx, binding, offset, value, function, result_type);
            ctx.builder.bitcast(ctx.u32_type, None, result).unwrap()
        }
        Opcode::StorageAtomicMaxF32x2 => {
            let function = ctx.f32x2_max_cas;
            let result_type = ctx.f32_vec2_type;
            let result = emit_storage_cas_call(ctx, binding, offset, value, function, result_type);
            pack_half_2x16(ctx, result)
        }
        _ => unreachable!("not a storage atomic: {:?}", inst.opcode),
    };
    ctx.set_value(block_idx, inst_idx, result);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::bindings::Bindings;
    use crate::ir::basic_block::Block;
    use crate::ir::instruction::Inst;
    use crate::ir::types::{ShaderStage, Type};
    use crate::ir::value::Value;
    use crate::ir::{self, SyntaxNode};
    use crate::profile::Profile;
    use crate::runtime_info::RuntimeInfo;
    use crate::shader_info::StorageBufferDescriptor;

    fn contains_opcode(ctx: &SpirvEmitContext, opcode: spirv::Op) -> bool {
        ctx.builder.module_ref().functions.iter().any(|function| {
            function.blocks.iter().any(|block| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| instruction.class.opcode == opcode)
            })
        })
    }

    #[test]
    fn cas_emits_native_compare_exchange_with_replacement_before_comparator() {
        for wide in [false, true] {
            let mut program = ir::Program::new(ShaderStage::Compute);
            program.blocks.push(Block::new());
            let opcode = if wide {
                Opcode::StorageAtomicCompareExchange64
            } else {
                Opcode::StorageAtomicCompareExchange32
            };
            let (compare, replacement) = if wide {
                (Value::ImmU64(17), Value::ImmU64(29))
            } else {
                (Value::ImmU32(17), Value::ImmU32(29))
            };
            program.block_mut(0).append_inst(Inst::new(
                opcode,
                vec![Value::ImmU32(0), Value::ImmU32(16), compare, replacement],
            ));
            program.info.used_storage_buffer_types = if wide {
                Type::U64 as u32
            } else {
                Type::U32 as u32
            };
            program.info.uses_int64 = wide;
            program.info.uses_int64_bit_atomics = wide;
            program.info.storage_buffers_descriptors = vec![StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                is_written: true,
            }];
            program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];
            let profile = Profile {
                support_int64: true,
                support_int64_atomics: true,
                support_descriptor_aliasing: true,
                ..Profile::default()
            };
            let mut ctx = SpirvEmitContext::new(&program, &profile, &RuntimeInfo::default());
            ctx.emit_program(&program);
            let atom = ctx
                .builder
                .module_ref()
                .functions
                .iter()
                .flat_map(|f| &f.blocks)
                .flat_map(|b| &b.instructions)
                .find(|i| i.class.opcode == spirv::Op::AtomicCompareExchange)
                .unwrap();
            for (operand, expected) in [(4, 29u64), (5, 17u64)] {
                let id = atom.operands[operand].unwrap_id_ref();
                let constant = ctx
                    .builder
                    .module_ref()
                    .types_global_values
                    .iter()
                    .find(|i| i.result_id == Some(id))
                    .unwrap();
                assert_eq!(
                    constant.operands[0],
                    if wide {
                        rspirv::dr::Operand::LiteralBit64(expected)
                    } else {
                        rspirv::dr::Operand::LiteralBit32(expected as u32)
                    }
                );
            }
            assert!(!contains_opcode(&ctx, spirv::Op::Store));
            if let Some(validator) = std::env::var_os("RUZU_SPIRV_VAL") {
                use rspirv::binary::Assemble;
                let words = ctx.builder.module().assemble();
                let path = std::env::temp_dir()
                    .join(format!("ruzu-cas-{}-{wide}.spv", std::process::id()));
                let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
                std::fs::write(&path, bytes).unwrap();
                let output = std::process::Command::new(validator)
                    .args(["--target-env", "vulkan1.2"])
                    .arg(&path)
                    .output()
                    .unwrap();
                let _ = std::fs::remove_file(path);
                assert!(
                    output.status.success(),
                    "CAS SPIR-V invalid: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }

    #[test]
    fn cas_64_rejects_missing_native_atomics() {
        let program = ir::Program::new(ShaderStage::Compute);
        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            storage_compare_exchange(&mut ctx, Value::ImmU32(0), Value::ImmU32(0), 0, 0, true);
        }));
        assert!(result
            .unwrap_err()
            .is::<crate::exception::NotImplementedException>());
    }

    fn emit_exchange_64(profile: Profile) -> SpirvEmitContext {
        let mut program = ir::Program::new(ShaderStage::Compute);
        program.shared_memory_size = 64;
        program.info.uses_int64 = true;
        program.info.uses_int64_bit_atomics = true;
        let mut ctx = SpirvEmitContext::new(&program, &profile, &RuntimeInfo::default());
        ctx.define_global_variables(&program, &mut Bindings::default());
        ctx.builder
            .begin_function(
                ctx.void_type,
                None,
                spirv::FunctionControl::NONE,
                ctx.void_fn_type,
            )
            .unwrap();
        ctx.builder.begin_block(None).unwrap();
        let offset = ctx.const_zero_u32;
        let value = ctx.builder.undef(ctx.u64_type, None);
        emit_shared_atomic_exchange_64(&mut ctx, offset, value);
        ctx.builder.ret().unwrap();
        ctx.builder.end_function().unwrap();
        ctx
    }

    #[test]
    fn shared_exchange_64_uses_native_atomic_with_explicit_layout() {
        let profile = Profile {
            supported_spirv: 0x0001_0400,
            support_int64: true,
            support_shared_int64_atomics: true,
            support_explicit_workgroup_layout: true,
            ..Profile::default()
        };
        let ctx = emit_exchange_64(profile);
        assert!(contains_opcode(&ctx, spirv::Op::AtomicExchange));
        assert_ne!(ctx.shared_memory_u64, 0);
    }

    /// Upstream gates the native path on `support_shared_int64_atomics`, not on
    /// the buffer-side `support_int64_atomics`. A device offering only the
    /// latter must take the non-atomic fallback.
    #[test]
    fn shared_exchange_64_ignores_buffer_only_int64_atomics() {
        let profile = Profile {
            supported_spirv: 0x0001_0400,
            support_int64: true,
            support_int64_atomics: true,
            support_explicit_workgroup_layout: true,
            ..Profile::default()
        };
        let ctx = emit_exchange_64(profile);
        assert!(!contains_opcode(&ctx, spirv::Op::AtomicExchange));
    }

    /// Upstream reads the *effective* `uses_explicit_workgroup_layout`, which
    /// also requires the 8-bit sub-capability when the program uses int8.
    #[test]
    fn shared_exchange_64_falls_back_without_workgroup_layout_8bit_access() {
        let profile = Profile {
            supported_spirv: 0x0001_0400,
            support_int8: true,
            support_int64: true,
            support_shared_int64_atomics: true,
            support_explicit_workgroup_layout: true,
            support_workgroup_layout_8bit_access: false,
            ..Profile::default()
        };
        let mut program = ir::Program::new(ShaderStage::Compute);
        program.shared_memory_size = 64;
        program.info.uses_int8 = true;
        program.info.uses_int64 = true;
        program.info.uses_int64_bit_atomics = true;
        let mut ctx = SpirvEmitContext::new(&program, &profile, &RuntimeInfo::default());
        ctx.define_global_variables(&program, &mut Bindings::default());
        assert!(!ctx.uses_explicit_workgroup_layout);
        ctx.builder
            .begin_function(
                ctx.void_type,
                None,
                spirv::FunctionControl::NONE,
                ctx.void_fn_type,
            )
            .unwrap();
        ctx.builder.begin_block(None).unwrap();
        let offset = ctx.const_zero_u32;
        let value = ctx.builder.undef(ctx.u64_type, None);
        emit_shared_atomic_exchange_64(&mut ctx, offset, value);
        ctx.builder.ret().unwrap();
        ctx.builder.end_function().unwrap();
        assert!(!contains_opcode(&ctx, spirv::Op::AtomicExchange));
    }

    #[test]
    fn shared_exchange_64_falls_back_to_two_non_atomic_words() {
        let profile = Profile {
            support_int64: true,
            ..Profile::default()
        };
        let ctx = emit_exchange_64(profile);
        assert!(!contains_opcode(&ctx, spirv::Op::AtomicExchange));
        assert!(contains_opcode(&ctx, spirv::Op::Load));
        assert!(contains_opcode(&ctx, spirv::Op::Store));
        assert!(contains_opcode(&ctx, spirv::Op::Bitcast));
    }

    #[test]
    fn lowered_shared_exchange_needs_no_int64_type() {
        let mut program = ir::Program::new(ShaderStage::Compute);
        program.shared_memory_size = 64;
        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());
        ctx.define_global_variables(&program, &mut Bindings::default());
        ctx.builder
            .begin_function(
                ctx.void_type,
                None,
                spirv::FunctionControl::NONE,
                ctx.void_fn_type,
            )
            .unwrap();
        ctx.builder.begin_block(None).unwrap();
        let value = ctx.builder.undef(ctx.u32_vec2_type, None);
        let offset = ctx.const_zero_u32;
        emit_shared_atomic_exchange_32x2(&mut ctx, offset, value);
        ctx.builder.ret().unwrap();
        ctx.builder.end_function().unwrap();

        assert!(contains_opcode(&ctx, spirv::Op::Load));
        assert!(contains_opcode(&ctx, spirv::Op::Store));
        assert_eq!(ctx.u64_type, ctx.u32_type);
    }

    #[test]
    fn storage_u32_atomics_use_typed_ssbo_pointer() {
        let mut program = ir::Program::new(ShaderStage::Compute);
        program.blocks.push(Block::new());
        for opcode in [
            Opcode::StorageAtomicIAdd32,
            Opcode::StorageAtomicSMin32,
            Opcode::StorageAtomicUMin32,
            Opcode::StorageAtomicSMax32,
            Opcode::StorageAtomicUMax32,
            Opcode::StorageAtomicAnd32,
            Opcode::StorageAtomicOr32,
            Opcode::StorageAtomicXor32,
            Opcode::StorageAtomicExchange32,
        ] {
            program.block_mut(0).append_inst(Inst::new(
                opcode,
                vec![Value::ImmU32(0), Value::ImmU32(12), Value::ImmU32(7)],
            ));
        }
        program.info.used_storage_buffer_types = Type::U32 as u32;
        program.info.storage_buffers_descriptors = vec![StorageBufferDescriptor {
            cbuf_index: 0,
            cbuf_offset: 0,
            count: 1,
            is_written: true,
        }];
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];

        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());
        ctx.emit_program(&program);

        for opcode in [
            spirv::Op::AtomicIAdd,
            spirv::Op::AtomicSMin,
            spirv::Op::AtomicUMin,
            spirv::Op::AtomicSMax,
            spirv::Op::AtomicUMax,
            spirv::Op::AtomicAnd,
            spirv::Op::AtomicOr,
            spirv::Op::AtomicXor,
            spirv::Op::AtomicExchange,
        ] {
            assert!(contains_opcode(&ctx, opcode), "missing {opcode:?}");
        }
        assert!(!contains_opcode(&ctx, spirv::Op::Undef));
    }

    #[test]
    fn storage_cas_helpers_follow_shader_info_usage() {
        let mut program = ir::Program::new(ShaderStage::Compute);
        program.info.used_storage_buffer_types = Type::U32 as u32;
        program.info.storage_buffers_descriptors = vec![StorageBufferDescriptor {
            cbuf_index: 0,
            cbuf_offset: 0,
            count: 1,
            is_written: true,
        }];
        program.info.uses_global_increment = true;
        program.info.uses_global_decrement = true;
        program.info.uses_atomic_f32_add = true;
        program.info.uses_atomic_f16x2_add = true;
        program.info.uses_atomic_f16x2_min = true;
        program.info.uses_atomic_f16x2_max = true;
        program.info.uses_atomic_f32x2_add = true;
        program.info.uses_atomic_f32x2_min = true;
        program.info.uses_atomic_f32x2_max = true;

        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());
        ctx.define_global_variables(&program, &mut Bindings::default());

        for helper in [
            ctx.increment_cas_ssbo,
            ctx.decrement_cas_ssbo,
            ctx.f32_add_cas,
            ctx.f16x2_add_cas,
            ctx.f16x2_min_cas,
            ctx.f16x2_max_cas,
            ctx.f32x2_add_cas,
            ctx.f32x2_min_cas,
            ctx.f32x2_max_cas,
        ] {
            assert_ne!(helper, 0);
        }
        assert!(contains_opcode(&ctx, spirv::Op::AtomicCompareExchange));
        assert!(ctx
            .builder
            .module_ref()
            .capabilities
            .iter()
            .any(|instruction| {
                instruction.operands
                    == [rspirv::dr::Operand::Capability(
                        spirv::Capability::VariablePointersStorageBuffer,
                    )]
            }));
    }

    fn emit_storage_iadd_64(profile: Profile) -> SpirvEmitContext {
        let mut program = ir::Program::new(ShaderStage::Compute);
        program.info.uses_int64 = true;
        program.info.used_storage_buffer_types = Type::U64 as u32 | Type::U32x2 as u32;
        program.info.storage_buffers_descriptors = vec![StorageBufferDescriptor {
            cbuf_index: 0,
            cbuf_offset: 0,
            count: 1,
            is_written: true,
        }];
        let mut ctx = SpirvEmitContext::new(&program, &profile, &RuntimeInfo::default());
        ctx.define_global_variables(&program, &mut Bindings::default());
        ctx.builder
            .begin_function(
                ctx.void_type,
                None,
                spirv::FunctionControl::NONE,
                ctx.void_fn_type,
            )
            .unwrap();
        ctx.builder.begin_block(None).unwrap();
        let value = ctx.builder.undef(ctx.u64_type, None);
        storage_atomic_u64(
            &mut ctx,
            Value::ImmU32(0),
            Value::ImmU32(0),
            value,
            StorageAtomicOp::IAdd,
        );
        ctx.builder.ret().unwrap();
        ctx.builder.end_function().unwrap();
        ctx
    }

    #[test]
    fn storage_iadd_64_uses_native_atomic_when_supported() {
        let profile = Profile {
            support_int64: true,
            support_int64_atomics: true,
            support_descriptor_aliasing: true,
            ..Profile::default()
        };
        let ctx = emit_storage_iadd_64(profile);
        assert!(contains_opcode(&ctx, spirv::Op::AtomicIAdd));
    }

    #[test]
    fn storage_iadd_64_matches_upstream_non_atomic_fallback() {
        let profile = Profile {
            support_int64: true,
            support_descriptor_aliasing: true,
            ..Profile::default()
        };
        let ctx = emit_storage_iadd_64(profile);
        assert!(!contains_opcode(&ctx, spirv::Op::AtomicIAdd));
        assert!(contains_opcode(&ctx, spirv::Op::Load));
        assert!(contains_opcode(&ctx, spirv::Op::Store));
        assert!(contains_opcode(&ctx, spirv::Op::IAdd));
    }
}
