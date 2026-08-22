// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! MSL sampled-image emission.
//!
//! This file owns the native-MSL counterparts of Eden's
//! `backend/spirv/emit_spirv_image.cpp` operations.

use crate::ir::instruction::Inst;
use crate::ir::opcodes::Opcode;
use crate::ir::types::{TextureInstInfo, Type};
use crate::ir::value::{InstRef, Value};
use crate::shader_info::TextureType;
use crate::stage::Stage;

use super::msl_emit_context::MslEmitContext;
use super::MslError;

fn validate_sample(context: &MslEmitContext, inst: &Inst) -> Result<TextureInstInfo, MslError> {
    let info = TextureInstInfo::from_u32(inst.flags);
    if TextureType::from_u8(info.texture_type) != TextureType::Color2D {
        return Err(MslError::UnsupportedProgramFeature(
            "sampled texture type other than Color2D",
        ));
    }
    if info.is_depth {
        return Err(MslError::UnsupportedProgramFeature(
            "depth texture sampling",
        ));
    }
    if info.has_bias {
        return Err(MslError::UnsupportedProgramFeature("texture LOD bias"));
    }
    if info.has_lod_clamp {
        return Err(MslError::UnsupportedProgramFeature("texture LOD clamp"));
    }
    if info.ndv_is_active {
        return Err(MslError::UnsupportedProgramFeature(
            "texture non-dependent value tracking",
        ));
    }
    let offset_arg = match inst.opcode {
        Opcode::ImageSampleImplicitLod | Opcode::ImageSampleExplicitLod => 3,
        _ => unreachable!("sample validation called for a non-sample opcode"),
    };
    if !matches!(inst.arg(offset_arg), Value::Void) {
        return Err(MslError::UnsupportedProgramFeature("texture sample offset"));
    }
    context.validate_texture(info)?;
    Ok(info)
}

pub fn emit_image_sample(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let info = validate_sample(context, inst)?;
    let (texture, sampler, is_integer) =
        context.texture_expressions(info, inst.arg(0), inst_ref)?;
    let coords = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let lod = match inst.opcode {
        Opcode::ImageSampleExplicitLod => {
            let lod = context.value_expression(inst.arg(2), inst_ref, 2)?;
            Some(lod)
        }
        Opcode::ImageSampleImplicitLod if context.stage() != Stage::Fragment => {
            Some("0.0f".to_owned())
        }
        Opcode::ImageSampleImplicitLod => None,
        _ => unreachable!("image sample emitter called for a non-sample opcode"),
    };
    let sample = if let Some(lod) = lod {
        format!("{texture}.sample({sampler}, {coords}, level({lod}))")
    } else {
        format!("{texture}.sample({sampler}, {coords})")
    };
    let expression = if is_integer {
        format!("as_type<float4>({sample})")
    } else {
        sample
    };
    context.define(inst_ref, Type::F32x4, expression, false)
}
