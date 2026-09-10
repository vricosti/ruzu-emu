// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SPIR-V image atomic emission — maps to zuyu's
//! `backend/spirv/emit_spirv_image_atomic.cpp`.

use super::spirv_emit_context::SpirvEmitContext;
use crate::ir::types::TextureInstInfo;
use crate::ir::value::Value;
use crate::ir::{self, Opcode};
use rspirv::spirv::{self, Word};

#[derive(Clone, Copy)]
enum ImageAtomicOp {
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

/// Port of upstream `Image` in `emit_spirv_image_atomic.cpp`.
fn image(ctx: &SpirvEmitContext, info: TextureInstInfo) -> Word {
    if crate::shader_info::TextureType::from_u8(info.texture_type)
        == crate::shader_info::TextureType::Buffer
    {
        ctx.image_buffers
            .get(info.descriptor_index as usize)
            .expect("SPIR-V: missing image-buffer descriptor")
            .id
    } else {
        ctx.images
            .get(info.descriptor_index as usize)
            .expect("SPIR-V: missing image descriptor")
            .id
    }
}

/// Port of upstream `AtomicArgs`.
fn atomic_args(ctx: &mut SpirvEmitContext) -> (Word, Word) {
    let scope = ctx.constant_u32(spirv::Scope::Device as u32);
    (scope, ctx.const_zero_u32)
}

/// Port of upstream `ImageAtomicU32`.
fn image_atomic_u32(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    index: Value,
    coords: Word,
    value: Word,
    operation: ImageAtomicOp,
) -> Word {
    if !index.is_void() && (!index.is_immediate() || index.imm_u32() != 0) {
        panic!("SPIR-V: image indexing is not implemented");
    }
    assert_ne!(
        ctx.image_u32, 0,
        "SPIR-V: atomic image pointer type was not declared"
    );
    let info = TextureInstInfo::from_u32(inst.flags);
    let image = image(ctx, info);
    let sample = ctx.const_zero_u32;
    let pointer = ctx
        .builder
        .image_texel_pointer(ctx.image_u32, None, image, coords, sample)
        .unwrap();
    let (scope, semantics) = atomic_args(ctx);
    match operation {
        ImageAtomicOp::IAdd => {
            ctx.builder
                .atomic_i_add(ctx.u32_type, None, pointer, scope, semantics, value)
        }
        ImageAtomicOp::SMin => {
            ctx.builder
                .atomic_s_min(ctx.u32_type, None, pointer, scope, semantics, value)
        }
        ImageAtomicOp::UMin => {
            ctx.builder
                .atomic_u_min(ctx.u32_type, None, pointer, scope, semantics, value)
        }
        ImageAtomicOp::SMax => {
            ctx.builder
                .atomic_s_max(ctx.u32_type, None, pointer, scope, semantics, value)
        }
        ImageAtomicOp::UMax => {
            ctx.builder
                .atomic_u_max(ctx.u32_type, None, pointer, scope, semantics, value)
        }
        ImageAtomicOp::And => {
            ctx.builder
                .atomic_and(ctx.u32_type, None, pointer, scope, semantics, value)
        }
        ImageAtomicOp::Or => {
            ctx.builder
                .atomic_or(ctx.u32_type, None, pointer, scope, semantics, value)
        }
        ImageAtomicOp::Xor => {
            ctx.builder
                .atomic_xor(ctx.u32_type, None, pointer, scope, semantics, value)
        }
        ImageAtomicOp::Exchange => {
            ctx.builder
                .atomic_exchange(ctx.u32_type, None, pointer, scope, semantics, value)
        }
    }
    .unwrap()
}

pub fn emit_image_atomic_iadd_32(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    index: Value,
    coords: Word,
    value: Word,
) -> Word {
    image_atomic_u32(ctx, inst, index, coords, value, ImageAtomicOp::IAdd)
}

pub fn emit_image_atomic_smin_32(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    index: Value,
    coords: Word,
    value: Word,
) -> Word {
    image_atomic_u32(ctx, inst, index, coords, value, ImageAtomicOp::SMin)
}

pub fn emit_image_atomic_umin_32(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    index: Value,
    coords: Word,
    value: Word,
) -> Word {
    image_atomic_u32(ctx, inst, index, coords, value, ImageAtomicOp::UMin)
}

pub fn emit_image_atomic_smax_32(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    index: Value,
    coords: Word,
    value: Word,
) -> Word {
    image_atomic_u32(ctx, inst, index, coords, value, ImageAtomicOp::SMax)
}

pub fn emit_image_atomic_umax_32(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    index: Value,
    coords: Word,
    value: Word,
) -> Word {
    image_atomic_u32(ctx, inst, index, coords, value, ImageAtomicOp::UMax)
}

pub fn emit_image_atomic_inc_32(
    _ctx: &mut SpirvEmitContext,
    _inst: &ir::Inst,
    _index: Value,
    _coords: Word,
    _value: Word,
) -> Word {
    panic!("SPIR-V: ImageAtomicInc32 is not implemented upstream")
}

pub fn emit_image_atomic_dec_32(
    _ctx: &mut SpirvEmitContext,
    _inst: &ir::Inst,
    _index: Value,
    _coords: Word,
    _value: Word,
) -> Word {
    panic!("SPIR-V: ImageAtomicDec32 is not implemented upstream")
}

pub fn emit_image_atomic_and_32(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    index: Value,
    coords: Word,
    value: Word,
) -> Word {
    image_atomic_u32(ctx, inst, index, coords, value, ImageAtomicOp::And)
}

pub fn emit_image_atomic_or_32(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    index: Value,
    coords: Word,
    value: Word,
) -> Word {
    image_atomic_u32(ctx, inst, index, coords, value, ImageAtomicOp::Or)
}

pub fn emit_image_atomic_xor_32(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    index: Value,
    coords: Word,
    value: Word,
) -> Word {
    image_atomic_u32(ctx, inst, index, coords, value, ImageAtomicOp::Xor)
}

pub fn emit_image_atomic_exchange_32(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    index: Value,
    coords: Word,
    value: Word,
) -> Word {
    image_atomic_u32(ctx, inst, index, coords, value, ImageAtomicOp::Exchange)
}

/// Dispatch the indexed image atomics produced by upstream `TexturePass`.
pub fn emit_image_atomic(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    block_idx: u32,
    inst_idx: u32,
) {
    let index = *inst.arg(0);
    let coords = ctx.resolve_value(inst.arg(1));
    let value = ctx.resolve_value(inst.arg(2));
    let result = match inst.opcode {
        Opcode::ImageAtomicIAdd32 => emit_image_atomic_iadd_32(ctx, inst, index, coords, value),
        Opcode::ImageAtomicSMin32 => emit_image_atomic_smin_32(ctx, inst, index, coords, value),
        Opcode::ImageAtomicUMin32 => emit_image_atomic_umin_32(ctx, inst, index, coords, value),
        Opcode::ImageAtomicSMax32 => emit_image_atomic_smax_32(ctx, inst, index, coords, value),
        Opcode::ImageAtomicUMax32 => emit_image_atomic_umax_32(ctx, inst, index, coords, value),
        Opcode::ImageAtomicInc32 => emit_image_atomic_inc_32(ctx, inst, index, coords, value),
        Opcode::ImageAtomicDec32 => emit_image_atomic_dec_32(ctx, inst, index, coords, value),
        Opcode::ImageAtomicAnd32 => emit_image_atomic_and_32(ctx, inst, index, coords, value),
        Opcode::ImageAtomicOr32 => emit_image_atomic_or_32(ctx, inst, index, coords, value),
        Opcode::ImageAtomicXor32 => emit_image_atomic_xor_32(ctx, inst, index, coords, value),
        Opcode::ImageAtomicExchange32 => {
            emit_image_atomic_exchange_32(ctx, inst, index, coords, value)
        }
        _ => panic!("SPIR-V: invalid indexed image atomic {:?}", inst.opcode),
    };
    ctx.set_value(block_idx, inst_idx, result);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::basic_block::Block;
    use crate::ir::instruction::Inst;
    use crate::ir::types::ShaderStage;
    use crate::ir::value::InstRef;
    use crate::ir::{Program, SyntaxNode};
    use crate::profile::Profile;
    use crate::runtime_info::RuntimeInfo;
    use crate::shader_info::{ImageDescriptor, ImageFormat, TextureType};
    use rspirv::binary::Assemble;

    fn validate_with_external_tool(ctx: SpirvEmitContext) {
        let Some(validator) = std::env::var_os("RUZU_SPIRV_VAL") else {
            return;
        };
        let words = ctx.builder.module().assemble();
        let path = std::env::temp_dir().join(format!(
            "ruzu-image-atomics-{}-{}.spv",
            std::process::id(),
            words.len()
        ));
        let mut bytes = Vec::with_capacity(words.len() * 4);
        for word in words {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        std::fs::write(&path, bytes).unwrap();
        let output = std::process::Command::new(validator)
            .arg("--target-env")
            .arg("vulkan1.2")
            .arg(&path)
            .output()
            .unwrap();
        let _ = std::fs::remove_file(path);
        assert!(
            output.status.success(),
            "spirv-val failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn indexed_image_atomics_emit_texel_pointer_and_real_atomics() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        program.info.image_descriptors.push(ImageDescriptor {
            texture_type: TextureType::Color2D,
            format: ImageFormat::R32Uint,
            is_written: true,
            is_read: true,
            is_integer: true,
            cbuf_index: 0,
            cbuf_offset: 0,
            count: 1,
            size_shift: 0,
        });
        let info = TextureInstInfo {
            descriptor_index: 0,
            texture_type: TextureType::Color2D as u8,
            image_format: ImageFormat::R32Uint as u8,
            ..TextureInstInfo::default()
        };
        let block = program.block_mut(0);
        let coords = block.append_inst(Inst::new(
            Opcode::CompositeConstructU32x2,
            vec![Value::ImmU32(1), Value::ImmU32(2)],
        ));
        for opcode in [
            Opcode::ImageAtomicIAdd32,
            Opcode::ImageAtomicSMin32,
            Opcode::ImageAtomicUMin32,
            Opcode::ImageAtomicSMax32,
            Opcode::ImageAtomicUMax32,
            Opcode::ImageAtomicAnd32,
            Opcode::ImageAtomicOr32,
            Opcode::ImageAtomicXor32,
            Opcode::ImageAtomicExchange32,
        ] {
            block.append_inst(Inst::with_flags(
                opcode,
                vec![
                    Value::ImmU32(0),
                    Value::Inst(InstRef {
                        block: 0,
                        inst: coords,
                    }),
                    Value::ImmU32(7),
                ],
                info.to_u32(),
            ));
        }
        program.syntax_list = vec![SyntaxNode::Block(0), SyntaxNode::Return];

        // Exercise the production metadata pass rather than supplying the
        // capability by hand: this must declare the image atomic pointer type.
        crate::ir_opt::collect_shader_info_pass::collect_shader_info_pass(&mut program);
        assert!(program.info.uses_atomic_image_u32);

        let mut ctx = SpirvEmitContext::new(&program, &Profile::default(), &RuntimeInfo::default());
        ctx.emit_program(&program);
        let opcodes = ctx
            .builder
            .module_ref()
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .map(|inst| inst.class.opcode)
            .collect::<Vec<_>>();
        assert!(opcodes.contains(&spirv::Op::ImageTexelPointer));
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
            assert!(opcodes.contains(&opcode), "missing {opcode:?}");
        }
        assert!(!opcodes.contains(&spirv::Op::Undef));
        validate_with_external_tool(ctx);
    }
}
