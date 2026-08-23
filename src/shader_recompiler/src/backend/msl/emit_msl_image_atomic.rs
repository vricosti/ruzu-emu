// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native-MSL storage-image atomic emission.
//!
//! This file owns the direct-MSL counterparts of Eden's
//! `backend/spirv/emit_spirv_image_atomic.cpp` operations.

use crate::ir::instruction::Inst;
use crate::ir::opcodes::Opcode;
use crate::ir::types::{TextureInstInfo, Type};
use crate::ir::value::{InstRef, Value};
use crate::shader_info::{ImageFormat, TextureType};

use super::emit_msl_image::append_storage_coordinates;
use super::msl_emit_context::MslEmitContext;
use super::MslError;

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

fn signed_texture_type(texture_type: TextureType) -> Result<&'static str, MslError> {
    match texture_type {
        TextureType::Color1D => Ok("texture1d<int, access::read_write>"),
        TextureType::ColorArray1D => Ok("texture1d_array<int, access::read_write>"),
        TextureType::Color2D => Ok("texture2d<int, access::read_write>"),
        TextureType::ColorArray2D => Ok("texture2d_array<int, access::read_write>"),
        TextureType::Color3D => Ok("texture3d<int, access::read_write>"),
        TextureType::Buffer => Ok("texture_buffer<int, access::read_write>"),
        TextureType::ColorCube | TextureType::ColorArrayCube | TextureType::Color2DRect => Err(
            MslError::UnsupportedProgramFeature("invalid storage-image atomic texture type"),
        ),
    }
}

fn emit_image_atomic_u32(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    operation: ImageAtomicOp,
) -> Result<(), MslError> {
    if !context.supports_texture_atomics() {
        return Err(MslError::UnsupportedProgramFeature(
            "texture atomics on this Metal device",
        ));
    }
    if !matches!(inst.arg(0), Value::ImmU32(0)) {
        // Eden's ImageAtomicU32 rejects descriptor-array indexing. Preserve
        // that contract until the shared backend semantics support layers.
        return Err(MslError::UnsupportedProgramFeature(
            "storage-image atomic descriptor indexing",
        ));
    }
    let info = TextureInstInfo::from_u32(inst.flags);
    if ImageFormat::from_u8(info.image_format) != ImageFormat::R32Uint {
        return Err(MslError::UnsupportedProgramFeature(
            "storage-image atomic format other than R32Uint",
        ));
    }
    let image = context.image_expressions(info, inst.arg(0), inst_ref)?;
    if !image.is_integer {
        return Err(MslError::UnsupportedProgramFeature(
            "storage-image atomic on a non-integer image",
        ));
    }
    let coords = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let value = context.value_expression(inst.arg(2), inst_ref, 2)?;
    let mut arguments = Vec::new();
    append_storage_coordinates(&mut arguments, image.texture_type, coords)?;

    let (target, method, value, signed) = match operation {
        ImageAtomicOp::IAdd => (image.image, "atomic_fetch_add", value, false),
        ImageAtomicOp::SMin => {
            context.require_texture_cast();
            (
                format!(
                    "spvTextureCast<{}>({})",
                    signed_texture_type(image.texture_type)?,
                    image.image
                ),
                "atomic_fetch_min",
                format!("as_type<int>({value})"),
                true,
            )
        }
        ImageAtomicOp::UMin => (image.image, "atomic_fetch_min", value, false),
        ImageAtomicOp::SMax => {
            context.require_texture_cast();
            (
                format!(
                    "spvTextureCast<{}>({})",
                    signed_texture_type(image.texture_type)?,
                    image.image
                ),
                "atomic_fetch_max",
                format!("as_type<int>({value})"),
                true,
            )
        }
        ImageAtomicOp::UMax => (image.image, "atomic_fetch_max", value, false),
        ImageAtomicOp::And => (image.image, "atomic_fetch_and", value, false),
        ImageAtomicOp::Or => (image.image, "atomic_fetch_or", value, false),
        ImageAtomicOp::Xor => (image.image, "atomic_fetch_xor", value, false),
        ImageAtomicOp::Exchange => (image.image, "atomic_exchange", value, false),
    };
    arguments.push(value);
    let atomic = format!("{target}.{method}({}).x", arguments.join(", "));
    let expression = if signed {
        format!("as_type<uint>({atomic})")
    } else {
        atomic
    };
    context.define(inst_ref, Type::U32, expression, false)
}

pub fn emit_image_atomic(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let operation = match inst.opcode {
        Opcode::ImageAtomicIAdd32 => ImageAtomicOp::IAdd,
        Opcode::ImageAtomicSMin32 => ImageAtomicOp::SMin,
        Opcode::ImageAtomicUMin32 => ImageAtomicOp::UMin,
        Opcode::ImageAtomicSMax32 => ImageAtomicOp::SMax,
        Opcode::ImageAtomicUMax32 => ImageAtomicOp::UMax,
        Opcode::ImageAtomicAnd32 => ImageAtomicOp::And,
        Opcode::ImageAtomicOr32 => ImageAtomicOp::Or,
        Opcode::ImageAtomicXor32 => ImageAtomicOp::Xor,
        Opcode::ImageAtomicExchange32 => ImageAtomicOp::Exchange,
        Opcode::ImageAtomicInc32 | Opcode::ImageAtomicDec32 => {
            return Err(MslError::UnsupportedOpcode {
                block: inst_ref.block,
                inst: inst_ref.inst,
                opcode: inst.opcode,
            });
        }
        _ => {
            return Err(MslError::UnsupportedOpcode {
                block: inst_ref.block,
                inst: inst_ref.inst,
                opcode: inst.opcode,
            });
        }
    };
    emit_image_atomic_u32(context, inst_ref, inst, operation)
}
