// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SPIR-V memory operation emission — maps to zuyu's
//! `backend/spirv/emit_spirv_memory.cpp`.

use super::spirv_emit_context::{SpirvEmitContext, StorageDefinitionKind, StorageTypeDefinition};
use crate::ir::value::Value;
use crate::ir::{self, Opcode};
use rspirv::spirv::Word;

fn storage_index(
    ctx: &mut SpirvEmitContext,
    offset: Value,
    element_size: u32,
    index_offset: u32,
) -> Word {
    if offset.is_immediate() {
        return ctx.constant_u32(offset.imm_u32() / element_size + index_offset);
    }
    let mut index = ctx.resolve_value(&offset);
    let shift = element_size.trailing_zeros();
    if shift != 0 {
        let shift = ctx.constant_u32(shift);
        index = ctx
            .builder
            .shift_right_logical(ctx.u32_type, None, index, shift)
            .unwrap();
    }
    if index_offset != 0 {
        let index_offset = ctx.constant_u32(index_offset);
        index = ctx
            .builder
            .i_add(ctx.u32_type, None, index, index_offset)
            .unwrap();
    }
    index
}

fn storage_pointer(
    ctx: &mut SpirvEmitContext,
    binding: Value,
    offset: Value,
    type_definition: StorageTypeDefinition,
    kind: StorageDefinitionKind,
    element_size: u32,
    index_offset: u32,
) -> Word {
    let binding = binding.imm_u32();
    let ssbo = ctx
        .ssbos
        .get(&binding)
        .copied()
        .unwrap_or_default()
        .get(kind);
    assert_ne!(ssbo, 0, "missing SSBO {binding} view {kind:?}");
    assert_ne!(
        type_definition.element, 0,
        "missing SSBO element pointer {kind:?}"
    );
    let index = storage_index(ctx, offset, element_size, index_offset);
    ctx.builder
        .access_chain(
            type_definition.element,
            None,
            ssbo,
            vec![ctx.const_zero_u32, index],
        )
        .unwrap()
}

fn load_storage(
    ctx: &mut SpirvEmitContext,
    binding: Value,
    offset: Value,
    result_type: Word,
    kind: StorageDefinitionKind,
    element_size: u32,
    index_offset: u32,
) -> Word {
    let type_definition = ctx.storage_types.get(kind);
    let pointer = storage_pointer(
        ctx,
        binding,
        offset,
        type_definition,
        kind,
        element_size,
        index_offset,
    );
    ctx.builder
        .load(result_type, None, pointer, None, [])
        .unwrap()
}

fn write_storage(
    ctx: &mut SpirvEmitContext,
    binding: Value,
    offset: Value,
    value: Word,
    kind: StorageDefinitionKind,
    element_size: u32,
    index_offset: u32,
) {
    let type_definition = ctx.storage_types.get(kind);
    let pointer = storage_pointer(
        ctx,
        binding,
        offset,
        type_definition,
        kind,
        element_size,
        index_offset,
    );
    ctx.builder.store(pointer, value, None, []).unwrap();
}

fn load_storage_32(
    ctx: &mut SpirvEmitContext,
    binding: Value,
    offset: Value,
    index_offset: u32,
) -> Word {
    load_storage(
        ctx,
        binding,
        offset,
        ctx.u32_type,
        StorageDefinitionKind::U32,
        4,
        index_offset,
    )
}

fn write_storage_32(
    ctx: &mut SpirvEmitContext,
    binding: Value,
    offset: Value,
    value: Word,
    index_offset: u32,
) {
    write_storage(
        ctx,
        binding,
        offset,
        value,
        StorageDefinitionKind::U32,
        4,
        index_offset,
    );
}

fn write_storage_by_cas_loop(
    ctx: &mut SpirvEmitContext,
    binding: Value,
    offset: Value,
    value: Word,
    bit_offset: Word,
    bit_count: u32,
) {
    assert_ne!(
        ctx.write_storage_cas_loop_func, 0,
        "missing storage subword CAS helper"
    );
    let type_definition = ctx.storage_types.get(StorageDefinitionKind::U32);
    let pointer = storage_pointer(
        ctx,
        binding,
        offset,
        type_definition,
        StorageDefinitionKind::U32,
        4,
        0,
    );
    let bit_count = ctx.constant_u32(bit_count);
    ctx.builder
        .function_call(
            ctx.void_type,
            None,
            ctx.write_storage_cas_loop_func,
            vec![pointer, value, bit_offset, bit_count],
        )
        .unwrap();
}

fn emit_load_storage(
    ctx: &mut SpirvEmitContext,
    opcode: Opcode,
    binding: Value,
    offset: Value,
) -> Word {
    match opcode {
        Opcode::LoadStorageU8
            if ctx.profile.support_int8
                && ctx.profile.support_storage_buffer_8bit
                && ctx.profile.support_descriptor_aliasing =>
        {
            let value = load_storage(
                ctx,
                binding,
                offset,
                ctx.u8_type,
                StorageDefinitionKind::U8,
                1,
                0,
            );
            ctx.builder.u_convert(ctx.u32_type, None, value).unwrap()
        }
        Opcode::LoadStorageS8
            if ctx.profile.support_int8
                && ctx.profile.support_storage_buffer_8bit
                && ctx.profile.support_descriptor_aliasing =>
        {
            let value = load_storage(
                ctx,
                binding,
                offset,
                ctx.i8_type,
                StorageDefinitionKind::I8,
                1,
                0,
            );
            ctx.builder.s_convert(ctx.u32_type, None, value).unwrap()
        }
        Opcode::LoadStorageU16
            if ctx.profile.support_int16
                && ctx.profile.support_storage_buffer_16bit
                && ctx.profile.support_descriptor_aliasing =>
        {
            let value = load_storage(
                ctx,
                binding,
                offset,
                ctx.u16_type,
                StorageDefinitionKind::U16,
                2,
                0,
            );
            ctx.builder.u_convert(ctx.u32_type, None, value).unwrap()
        }
        Opcode::LoadStorageS16
            if ctx.profile.support_int16
                && ctx.profile.support_storage_buffer_16bit
                && ctx.profile.support_descriptor_aliasing =>
        {
            let value = load_storage(
                ctx,
                binding,
                offset,
                ctx.i16_type,
                StorageDefinitionKind::I16,
                2,
                0,
            );
            ctx.builder.s_convert(ctx.u32_type, None, value).unwrap()
        }
        Opcode::LoadStorageU8
        | Opcode::LoadStorageS8
        | Opcode::LoadStorageU16
        | Opcode::LoadStorageS16 => {
            let word = load_storage_32(ctx, binding, offset, 0);
            let (width, signed) = match opcode {
                Opcode::LoadStorageU8 => (8, false),
                Opcode::LoadStorageS8 => (8, true),
                Opcode::LoadStorageU16 => (16, false),
                Opcode::LoadStorageS16 => (16, true),
                _ => unreachable!(),
            };
            let bit_offset = if width == 8 {
                ctx.bit_offset_8(offset)
            } else {
                ctx.bit_offset_16(offset)
            };
            let width = ctx.constant_u32(width);
            if signed {
                ctx.builder
                    .bit_field_s_extract(ctx.u32_type, None, word, bit_offset, width)
                    .unwrap()
            } else {
                ctx.builder
                    .bit_field_u_extract(ctx.u32_type, None, word, bit_offset, width)
                    .unwrap()
            }
        }
        Opcode::LoadStorage32 => load_storage_32(ctx, binding, offset, 0),
        Opcode::LoadStorage64 if ctx.profile.support_descriptor_aliasing => load_storage(
            ctx,
            binding,
            offset,
            ctx.u32_vec2_type,
            StorageDefinitionKind::U32x2,
            8,
            0,
        ),
        Opcode::LoadStorage128 if ctx.profile.support_descriptor_aliasing => load_storage(
            ctx,
            binding,
            offset,
            ctx.u32_vec4_type,
            StorageDefinitionKind::U32x4,
            16,
            0,
        ),
        Opcode::LoadStorage64 => {
            let x = load_storage_32(ctx, binding, offset, 0);
            let y = load_storage_32(ctx, binding, offset, 1);
            ctx.builder
                .composite_construct(ctx.u32_vec2_type, None, vec![x, y])
                .unwrap()
        }
        Opcode::LoadStorage128 => {
            let values: Vec<_> = (0..4)
                .map(|index| load_storage_32(ctx, binding, offset, index))
                .collect();
            ctx.builder
                .composite_construct(ctx.u32_vec4_type, None, values)
                .unwrap()
        }
        _ => unreachable!("non-storage load opcode {opcode:?}"),
    }
}

fn emit_write_storage(
    ctx: &mut SpirvEmitContext,
    opcode: Opcode,
    binding: Value,
    offset: Value,
    value: Word,
) {
    match opcode {
        Opcode::WriteStorageU8
            if ctx.profile.support_int8
                && ctx.profile.support_storage_buffer_8bit
                && ctx.profile.support_descriptor_aliasing =>
        {
            let value = ctx.builder.s_convert(ctx.u8_type, None, value).unwrap();
            write_storage(ctx, binding, offset, value, StorageDefinitionKind::U8, 1, 0);
        }
        Opcode::WriteStorageS8
            if ctx.profile.support_int8
                && ctx.profile.support_storage_buffer_8bit
                && ctx.profile.support_descriptor_aliasing =>
        {
            let value = ctx.builder.s_convert(ctx.i8_type, None, value).unwrap();
            write_storage(ctx, binding, offset, value, StorageDefinitionKind::I8, 1, 0);
        }
        Opcode::WriteStorageU16
            if ctx.profile.support_int16
                && ctx.profile.support_storage_buffer_16bit
                && ctx.profile.support_descriptor_aliasing =>
        {
            let value = ctx.builder.s_convert(ctx.u16_type, None, value).unwrap();
            write_storage(
                ctx,
                binding,
                offset,
                value,
                StorageDefinitionKind::U16,
                2,
                0,
            );
        }
        Opcode::WriteStorageS16
            if ctx.profile.support_int16
                && ctx.profile.support_storage_buffer_16bit
                && ctx.profile.support_descriptor_aliasing =>
        {
            let value = ctx.builder.s_convert(ctx.i16_type, None, value).unwrap();
            write_storage(
                ctx,
                binding,
                offset,
                value,
                StorageDefinitionKind::I16,
                2,
                0,
            );
        }
        Opcode::WriteStorageU8 | Opcode::WriteStorageS8 => {
            let bit_offset = ctx.bit_offset_8(offset);
            write_storage_by_cas_loop(ctx, binding, offset, value, bit_offset, 8);
        }
        Opcode::WriteStorageU16 | Opcode::WriteStorageS16 => {
            let bit_offset = ctx.bit_offset_16(offset);
            write_storage_by_cas_loop(ctx, binding, offset, value, bit_offset, 16);
        }
        Opcode::WriteStorage32 => write_storage_32(ctx, binding, offset, value, 0),
        Opcode::WriteStorage64 if ctx.profile.support_descriptor_aliasing => write_storage(
            ctx,
            binding,
            offset,
            value,
            StorageDefinitionKind::U32x2,
            8,
            0,
        ),
        Opcode::WriteStorage128 if ctx.profile.support_descriptor_aliasing => write_storage(
            ctx,
            binding,
            offset,
            value,
            StorageDefinitionKind::U32x4,
            16,
            0,
        ),
        Opcode::WriteStorage64 => {
            for index in 0..2 {
                let element = ctx
                    .builder
                    .composite_extract(ctx.u32_type, None, value, vec![index])
                    .unwrap();
                write_storage_32(ctx, binding, offset, element, index);
            }
        }
        Opcode::WriteStorage128 => {
            for index in 0..4 {
                let element = ctx
                    .builder
                    .composite_extract(ctx.u32_type, None, value, vec![index])
                    .unwrap();
                write_storage_32(ctx, binding, offset, element, index);
            }
        }
        _ => unreachable!("non-storage write opcode {opcode:?}"),
    }
}

fn emit_load_global(ctx: &mut SpirvEmitContext, opcode: Opcode, address: Word) -> Word {
    let (result_type, function) = match opcode {
        Opcode::LoadGlobal32 => (ctx.u32_type, ctx.load_global_func_u32),
        Opcode::LoadGlobal64 => (ctx.u32_vec2_type, ctx.load_global_func_u32x2),
        Opcode::LoadGlobal128 => (ctx.u32_vec4_type, ctx.load_global_func_u32x4),
        _ => unreachable!("non-global load opcode {opcode:?}"),
    };
    if !ctx.profile.support_int64 {
        log::warn!("Int64 not supported, ignoring memory operation");
        return ctx.builder.constant_null(result_type);
    }
    assert_ne!(function, 0, "missing global load helper {opcode:?}");
    ctx.builder
        .function_call(result_type, None, function, vec![address])
        .unwrap()
}

fn emit_write_global(ctx: &mut SpirvEmitContext, opcode: Opcode, address: Word, value: Word) {
    if !ctx.profile.support_int64 {
        log::warn!("Int64 not supported, ignoring memory operation");
        return;
    }
    let function = match opcode {
        Opcode::WriteGlobal32 => ctx.write_global_func_u32,
        Opcode::WriteGlobal64 => ctx.write_global_func_u32x2,
        Opcode::WriteGlobal128 => ctx.write_global_func_u32x4,
        _ => unreachable!("non-global write opcode {opcode:?}"),
    };
    assert_ne!(function, 0, "missing global write helper {opcode:?}");
    ctx.builder
        .function_call(ctx.void_type, None, function, vec![address, value])
        .unwrap();
}

/// Dispatch storage/global load IR instructions.
pub fn emit_load(ctx: &mut SpirvEmitContext, inst: &ir::Inst, block_idx: u32, inst_idx: u32) {
    let value = match inst.opcode {
        Opcode::LoadStorageU8
        | Opcode::LoadStorageS8
        | Opcode::LoadStorageU16
        | Opcode::LoadStorageS16
        | Opcode::LoadStorage32
        | Opcode::LoadStorage64
        | Opcode::LoadStorage128 => emit_load_storage(ctx, inst.opcode, *inst.arg(0), *inst.arg(1)),
        Opcode::LoadGlobalU8
        | Opcode::LoadGlobalS8
        | Opcode::LoadGlobalU16
        | Opcode::LoadGlobalS16 => {
            panic!("SPIR-V global subword loads are not implemented upstream")
        }
        Opcode::LoadGlobal32 | Opcode::LoadGlobal64 | Opcode::LoadGlobal128 => {
            let address = ctx.resolve_value(inst.arg(0));
            emit_load_global(ctx, inst.opcode, address)
        }
        _ => unreachable!("non-memory load opcode {:?}", inst.opcode),
    };
    ctx.set_value(block_idx, inst_idx, value);
}

/// Dispatch storage/global store IR instructions.
pub fn emit_store(ctx: &mut SpirvEmitContext, inst: &ir::Inst, _block_idx: u32, _inst_idx: u32) {
    match inst.opcode {
        Opcode::WriteStorageU8
        | Opcode::WriteStorageS8
        | Opcode::WriteStorageU16
        | Opcode::WriteStorageS16
        | Opcode::WriteStorage32
        | Opcode::WriteStorage64
        | Opcode::WriteStorage128 => {
            let value = ctx.resolve_value(inst.arg(2));
            emit_write_storage(ctx, inst.opcode, *inst.arg(0), *inst.arg(1), value);
        }
        Opcode::WriteGlobalU8
        | Opcode::WriteGlobalS8
        | Opcode::WriteGlobalU16
        | Opcode::WriteGlobalS16 => {
            panic!("SPIR-V global subword stores are not implemented upstream")
        }
        Opcode::WriteGlobal32 | Opcode::WriteGlobal64 | Opcode::WriteGlobal128 => {
            let address = ctx.resolve_value(inst.arg(0));
            let value = ctx.resolve_value(inst.arg(1));
            emit_write_global(ctx, inst.opcode, address, value);
        }
        _ => unreachable!("non-memory store opcode {:?}", inst.opcode),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::bindings::Bindings;
    use crate::ir::types::{ShaderStage, Type};
    use crate::profile::Profile;
    use crate::runtime_info::RuntimeInfo;
    use crate::shader_info::StorageBufferDescriptor;
    use rspirv::spirv;

    fn context(profile: Profile, used_types: u32, uses_int8: bool) -> SpirvEmitContext {
        let mut program = ir::Program::new(ShaderStage::Compute);
        program.info.uses_int8 = uses_int8;
        program.info.used_storage_buffer_types = used_types;
        program.info.storage_buffers_descriptors = vec![StorageBufferDescriptor {
            cbuf_index: 0,
            cbuf_offset: 0,
            count: 1,
            is_written: true, ..Default::default() }];
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
        ctx
    }

    fn contains_opcode(ctx: &SpirvEmitContext, opcode: spirv::Op) -> bool {
        ctx.builder
            .module_ref()
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .any(|inst| inst.class.opcode == opcode)
    }

    #[test]
    fn typed_storage_subword_load_uses_narrow_ssbo_view() {
        let profile = Profile {
            support_descriptor_aliasing: true,
            support_int8: true,
            support_storage_buffer_8bit: true,
            ..Profile::default()
        };
        let mut ctx = context(profile, Type::U8 as u32, true);

        let _ = emit_load_storage(
            &mut ctx,
            Opcode::LoadStorageU8,
            Value::ImmU32(0),
            Value::ImmU32(3),
        );
        ctx.builder.ret().unwrap();
        ctx.builder.end_function().unwrap();

        assert!(contains_opcode(&ctx, spirv::Op::Load));
        assert!(contains_opcode(&ctx, spirv::Op::UConvert));
        assert!(!contains_opcode(&ctx, spirv::Op::BitFieldUExtract));
    }

    #[test]
    fn missing_storage_8bit_capability_uses_u32_fallback() {
        let profile = Profile {
            support_descriptor_aliasing: true,
            support_int8: true,
            ..Profile::default()
        };
        let mut ctx = context(profile, Type::U8 as u32, true);

        let _ = emit_load_storage(
            &mut ctx,
            Opcode::LoadStorageU8,
            Value::ImmU32(0),
            Value::ImmU32(3),
        );
        ctx.builder.ret().unwrap();
        ctx.builder.end_function().unwrap();

        assert!(contains_opcode(&ctx, spirv::Op::BitFieldUExtract));
        assert!(!contains_opcode(&ctx, spirv::Op::UConvert));
    }

    #[test]
    fn unsupported_storage_subword_store_calls_cas_helper() {
        let mut ctx = context(Profile::default(), Type::U32 as u32, true);
        let value = ctx.constant_u32(0xab);

        emit_write_storage(
            &mut ctx,
            Opcode::WriteStorageU8,
            Value::ImmU32(0),
            Value::ImmU32(3),
            value,
        );
        ctx.builder.ret().unwrap();
        ctx.builder.end_function().unwrap();

        assert!(contains_opcode(&ctx, spirv::Op::FunctionCall));
        assert!(contains_opcode(&ctx, spirv::Op::AtomicCompareExchange));
    }
}
