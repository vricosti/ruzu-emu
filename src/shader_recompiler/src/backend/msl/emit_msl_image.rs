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

fn append_sample_coordinates(
    arguments: &mut Vec<String>,
    texture_type: TextureType,
    coords: String,
) -> Result<(), MslError> {
    match texture_type {
        TextureType::Color1D => arguments.push(coords),
        TextureType::ColorArray1D => {
            arguments.push(format!("({coords}).x"));
            arguments.push(format!("uint(({coords}).y)"));
        }
        TextureType::Color2D | TextureType::Color2DRect => arguments.push(coords),
        TextureType::ColorArray2D => {
            arguments.push(format!("({coords}).xy"));
            arguments.push(format!("uint(({coords}).z)"));
        }
        TextureType::Color3D | TextureType::ColorCube => arguments.push(coords),
        TextureType::ColorArrayCube => {
            arguments.push(format!("({coords}).xyz"));
            arguments.push(format!("uint(({coords}).w)"));
        }
        TextureType::Buffer => {
            return Err(MslError::UnsupportedProgramFeature(
                "texture buffer sampling",
            ));
        }
    }
    Ok(())
}

fn add_offset_to_coordinates(
    context: &MslEmitContext,
    inst_ref: InstRef,
    info: TextureInstInfo,
    coords: String,
    offset: &Value,
) -> Result<String, MslError> {
    if matches!(offset, Value::Void) {
        return Ok(coords);
    }
    let offset = context.value_expression(offset, inst_ref, 2)?;
    Ok(match TextureType::from_u8(info.texture_type) {
        TextureType::Buffer | TextureType::Color1D => format!("(({coords}) + ({offset}))"),
        TextureType::ColorArray1D => {
            format!("(({coords}) + uint2(({offset}), 0u))")
        }
        TextureType::Color2D | TextureType::Color2DRect => {
            format!("(({coords}) + ({offset}))")
        }
        TextureType::ColorArray2D => {
            format!("(({coords}) + uint3(({offset}).xy, 0u))")
        }
        TextureType::Color3D => format!("(({coords}) + ({offset}))"),
        TextureType::ColorCube | TextureType::ColorArrayCube => coords,
    })
}

fn append_fetch_coordinates(
    arguments: &mut Vec<String>,
    texture_type: TextureType,
    coords: String,
) -> Result<(), MslError> {
    match texture_type {
        TextureType::Color1D => arguments.push(coords),
        TextureType::ColorArray1D => {
            arguments.push(format!("({coords}).x"));
            arguments.push(format!("({coords}).y"));
        }
        TextureType::Color2D | TextureType::Color2DRect => arguments.push(coords),
        TextureType::ColorArray2D => {
            arguments.push(format!("({coords}).xy"));
            arguments.push(format!("({coords}).z"));
        }
        TextureType::Color3D => arguments.push(coords),
        TextureType::ColorCube => {
            arguments.push(format!("({coords}).xy"));
            arguments.push(format!("({coords}).z"));
        }
        TextureType::ColorArrayCube => {
            arguments.push(format!("({coords}).xy"));
            arguments.push(format!("({coords}).z"));
            arguments.push(format!("({coords}).w"));
        }
        TextureType::Buffer => {
            return Err(MslError::UnsupportedProgramFeature("texture buffer fetch"));
        }
    }
    Ok(())
}

fn validate_sample(context: &MslEmitContext, inst: &Inst) -> Result<TextureInstInfo, MslError> {
    let info = TextureInstInfo::from_u32(inst.flags);
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
    let texture = context.texture_expressions(info, inst.arg(0), inst_ref)?;
    if texture.is_depth || info.is_depth {
        return Err(MslError::UnsupportedProgramFeature(
            "depth texture used by a color sample",
        ));
    }
    let coords = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let mut arguments = vec![texture.sampler.clone()];
    append_sample_coordinates(&mut arguments, texture.texture_type, coords)?;
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
    if let Some(lod) = lod.filter(|_| {
        !matches!(
            texture.texture_type,
            TextureType::Color1D | TextureType::ColorArray1D
        )
    }) {
        arguments.push(format!("level({lod})"));
    }
    let sample = format!("{}.sample({})", texture.texture, arguments.join(", "));
    let expression = if texture.is_integer {
        format!("as_type<float4>({sample})")
    } else {
        sample
    };
    context.define(inst_ref, Type::F32x4, expression, false)
}

pub fn emit_image_sample_dref(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let info = TextureInstInfo::from_u32(inst.flags);
    if !info.is_depth {
        return Err(MslError::UnsupportedProgramFeature(
            "color texture used by a depth-reference sample",
        ));
    }
    if info.has_bias {
        return Err(MslError::UnsupportedProgramFeature(
            "depth texture LOD bias",
        ));
    }
    if info.has_lod_clamp {
        return Err(MslError::UnsupportedProgramFeature(
            "depth texture LOD clamp",
        ));
    }
    if info.ndv_is_active {
        return Err(MslError::UnsupportedProgramFeature(
            "depth texture non-dependent value tracking",
        ));
    }
    let offset_arg = match inst.opcode {
        Opcode::ImageSampleDrefImplicitLod | Opcode::ImageSampleDrefExplicitLod => 4,
        _ => unreachable!("depth sample emitter called for a non-depth-sample opcode"),
    };
    if !matches!(inst.arg(offset_arg), Value::Void) {
        return Err(MslError::UnsupportedProgramFeature(
            "depth texture sample offset",
        ));
    }
    context.validate_texture(info)?;
    let texture = context.texture_expressions(info, inst.arg(0), inst_ref)?;
    if !texture.is_depth {
        return Err(MslError::UnsupportedProgramFeature(
            "non-depth descriptor used by a depth-reference sample",
        ));
    }
    let coords = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let dref = context.value_expression(inst.arg(2), inst_ref, 2)?;
    let mut arguments = vec![texture.sampler.clone()];
    append_sample_coordinates(&mut arguments, texture.texture_type, coords)?;
    arguments.push(dref);
    let lod = match inst.opcode {
        Opcode::ImageSampleDrefExplicitLod => {
            Some(context.value_expression(inst.arg(3), inst_ref, 3)?)
        }
        Opcode::ImageSampleDrefImplicitLod if context.stage() != Stage::Fragment => {
            Some("0.0f".to_owned())
        }
        Opcode::ImageSampleDrefImplicitLod => None,
        _ => unreachable!("depth sample emitter called for a non-depth-sample opcode"),
    };
    if let Some(lod) = lod {
        arguments.push(format!("level({lod})"));
    }
    context.define(
        inst_ref,
        Type::F32,
        format!(
            "{}.sample_compare({})",
            texture.texture,
            arguments.join(", ")
        ),
        false,
    )
}

pub fn emit_image_fetch(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let info = TextureInstInfo::from_u32(inst.flags);
    context.validate_texture(info)?;
    let texture = context.texture_expressions(info, inst.arg(0), inst_ref)?;
    let coords = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let coords = add_offset_to_coordinates(context, inst_ref, info, coords, inst.arg(2))?;
    let mut arguments = Vec::with_capacity(4);
    append_fetch_coordinates(&mut arguments, texture.texture_type, coords)?;

    let sample = (!matches!(inst.arg(4), Value::Void))
        .then(|| context.value_expression(inst.arg(4), inst_ref, 4))
        .transpose()?;
    if texture.is_multisample != sample.is_some() {
        return Err(MslError::UnsupportedProgramFeature(
            "texture fetch multisample descriptor/instruction mismatch",
        ));
    }
    if let Some(sample) = sample {
        arguments.push(sample);
    } else if !matches!(inst.arg(3), Value::Void)
        && !matches!(
            texture.texture_type,
            TextureType::Color1D | TextureType::ColorArray1D
        )
    {
        arguments.push(context.value_expression(inst.arg(3), inst_ref, 3)?);
    }

    let read = format!("{}.read({})", texture.texture, arguments.join(", "));
    let expression = if texture.is_depth {
        format!("float4({read})")
    } else if texture.is_integer {
        format!("as_type<float4>({read})")
    } else {
        read
    };
    context.define(inst_ref, Type::F32x4, expression, false)
}

pub fn emit_image_query_dimensions(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let info = TextureInstInfo::from_u32(inst.flags);
    context.validate_texture(info)?;
    let texture = context.texture_expressions(info, inst.arg(0), inst_ref)?;
    if texture.texture_type == TextureType::Buffer {
        return Err(MslError::UnsupportedProgramFeature(
            "texture buffer dimension query",
        ));
    }
    let lod = if texture.is_multisample
        || matches!(
            texture.texture_type,
            TextureType::Color1D | TextureType::ColorArray1D
        ) {
        None
    } else {
        Some(context.value_expression(inst.arg(1), inst_ref, 1)?)
    };
    let query = |method: &str| match &lod {
        Some(lod) => format!("{}.{}({lod})", texture.texture, method),
        None => format!("{}.{}()", texture.texture, method),
    };
    let skip_mips = inst.args.get(2).map(Value::imm_u1).unwrap_or(false);
    let mips = if skip_mips {
        "0u".to_owned()
    } else if texture.is_multisample {
        "1u".to_owned()
    } else {
        format!("{}.get_num_mip_levels()", texture.texture)
    };
    let width = query("get_width");
    let expression = match texture.texture_type {
        TextureType::Color1D => format!("uint4({width}, 0u, 0u, {mips})"),
        TextureType::ColorArray1D => format!(
            "uint4({width}, {}.get_array_size(), 0u, {mips})",
            texture.texture
        ),
        TextureType::Color2D | TextureType::Color2DRect | TextureType::ColorCube => {
            format!("uint4({width}, {}, 0u, {mips})", query("get_height"))
        }
        TextureType::ColorArray2D | TextureType::ColorArrayCube => format!(
            "uint4({width}, {}, {}.get_array_size(), {mips})",
            query("get_height"),
            texture.texture
        ),
        TextureType::Color3D => format!(
            "uint4({width}, {}, {}, {mips})",
            query("get_height"),
            query("get_depth")
        ),
        TextureType::Buffer => unreachable!("texture buffers were rejected above"),
    };
    context.define(inst_ref, Type::U32x4, expression, false)
}
