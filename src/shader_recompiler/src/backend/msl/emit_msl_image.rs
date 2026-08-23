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

pub(super) fn append_storage_coordinates(
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
        TextureType::Color2D => arguments.push(coords),
        TextureType::ColorArray2D => {
            arguments.push(format!("({coords}).xy"));
            arguments.push(format!("({coords}).z"));
        }
        TextureType::Color3D => arguments.push(coords),
        TextureType::Buffer => {
            return Err(MslError::UnsupportedProgramFeature(
                "image buffer in storage image operation",
            ));
        }
        TextureType::ColorCube | TextureType::ColorArrayCube | TextureType::Color2DRect => {
            return Err(MslError::UnsupportedProgramFeature(
                "invalid storage image texture type",
            ));
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

enum GatherOffsets {
    None,
    Single(String),
    Ptp([[i32; 2]; 4]),
}

fn gather_offsets(
    context: &MslEmitContext,
    program: &Program,
    inst_ref: InstRef,
    texture_type: TextureType,
    offset: Value,
    offset2: Value,
) -> Result<GatherOffsets, MslError> {
    let supports_offsets = matches!(
        texture_type,
        TextureType::Color2D | TextureType::Color2DRect | TextureType::ColorArray2D
    );
    if matches!(offset2, Value::Void) {
        if matches!(offset, Value::Void) {
            return Ok(GatherOffsets::None);
        }
        if !supports_offsets {
            return Err(MslError::UnsupportedProgramFeature(
                "texture gather offset for this Metal texture dimension",
            ));
        }
        if let Some(components) = immediate_offset_components(program, offset) {
            if components.len() != 2 {
                return Err(MslError::UnsupportedProgramFeature(
                    "texture gather offset component count",
                ));
            }
            return Ok(GatherOffsets::Single(format!(
                "int2({}, {})",
                components[0], components[1]
            )));
        }
        let expression = context.value_expression(&offset, inst_ref, 2)?;
        return Ok(GatherOffsets::Single(format!("int2({expression})")));
    }

    if !supports_offsets {
        return Err(MslError::UnsupportedProgramFeature(
            "PTP gather for this Metal texture dimension",
        ));
    }
    let first = immediate_offset_components(program, offset);
    let second = immediate_offset_components(program, offset2);
    let (Some(first), Some(second)) = (first, second) else {
        // Upstream ignores a PTP operand unless all eight components are
        // immediate. Preserve that behavior instead of inventing dynamic PTP.
        log::warn!("MSL: not all arguments in PTP are immediate, ignoring");
        return Ok(GatherOffsets::None);
    };
    if first.len() != 4 || second.len() != 4 {
        return Err(MslError::UnsupportedProgramFeature(
            "invalid PTP gather operands",
        ));
    }
    Ok(GatherOffsets::Ptp([
        [first[0], first[1]],
        [first[2], first[3]],
        [second[0], second[1]],
        [second[2], second[3]],
    ]))
}

fn gather_coordinates(
    context: &MslEmitContext,
    texture: &str,
    texture_type: TextureType,
    coords: String,
) -> String {
    if !context.need_gather_subpixel_offset() {
        return coords;
    }
    let nudge = format!(
        "(float2(0.001953125f) / float2(float({texture}.get_width(0u)), float({texture}.get_height(0u))))"
    );
    match texture_type {
        TextureType::Color2D | TextureType::Color2DRect => {
            format!("(({coords}) + {nudge})")
        }
        TextureType::ColorArray2D | TextureType::ColorCube => {
            format!("float3(({coords}).xy + {nudge}, ({coords}).z)")
        }
        _ => coords,
    }
}

fn gather_arguments(
    texture_type: TextureType,
    sampler: &str,
    coords: &str,
) -> Result<Vec<String>, MslError> {
    let mut arguments = vec![sampler.to_owned()];
    match texture_type {
        TextureType::Color2D | TextureType::Color2DRect => {
            arguments.push(coords.to_owned());
        }
        TextureType::ColorArray2D => {
            arguments.push(format!("({coords}).xy"));
            arguments.push(format!("uint(({coords}).z)"));
        }
        TextureType::ColorCube => arguments.push(coords.to_owned()),
        TextureType::ColorArrayCube => {
            arguments.push(format!("({coords}).xyz"));
            arguments.push(format!("uint(({coords}).w)"));
        }
        TextureType::Color1D | TextureType::ColorArray1D | TextureType::Color3D => {
            return Err(MslError::UnsupportedProgramFeature(
                "texture gather for this Metal texture dimension",
            ));
        }
        TextureType::Buffer => {
            return Err(MslError::UnsupportedProgramFeature("texture buffer gather"));
        }
    }
    Ok(arguments)
}

fn gather_component(component: u8) -> Result<&'static str, MslError> {
    match component {
        0 => Ok("component::x"),
        1 => Ok("component::y"),
        2 => Ok("component::z"),
        3 => Ok("component::w"),
        _ => Err(MslError::UnsupportedProgramFeature(
            "texture gather component",
        )),
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

fn sample_offset_expression(
    program: &Program,
    texture_type: TextureType,
    offset: Value,
) -> Result<Option<String>, MslError> {
    if matches!(offset, Value::Void) {
        return Ok(None);
    }
    let Some(components) = immediate_offset_components(program, offset) else {
        // Eden's ImageOperands only emits ConstOffset for sampling. A
        // non-immediate operand is deliberately omitted rather than lowered
        // as a runtime offset.
        return Ok(None);
    };
    let spatial_components = match texture_type {
        // Native Metal 1D sampling has no offset overload. SPIRV-Cross drops
        // the qualifier unless 1D textures are represented as 2D.
        TextureType::Color1D | TextureType::ColorArray1D => return Ok(None),
        TextureType::Color2D | TextureType::Color2DRect | TextureType::ColorArray2D => 2,
        TextureType::Color3D => 3,
        TextureType::ColorCube | TextureType::ColorArrayCube => return Ok(None),
        TextureType::Buffer => {
            return Err(MslError::UnsupportedProgramFeature(
                "texture buffer sample offset",
            ));
        }
    };
    if components.len() < spatial_components {
        return Err(MslError::UnsupportedProgramFeature(
            "texture sample offset component count",
        ));
    }
    Ok(Some(format!(
        "int{}({})",
        spatial_components,
        components
            .iter()
            .take(spatial_components)
            .map(i32::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

fn append_sample_operands(
    arguments: &mut Vec<String>,
    context: &MslEmitContext,
    program: &Program,
    inst_ref: InstRef,
    info: TextureInstInfo,
    texture_type: TextureType,
    explicit_lod: bool,
    implicit_non_fragment_lod_clamp: bool,
    lod_or_bias: &Value,
    lod_arg_index: u32,
    offset: Value,
) -> Result<(), MslError> {
    let supports_lod = !matches!(
        texture_type,
        TextureType::Color1D | TextureType::ColorArray1D
    );
    if explicit_lod {
        if supports_lod {
            let lod = context.value_expression(lod_or_bias, inst_ref, lod_arg_index)?;
            arguments.push(format!("level({lod})"));
        }
    } else if context.stage() == Stage::Fragment {
        if info.has_bias && supports_lod {
            let bias_lod_clamp = context.value_expression(lod_or_bias, inst_ref, lod_arg_index)?;
            let bias = if info.has_lod_clamp {
                format!("({bias_lod_clamp}).x")
            } else {
                bias_lod_clamp
            };
            arguments.push(format!("bias({bias})"));
        }
        if info.has_lod_clamp {
            let bias_lod_clamp = context.value_expression(lod_or_bias, inst_ref, lod_arg_index)?;
            let lod_clamp = if info.has_bias {
                format!("({bias_lod_clamp}).y")
            } else {
                bias_lod_clamp
            };
            arguments.push(format!("min_lod_clamp({lod_clamp})"));
        }
    } else {
        // Maxwell implicit samples outside fragment shaders behave as an
        // explicit level-zero sample. Eden only reuses that zero as MinLod
        // for color samples; the depth-reference path omits MinLod.
        if supports_lod {
            arguments.push("level(0.0f)".to_owned());
        }
        if info.has_lod_clamp && implicit_non_fragment_lod_clamp {
            arguments.push("min_lod_clamp(0.0f)".to_owned());
        }
    }
    if let Some(offset) = sample_offset_expression(program, texture_type, offset)? {
        arguments.push(offset);
    }
    Ok(())
}

fn validate_sample(context: &MslEmitContext, inst: &Inst) -> Result<TextureInstInfo, MslError> {
    let info = TextureInstInfo::from_u32(inst.flags);
    context.validate_texture(info)?;
    Ok(info)
}

pub fn emit_image_sample(
    context: &mut MslEmitContext,
    program: &Program,
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
    append_sample_operands(
        &mut arguments,
        context,
        program,
        inst_ref,
        info,
        texture.texture_type,
        inst.opcode == Opcode::ImageSampleExplicitLod,
        true,
        inst.arg(2),
        2,
        *inst.arg(3),
    )?;
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
    program: &Program,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let info = TextureInstInfo::from_u32(inst.flags);
    if !info.is_depth {
        return Err(MslError::UnsupportedProgramFeature(
            "color texture used by a depth-reference sample",
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
    append_sample_operands(
        &mut arguments,
        context,
        program,
        inst_ref,
        info,
        texture.texture_type,
        inst.opcode == Opcode::ImageSampleDrefExplicitLod,
        false,
        inst.arg(3),
        3,
        *inst.arg(4),
    )?;
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

pub fn emit_image_gather(
    context: &mut MslEmitContext,
    program: &Program,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    if inst
        .get_associated_pseudo(Opcode::GetSparseFromOp)
        .is_some()
    {
        return Err(MslError::UnsupportedProgramFeature("sparse texture gather"));
    }
    let info = TextureInstInfo::from_u32(inst.flags);
    context.validate_texture(info)?;
    let texture = context.texture_expressions(info, inst.arg(0), inst_ref)?;
    if texture.is_multisample {
        return Err(MslError::UnsupportedProgramFeature(
            "multisample texture gather",
        ));
    }
    let is_dref = inst.opcode == Opcode::ImageGatherDref;
    if is_dref != info.is_depth || is_dref != texture.is_depth {
        return Err(MslError::UnsupportedProgramFeature(
            "texture gather depth/color descriptor mismatch",
        ));
    }
    let coords = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let coords = gather_coordinates(context, &texture.texture, texture.texture_type, coords);
    let base_arguments = gather_arguments(texture.texture_type, &texture.sampler, &coords)?;
    let offsets = gather_offsets(
        context,
        program,
        inst_ref,
        texture.texture_type,
        *inst.arg(2),
        *inst.arg(3),
    )?;
    let dref = is_dref
        .then(|| context.value_expression(inst.arg(4), inst_ref, 4))
        .transpose()?;
    let component = (!is_dref)
        .then(|| gather_component(info.gather_component))
        .transpose()?;

    let make_gather = |offset: Option<String>| {
        let mut arguments = base_arguments.clone();
        if let Some(dref) = &dref {
            arguments.push(dref.clone());
        }
        if let Some(offset) = offset {
            arguments.push(offset);
        } else if component.is_some()
            && matches!(
                texture.texture_type,
                TextureType::Color2D | TextureType::Color2DRect | TextureType::ColorArray2D
            )
        {
            // In the Metal 2D gather overload the component follows the
            // optional offset, so selecting a component requires spelling
            // the default offset explicitly.
            arguments.push("int2(0)".to_owned());
        }
        if let Some(component) = component {
            arguments.push(component.to_owned());
        }
        let method = if is_dref { "gather_compare" } else { "gather" };
        format!("{}.{method}({})", texture.texture, arguments.join(", "))
    };

    let gathered = match offsets {
        GatherOffsets::None => make_gather(None),
        GatherOffsets::Single(offset) => make_gather(Some(offset)),
        GatherOffsets::Ptp(offsets) => {
            let samples = offsets.map(|[x, y]| make_gather(Some(format!("int2({x}, {y})"))));
            let result_type = if texture.is_integer {
                "uint4"
            } else {
                "float4"
            };
            format!(
                "{result_type}(({}).w, ({}).w, ({}).w, ({}).w)",
                samples[0], samples[1], samples[2], samples[3]
            )
        }
    };
    let expression = if texture.is_integer {
        format!("as_type<float4>({gathered})")
    } else {
        gathered
    };
    context.define(inst_ref, Type::F32x4, expression, false)
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

/// Native-MSL counterpart of upstream `EmitImageRead`.
pub fn emit_image_read(
    context: &mut MslEmitContext,
    _program: &Program,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let info = TextureInstInfo::from_u32(inst.flags);
    if crate::shader_info::ImageFormat::from_u8(info.image_format)
        == crate::shader_info::ImageFormat::Typeless
        && !context.supports_typeless_image_loads()
    {
        log::warn!("MSL: typeless image read not supported by host");
        return context.define(inst_ref, Type::U32x4, "uint4(0u)".to_owned(), false);
    }
    if inst
        .get_associated_pseudo(Opcode::GetSparseFromOp)
        .is_some()
    {
        return Err(MslError::UnsupportedProgramFeature(
            "sparse storage image read",
        ));
    }
    let image = context.image_expressions(info, inst.arg(0), inst_ref)?;
    let coords = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let mut arguments = Vec::new();
    append_storage_coordinates(&mut arguments, image.texture_type, coords)?;
    let read = format!("{}.read({})", image.image, arguments.join(", "));
    let expression = if image.is_integer {
        read
    } else {
        format!("as_type<uint4>({read})")
    };
    context.define(inst_ref, Type::U32x4, expression, false)
}

/// Native-MSL counterpart of upstream `EmitImageWrite`.
pub fn emit_image_write(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let info = TextureInstInfo::from_u32(inst.flags);
    let image = context.image_expressions(info, inst.arg(0), inst_ref)?;
    let coords = context.value_expression(inst.arg(1), inst_ref, 1)?;
    let color = context.value_expression(inst.arg(2), inst_ref, 2)?;
    let color = if image.is_integer {
        color
    } else {
        format!("as_type<float4>({color})")
    };
    let mut arguments = vec![color];
    append_storage_coordinates(&mut arguments, image.texture_type, coords)?;
    context.push_statement(format!("{}.write({});", image.image, arguments.join(", ")));
    Ok(())
}

fn emit_is_scaled(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
    masks: &'static str,
    base_index: u32,
) -> Result<(), MslError> {
    let expression = match inst.arg(0) {
        Value::ImmU32(index) => {
            let index = index.wrapping_add(base_index);
            let word_index = index / 32;
            let bit_mask = 1u32 << (index % 32);
            format!("(({masks}[{word_index}u] & 0x{bit_mask:08X}u) != 0u)")
        }
        index => {
            let index = context.value_expression(index, inst_ref, 0)?;
            let index = if base_index == 0 {
                index
            } else {
                format!("({index} + {base_index}u)")
            };
            format!("(({masks}[({index} >> 5u)] & (1u << ({index} & 31u))) != 0u)")
        }
    };
    context.define(inst_ref, Type::U1, expression, false)
}

/// Native-MSL counterpart of upstream `EmitIsTextureScaled`.
pub fn emit_is_texture_scaled(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let base_index = context.texture_rescaling_index();
    emit_is_scaled(
        context,
        inst_ref,
        inst,
        "rescaling_push_constants.rescaling_textures",
        base_index,
    )
}

/// Native-MSL counterpart of upstream `EmitIsImageScaled`.
pub fn emit_is_image_scaled(
    context: &mut MslEmitContext,
    inst_ref: InstRef,
    inst: &Inst,
) -> Result<(), MslError> {
    let base_index = context.image_rescaling_index();
    emit_is_scaled(
        context,
        inst_ref,
        inst,
        "rescaling_push_constants.rescaling_images",
        base_index,
    )
}
