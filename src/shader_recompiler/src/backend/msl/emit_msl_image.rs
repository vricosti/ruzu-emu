// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! MSL sampled-image emission.
//!
//! This file owns the native-MSL counterparts of Eden's
//! `backend/spirv/emit_spirv_image.cpp` operations.

use crate::ir::instruction::Inst;
use crate::ir::opcodes::Opcode;
use crate::ir::program::Program;
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

fn query_lod_coordinates(texture_type: TextureType, coords: String) -> Result<String, MslError> {
    match texture_type {
        TextureType::Color1D | TextureType::ColorArray1D => Err(
            MslError::UnsupportedProgramFeature("texture LOD query on a Metal 1D texture"),
        ),
        TextureType::Color2D | TextureType::Color2DRect => Ok(coords),
        TextureType::ColorArray2D => Ok(format!("({coords}).xy")),
        TextureType::Color3D | TextureType::ColorCube => Ok(coords),
        TextureType::ColorArrayCube => Ok(format!("({coords}).xyz")),
        TextureType::Buffer => Err(MslError::UnsupportedProgramFeature(
            "texture buffer LOD query",
        )),
    }
}

fn resolve_ir_value(program: &Program, mut value: Value) -> Value {
    while let Value::Inst(inst_ref) = value {
        let inst = program.block(inst_ref.block).inst(inst_ref.inst);
        if inst.opcode != Opcode::Identity || inst.args.is_empty() {
            break;
        }
        value = inst.args[0];
    }
    value
}

fn immediate_offset_components(program: &Program, offset: Value) -> Option<Vec<i32>> {
    match resolve_ir_value(program, offset) {
        Value::ImmU32(value) => Some(vec![value as i32]),
        Value::Inst(inst_ref) => {
            let inst = program.block(inst_ref.block).inst(inst_ref.inst);
            let count = match inst.opcode {
                Opcode::CompositeConstructU32x2 => 2,
                Opcode::CompositeConstructU32x3 => 3,
                Opcode::CompositeConstructU32x4 => 4,
                _ => return None,
            };
            inst.args
                .iter()
                .take(count)
                .map(|value| match resolve_ir_value(program, *value) {
                    Value::ImmU32(value) => Some(value as i32),
                    _ => None,
                })
                .collect()
        }
        _ => None,
    }
}

fn gradient_expression(
    context: &MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    texture_type: TextureType,
) -> Result<Option<String>, MslError> {
    let info = TextureInstInfo::from_u32(inst.flags);
    let derivatives = context.value_expression(inst.arg(2), inst_ref, 2)?;
    let expected_derivatives = match texture_type {
        TextureType::Color1D | TextureType::ColorArray1D => 1,
        TextureType::Color2D | TextureType::Color2DRect | TextureType::ColorArray2D => 2,
        TextureType::Color3D | TextureType::ColorCube | TextureType::ColorArrayCube => 3,
        TextureType::Buffer => {
            return Err(MslError::UnsupportedProgramFeature(
                "texture buffer gradient",
            ));
        }
    };
    if info.num_derivatives != expected_derivatives {
        return Err(MslError::UnsupportedProgramFeature(
            "texture gradient derivative count mismatch",
        ));
    }
    Ok(Some(match info.num_derivatives {
        // MSL has no gradient1d type or texture1d gradient overload.
        // SPIRV-Cross lowers this exact case to an implicit sample as well.
        1 => return Ok(None),
        2 => format!(
            "gradient2d(float2(({derivatives}).x, ({derivatives}).z), float2(({derivatives}).y, ({derivatives}).w))"
        ),
        3 => {
            let second = context.value_expression(inst.arg(3), inst_ref, 3)?;
            let gradient = if matches!(
                texture_type,
                TextureType::ColorCube | TextureType::ColorArrayCube
            ) {
                "gradientcube"
            } else {
                "gradient3d"
            };
            format!(
                "{gradient}(float3(({derivatives}).x, ({derivatives}).z, ({second}).x), float3(({derivatives}).y, ({derivatives}).w, ({second}).y))"
            )
        }
        _ => {
            return Err(MslError::UnsupportedProgramFeature(
                "texture gradient derivative count",
            ));
        }
    }))
}

fn gradient_offset_expression(
    program: &Program,
    texture_type: TextureType,
    offset: Value,
) -> Result<Option<String>, MslError> {
    if matches!(offset, Value::Void) {
        return Ok(None);
    }
    let Some(components) = immediate_offset_components(program, offset) else {
        // Upstream only adds ConstOffset here. Runtime TXD offsets are
        // deliberately omitted when they cannot be proven immediate.
        return Ok(None);
    };
    let spatial_components = match texture_type {
        TextureType::Color1D | TextureType::ColorArray1D => 1,
        TextureType::Color2D | TextureType::Color2DRect | TextureType::ColorArray2D => 2,
        TextureType::Color3D => 3,
        TextureType::ColorCube | TextureType::ColorArrayCube => {
            return Err(MslError::UnsupportedProgramFeature(
                "cube texture gradient offset",
            ));
        }
        TextureType::Buffer => {
            return Err(MslError::UnsupportedProgramFeature(
                "texture buffer gradient offset",
            ));
        }
    };
    if components.len() != spatial_components {
        return Err(MslError::UnsupportedProgramFeature(
            "texture gradient offset component count",
        ));
    }
    let expression = if spatial_components == 1 {
        components[0].to_string()
    } else {
        format!(
            "int{}({})",
            spatial_components,
            components
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    Ok(Some(expression))
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

pub fn emit_image_query_lod(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    if context.stage() != Stage::Fragment {
        return Err(MslError::UnsupportedProgramFeature(
            "texture LOD query outside a fragment shader",
        ));
    }
    if !context.supports_query_texture_lod() {
        return Err(MslError::UnsupportedProgramFeature(
            "texture LOD query on the selected Metal device",
        ));
    }
    let info = TextureInstInfo::from_u32(inst.flags);
    context.validate_texture(info)?;
    let texture = context.texture_expressions(info, inst.arg(0), inst_ref)?;
    if texture.is_multisample {
        return Err(MslError::UnsupportedProgramFeature(
            "multisample texture LOD query",
        ));
    }
    let coords = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let coords = query_lod_coordinates(texture.texture_type, coords)?;
    let clamped = format!(
        "{}.calculate_clamped_lod({}, {coords})",
        texture.texture, texture.sampler
    );
    let unclamped = format!(
        "{}.calculate_unclamped_lod({}, {coords})",
        texture.texture, texture.sampler
    );
    context.define(
        inst_ref,
        Type::F32x4,
        format!("float4({clamped}, {unclamped}, 0.0f, 0.0f)"),
        false,
    )
}

pub fn emit_image_gradient(
    context: &mut MslEmitContext,
    program: &Program,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    if inst
        .get_associated_pseudo(Opcode::GetSparseFromOp)
        .is_some()
    {
        return Err(MslError::UnsupportedProgramFeature(
            "sparse texture gradient",
        ));
    }
    let info = TextureInstInfo::from_u32(inst.flags);
    context.validate_texture(info)?;
    let texture = context.texture_expressions(info, inst.arg(0), inst_ref)?;
    if texture.is_depth || info.is_depth {
        return Err(MslError::UnsupportedProgramFeature(
            "depth texture used by a color gradient sample",
        ));
    }
    if texture.is_multisample {
        return Err(MslError::UnsupportedProgramFeature(
            "multisample texture gradient",
        ));
    }
    let coords = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let mut arguments = vec![texture.sampler.clone()];
    append_sample_coordinates(&mut arguments, texture.texture_type, coords)?;
    let gradient = gradient_expression(context, inst_ref, inst, texture.texture_type)?;
    if gradient.is_none() && (!matches!(inst.arg(3), Value::Void) || info.has_lod_clamp) {
        return Err(MslError::UnsupportedProgramFeature(
            "Metal 1D texture gradient operands",
        ));
    }
    if let Some(gradient) = gradient {
        arguments.push(gradient);
    }
    let offset = if info.num_derivatives != 3 {
        gradient_offset_expression(program, texture.texture_type, *inst.arg(3))?
    } else {
        None
    };
    if info.has_lod_clamp {
        let clamp = context.value_expression(inst.arg(4), inst_ref, 4)?;
        arguments.push(format!("min_lod_clamp({clamp})"));
    }
    if let Some(offset) = offset {
        arguments.push(offset);
    }
    let sample = format!("{}.sample({})", texture.texture, arguments.join(", "));
    let expression = if texture.is_integer {
        format!("as_type<float4>({sample})")
    } else {
        sample
    };
    context.define(inst_ref, Type::F32x4, expression, false)
}
