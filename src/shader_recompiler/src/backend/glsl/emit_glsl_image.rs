// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/backend/glsl/emit_glsl_image.cpp
//!
//! GLSL emission for sampled-texture (`Image*`) and storage-image
//! (`Image{Read,Write,Atomic*}`) IR opcodes. Each public `emit_*` function
//! corresponds to an upstream `Emit*` free function in
//! `backend/glsl/emit_glsl_image.cpp`. Helper functions (`Texture`,
//! `Image`, `CastToIntVec`, `CoordsCastToInt`, `GetOffsetVec`,
//! `PtpOffsets`, `PrepareSparse`, `IsTextureMsaa`,
//! `ImageGatherSubpixelOffset`, `NeedsShadowLodExt`) preserve upstream's
//! anonymous-namespace ownership in the same Rust file.
//!
//! Upstream signature shape:
//! ```cpp
//! void EmitImageSampleImplicitLod(EmitContext& ctx, IR::Inst& inst,
//!         const IR::Value& index, std::string_view coords,
//!         std::string_view bias_lc, const IR::Value& offset);
//! ```
//! The Rust port collapses the pre-consumed `coords`/`bias_lc`/etc.
//! string arguments back into raw `&inst.args[N]` indexing: each emit
//! function takes `(ctx, program, inst_ref, inst)` and consumes the
//! per-position args internally via `ctx.var_alloc.consume`. This
//! matches the existing dispatcher pattern in `emit_glsl.rs` and keeps
//! method ownership in the same file.
//!
//! Image-arg layout (mirrors upstream `Inst(op, Flags{info}, handle,
//! coords, ...)` calls in `frontend/ir/ir_emitter.cpp`):
//! - `args[0]` = handle / index (raw `IR::Value`; immediate u32 for
//!   bound, register-sourced for bindless)
//! - `args[1]` = coords (consumed)
//! - `args[2]` = bias_lc / lod_lc / dref / value (consumed when present)
//! - `args[3]` = offset (raw `IR::Value`; Empty for no offset)
//! - `args[4..]` = lod / ms / color / value (consumed when present)
//!
//! Bound/Bindless IR opcodes (`BoundImage*` / `BindlessImage*`) fold
//! into the non-prefixed variants via an earlier IR pass; the prefixed
//! emit handlers panic with the same `NotImplemented` message upstream
//! uses (their direct dispatch would indicate a pass-ordering bug).

use crate::ir;
use crate::ir::instruction::Inst;
use crate::ir::opcodes::Opcode;
use crate::ir::types::TextureInstInfo;
use crate::ir::value::{InstRef, Value};
use crate::shader_info::TextureType;

use super::glsl_emit_context::EmitContext;
use super::var_alloc::GlslVarType;

// ── Helpers ────────────────────────────────────────────────────────────

fn inst_mut<'a>(program: &'a mut ir::Program, inst_ref: InstRef) -> &'a mut Inst {
    program.block_mut(inst_ref.block).inst_mut(inst_ref.inst)
}

/// Port of upstream `Texture(EmitContext&, TextureInstInfo, IR::Value)`.
fn texture(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    info: TextureInstInfo,
    index: &Value,
) -> String {
    let ty = TextureType::from_u8(info.texture_type);
    let def_opt = if ty == TextureType::Buffer {
        ctx.texture_buffers
            .get(info.descriptor_index as usize)
            .copied()
    } else {
        ctx.textures.get(info.descriptor_index as usize).copied()
    };
    let (binding, count) = match def_opt {
        Some(def) => (def.binding, def.count),
        None => {
            // Keep emission deterministic for partially-ported shaders; a
            // missing descriptor falls back to descriptor_index as the
            // binding so the resulting GLSL still names something visible
            // (will fail at link time rather than crash here).
            (info.descriptor_index as u32, 1)
        }
    };
    if count > 1 {
        let idx_str = ctx.var_alloc.consume(program, index);
        format!("tex{}[{}]", binding, idx_str)
    } else {
        format!("tex{}", binding)
    }
}

/// Port of upstream `Image(EmitContext&, TextureInstInfo, IR::Value)`.
fn image(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    info: TextureInstInfo,
    index: &Value,
) -> String {
    let ty = TextureType::from_u8(info.texture_type);
    let def_opt = if ty == TextureType::Buffer {
        ctx.image_buffers
            .get(info.descriptor_index as usize)
            .copied()
    } else {
        ctx.images.get(info.descriptor_index as usize).copied()
    };
    let (binding, count) = match def_opt {
        Some(def) => (def.binding, def.count),
        None => (info.descriptor_index as u32, 1),
    };
    if count > 1 {
        let idx_str = ctx.var_alloc.consume(program, index);
        format!("img{}[{}]", binding, idx_str)
    } else {
        format!("img{}", binding)
    }
}

/// Port of upstream `IsTextureMsaa(EmitContext&, TextureInstInfo)`.
fn is_texture_msaa(ctx: &EmitContext, info: TextureInstInfo) -> bool {
    let ty = TextureType::from_u8(info.texture_type);
    if ty == TextureType::Buffer {
        return false;
    }
    ctx.textures
        .get(info.descriptor_index as usize)
        .map(|def| def.is_multisample)
        .unwrap_or(false)
}

/// Port of upstream `CastToIntVec(std::string_view, TextureInstInfo)`.
fn cast_to_int_vec(value: &str, info: TextureInstInfo) -> String {
    match TextureType::from_u8(info.texture_type) {
        TextureType::Color1D | TextureType::Buffer => format!("int({})", value),
        TextureType::ColorArray1D | TextureType::Color2D | TextureType::ColorArray2D => {
            format!("ivec2({})", value)
        }
        TextureType::Color3D | TextureType::ColorCube => format!("ivec3({})", value),
        TextureType::ColorArrayCube => format!("ivec4({})", value),
        ty => panic!("Integer cast for TextureType {:?}", ty),
    }
}

/// Port of upstream `CoordsCastToInt(std::string_view, TextureInstInfo)`.
///
/// Differs from `cast_to_int_vec` for `ColorArray1D` (ivec2) vs `Color2D`
/// (ivec2) vs `ColorArray2D` (ivec3).
fn coords_cast_to_int(value: &str, info: TextureInstInfo) -> String {
    match TextureType::from_u8(info.texture_type) {
        TextureType::Color1D | TextureType::Buffer => format!("int({})", value),
        TextureType::ColorArray1D | TextureType::Color2D => format!("ivec2({})", value),
        TextureType::ColorArray2D | TextureType::Color3D | TextureType::ColorCube => {
            format!("ivec3({})", value)
        }
        TextureType::ColorArrayCube => format!("ivec4({})", value),
        ty => panic!("TexelFetchCast type {:?}", ty),
    }
}

/// Port of upstream `NeedsShadowLodExt(TextureType)`.
fn needs_shadow_lod_ext(ty: TextureType) -> bool {
    matches!(
        ty,
        TextureType::ColorArray2D | TextureType::ColorCube | TextureType::ColorArrayCube
    )
}

fn inst_recursive_value(program: &ir::Program, mut value: Value) -> Value {
    loop {
        let Value::Inst(inst_ref) = value else {
            return value;
        };
        let inst = program.block(inst_ref.block).inst(inst_ref.inst);
        if inst.opcode != Opcode::Identity || inst.args.is_empty() {
            return value;
        }
        value = inst.args[0];
    }
}

fn eval_const_offset_component(program: &ir::Program, value: &Value) -> Option<i32> {
    match inst_recursive_value(program, *value) {
        Value::ImmU32(value) => Some(value as i32),
        Value::Inst(inst_ref) => {
            let inst = program.block(inst_ref.block).inst(inst_ref.inst);
            match inst.opcode {
                Opcode::BitFieldSExtract | Opcode::BitFieldUExtract if inst.args.len() == 3 => {
                    let base = eval_const_offset_component(program, &inst.args[0])? as u32;
                    let offset = eval_const_offset_component(program, &inst.args[1])? as u32;
                    let count = eval_const_offset_component(program, &inst.args[2])? as u32;
                    if count == 0 || count > 32 || offset >= 32 {
                        return None;
                    }
                    let shifted = base >> offset;
                    let mask = if count == 32 {
                        u32::MAX
                    } else {
                        (1u32 << count) - 1
                    };
                    let extracted = shifted & mask;
                    if inst.opcode == Opcode::BitFieldSExtract && count < 32 {
                        let sign = 1u32 << (count - 1);
                        if extracted & sign != 0 {
                            return Some((extracted | !mask) as i32);
                        }
                    }
                    Some(extracted as i32)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn eval_const_offset_vec(program: &ir::Program, offset: &Value) -> Option<String> {
    match inst_recursive_value(program, *offset) {
        Value::ImmU32(value) => Some(format!("int({})", value)),
        Value::Inst(inst_ref) => {
            let inst = program.block(inst_ref.block).inst(inst_ref.inst);
            let args = inst
                .args
                .iter()
                .map(|arg| eval_const_offset_component(program, arg))
                .collect::<Option<Vec<_>>>()?;
            match inst.opcode {
                Opcode::CompositeConstructU32x2 if args.len() == 2 => {
                    Some(format!("ivec2({},{})", args[0], args[1]))
                }
                Opcode::CompositeConstructU32x3 if args.len() == 3 => {
                    Some(format!("ivec3({},{},{})", args[0], args[1], args[2]))
                }
                Opcode::CompositeConstructU32x4 if args.len() == 4 => Some(format!(
                    "ivec4({},{},{},{})",
                    args[0], args[1], args[2], args[3]
                )),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Port of upstream `GetOffsetVec(EmitContext&, IR::Value)`.
///
/// `offset` is a raw IR value, often an immediate U32 or a
/// `CompositeConstructU32x{2,3,4}` of immediates. Otherwise we consume
/// it via `var_alloc` and cast to the matching integer vector width.
fn get_offset_vec(ctx: &mut EmitContext, program: &mut ir::Program, offset: &Value) -> String {
    if let Some(offset) = eval_const_offset_vec(program, offset) {
        return offset;
    }
    let resolved_offset = inst_recursive_value(program, *offset);
    if let Value::Inst(inst_ref) = resolved_offset {
        let opcode = program.block(inst_ref.block).inst(inst_ref.inst).opcode;
        let all_imm = program
            .block(inst_ref.block)
            .inst(inst_ref.inst)
            .args
            .iter()
            .all(|a| a.is_immediate());
        if all_imm {
            let args: Vec<u32> = program
                .block(inst_ref.block)
                .inst(inst_ref.inst)
                .args
                .iter()
                .filter_map(|a| match a {
                    Value::ImmU32(u) => Some(*u),
                    _ => None,
                })
                .collect();
            match opcode {
                Opcode::CompositeConstructU32x2 if args.len() == 2 => {
                    return format!("ivec2({},{})", args[0], args[1]);
                }
                Opcode::CompositeConstructU32x3 if args.len() == 3 => {
                    return format!("ivec3({},{},{})", args[0], args[1], args[2]);
                }
                Opcode::CompositeConstructU32x4 if args.len() == 4 => {
                    return format!("ivec4({},{},{},{})", args[0], args[1], args[2], args[3]);
                }
                _ => {}
            }
        }
    }
    let has_var_aoffi = ctx.profile.support_gl_variable_aoffi;
    if !has_var_aoffi {
        log::warn!("Device does not support variable texture offsets, STUBBING");
    }
    let offset_str = if has_var_aoffi {
        ctx.var_alloc.consume(program, offset)
    } else {
        "0".to_string()
    };
    let ty = match resolved_offset {
        Value::Inst(inst_ref) => {
            program
                .block(inst_ref.block)
                .inst(inst_ref.inst)
                .opcode
                .meta()
                .return_type
        }
        _ => offset.ir_type(),
    };
    match ty {
        ir::types::Type::U32 => format!("int({})", offset_str),
        ir::types::Type::U32x2 => format!("ivec2({})", offset_str),
        ir::types::Type::U32x3 => format!("ivec3({})", offset_str),
        ir::types::Type::U32x4 => format!("ivec4({})", offset_str),
        other => panic!("Offset type {:?}", other),
    }
}

/// Port of upstream `PtpOffsets(IR::Value, IR::Value)`.
///
/// PTP (per-texel program) gather offsets — two `CompositeConstructU32x4`
/// values, each containing 4 immediates. Falls back to a "0..3" identity
/// array (matching upstream stub) when args aren't all immediate.
fn ptp_offsets(program: &mut ir::Program, offset: &Value, offset2: &Value) -> String {
    let (Value::Inst(r0), Value::Inst(r1)) = (offset, offset2) else {
        log::warn!("Not all arguments in PTP are immediate, STUBBING");
        return "ivec2[](ivec2(0), ivec2(1), ivec2(2), ivec2(3))".to_string();
    };
    let inst0 = program.block(r0.block).inst(r0.inst).clone();
    let inst1 = program.block(r1.block).inst(r1.inst).clone();
    let all_imm0 = inst0.args.iter().all(|a| a.is_immediate());
    let all_imm1 = inst1.args.iter().all(|a| a.is_immediate());
    if !all_imm0 || !all_imm1 {
        log::warn!("Not all arguments in PTP are immediate, STUBBING");
        return "ivec2[](ivec2(0), ivec2(1), ivec2(2), ivec2(3))".to_string();
    }
    if inst0.opcode != inst1.opcode || inst0.opcode != Opcode::CompositeConstructU32x4 {
        panic!("Invalid PTP arguments");
    }
    let read = |inst: &Inst, idx: usize| -> u32 {
        match &inst.args[idx] {
            Value::ImmU32(u) => *u,
            _ => unreachable!("checked all_imm above"),
        }
    };
    format!(
        "ivec2[](ivec2({},{}),ivec2({},{}),ivec2({},{}),ivec2({},{}))",
        read(&inst0, 0),
        read(&inst0, 1),
        read(&inst0, 2),
        read(&inst0, 3),
        read(&inst1, 0),
        read(&inst1, 1),
        read(&inst1, 2),
        read(&inst1, 3),
    )
}

/// Port of upstream `PrepareSparse(IR::Inst&)`.
///
/// Returns the associated `GetSparseFromOp` pseudo-instruction reference if
/// present, and invalidates it (sparse residency is handled inline by the
/// caller through the returned `InstRef` rather than as a separate
/// `Get*FromOp` opcode emission).
fn prepare_sparse(program: &mut ir::Program, inst_ref: InstRef) -> Option<InstRef> {
    let sparse = program
        .block(inst_ref.block)
        .inst(inst_ref.inst)
        .get_associated_pseudo(Opcode::GetSparseFromOp);
    if let Some(s) = sparse {
        program.block_mut(s.block).inst_mut(s.inst).opcode = Opcode::Void;
    }
    sparse
}

/// Port of upstream `ImageGatherSubpixelOffset(TextureInstInfo, ..., ...)`.
///
/// Applies a 1/512 texel subpixel offset for AMD parity with Nvidia/Maxwell
/// gather rounding behaviour. No-op for texture types that don't gather.
fn image_gather_subpixel_offset(info: TextureInstInfo, texture_name: &str, coords: &str) -> String {
    match TextureType::from_u8(info.texture_type) {
        TextureType::Color2D | TextureType::Color2DRect => format!(
            "{}+vec2(0.001953125)/vec2(textureSize({}, 0))",
            coords, texture_name
        ),
        TextureType::ColorArray2D | TextureType::ColorCube => format!(
            "vec3({}.xy+vec2(0.001953125)/vec2(textureSize({}, 0)),{}.z)",
            coords, texture_name, coords
        ),
        _ => coords.to_string(),
    }
}

/// Define a GLSL var for `inst` and return its name + a representation for
/// any associated sparse pseudo-inst (if it exists).
fn define_with_sparse(
    program: &mut ir::Program,
    var_alloc: &mut super::var_alloc::VarAlloc,
    inst_ref: InstRef,
    var_type: GlslVarType,
) -> (String, Option<String>) {
    let sparse = prepare_sparse(program, inst_ref);
    let sparse_name = sparse.map(|s| var_alloc.define(inst_mut(program, s), GlslVarType::U1));
    let dst = var_alloc.define(inst_mut(program, inst_ref), var_type);
    (dst, sparse_name)
}

// ── Sample variants ────────────────────────────────────────────────────

/// Port of upstream `EmitImageSampleImplicitLod`.
pub fn emit_image_sample_implicit_lod_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    if info.has_lod_clamp {
        panic!("EmitImageSampleImplicitLod Lod clamp samples");
    }
    let texture_name = texture(ctx, program, info, &inst.args[0]);
    let coords = ctx.var_alloc.consume(program, &inst.args[1]);
    let bias_lc = if info.has_bias {
        format!(",{}", ctx.var_alloc.consume(program, &inst.args[2]))
    } else {
        String::new()
    };
    let offset = inst.args.get(3).cloned().unwrap_or(Value::Void);
    let var_alloc = &mut ctx.var_alloc as *mut _;
    let (texel, sparse_name) =
        unsafe { define_with_sparse(program, &mut *var_alloc, inst_ref, GlslVarType::F32x4) };
    let supports_sparse = ctx.profile.support_gl_sparse_textures;
    if let Some(ref sn) = sparse_name {
        if !supports_sparse {
            log::warn!("Device does not support sparse texture queries. STUBBING");
            ctx.add_fmt(format!("{}=true;", sn));
        }
    }
    if sparse_name.is_none() || !supports_sparse {
        if !offset.is_void() {
            let off = get_offset_vec(ctx, program, &offset);
            if ctx.stage == crate::stage::Stage::Fragment {
                ctx.add_fmt(format!(
                    "{}=textureOffset({},{},{}{});",
                    texel, texture_name, coords, off, bias_lc
                ));
            } else {
                ctx.add_fmt(format!(
                    "{}=textureLodOffset({},{},0.0,{});",
                    texel, texture_name, coords, off
                ));
            }
        } else if ctx.stage == crate::stage::Stage::Fragment {
            ctx.add_fmt(format!(
                "{}=texture({},{}{});",
                texel, texture_name, coords, bias_lc
            ));
        } else {
            ctx.add_fmt(format!(
                "{}=textureLod({},{},0.0);",
                texel, texture_name, coords
            ));
        }
        return;
    }
    let sn = sparse_name.unwrap();
    if !offset.is_void() {
        let off = get_offset_vec(ctx, program, &offset);
        ctx.add_fmt(format!(
            "{}=sparseTexelsResidentARB(sparseTextureOffsetARB({},{},{},{}{}));",
            sn, texture_name, coords, off, texel, bias_lc
        ));
    } else {
        ctx.add_fmt(format!(
            "{}=sparseTexelsResidentARB(sparseTextureARB({},{},{}{}));",
            sn, texture_name, coords, texel, bias_lc
        ));
    }
}

/// Port of upstream `EmitImageSampleExplicitLod`.
pub fn emit_image_sample_explicit_lod_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    if info.has_bias {
        panic!("EmitImageSampleExplicitLod Bias texture samples");
    }
    if info.has_lod_clamp {
        panic!("EmitImageSampleExplicitLod Lod clamp samples");
    }
    let texture_name = texture(ctx, program, info, &inst.args[0]);
    let coords = ctx.var_alloc.consume(program, &inst.args[1]);
    let lod_lc = ctx.var_alloc.consume(program, &inst.args[2]);
    let offset = inst.args.get(3).cloned().unwrap_or(Value::Void);
    let var_alloc = &mut ctx.var_alloc as *mut _;
    let (texel, sparse_name) =
        unsafe { define_with_sparse(program, &mut *var_alloc, inst_ref, GlslVarType::F32x4) };
    let supports_sparse = ctx.profile.support_gl_sparse_textures;
    if let Some(ref sn) = sparse_name {
        if !supports_sparse {
            log::warn!("Device does not support sparse texture queries. STUBBING");
            ctx.add_fmt(format!("{}=true;", sn));
        }
    }
    if sparse_name.is_none() || !supports_sparse {
        if !offset.is_void() {
            let off = get_offset_vec(ctx, program, &offset);
            ctx.add_fmt(format!(
                "{}=textureLodOffset({},{},{},{});",
                texel, texture_name, coords, lod_lc, off
            ));
        } else {
            ctx.add_fmt(format!(
                "{}=textureLod({},{},{});",
                texel, texture_name, coords, lod_lc
            ));
        }
        return;
    }
    let sn = sparse_name.unwrap();
    if !offset.is_void() {
        let off = get_offset_vec(ctx, program, &offset);
        ctx.add_fmt(format!(
            "{}=sparseTexelsResidentARB(sparseTexelFetchOffsetARB({},{},int({}),{},{}));",
            sn,
            texture_name,
            cast_to_int_vec(&coords, info),
            lod_lc,
            off,
            texel
        ));
    } else {
        ctx.add_fmt(format!(
            "{}=sparseTexelsResidentARB(sparseTextureLodARB({},{},{},{}));",
            sn, texture_name, coords, lod_lc, texel
        ));
    }
}

/// Port of upstream `EmitImageSampleDrefImplicitLod`.
pub fn emit_image_sample_dref_implicit_lod_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    let sparse = program
        .block(inst_ref.block)
        .inst(inst_ref.inst)
        .get_associated_pseudo(Opcode::GetSparseFromOp);
    if sparse.is_some() {
        panic!("EmitImageSampleDrefImplicitLod Sparse texture samples");
    }
    if info.has_bias {
        panic!("EmitImageSampleDrefImplicitLod Bias texture samples");
    }
    if info.has_lod_clamp {
        panic!("EmitImageSampleDrefImplicitLod Lod clamp samples");
    }
    let texture_name = texture(ctx, program, info, &inst.args[0]);
    let coords = ctx.var_alloc.consume(program, &inst.args[1]);
    let dref = ctx.var_alloc.consume(program, &inst.args[2]);
    let bias_lc = if info.has_bias {
        format!(",{}", ctx.var_alloc.consume(program, &inst.args[3]))
    } else {
        String::new()
    };
    let offset = inst.args.get(4).cloned().unwrap_or(Value::Void);
    let dst = ctx
        .var_alloc
        .define(inst_mut(program, inst_ref), GlslVarType::F32);
    let ty = TextureType::from_u8(info.texture_type);
    let needs_shadow_ext = needs_shadow_lod_ext(ty);
    let cast = if needs_shadow_ext { "vec4" } else { "vec3" };
    let use_grad = !ctx.profile.support_gl_texture_shadow_lod
        && ctx.stage != crate::stage::Stage::Fragment
        && needs_shadow_ext;
    if use_grad {
        log::warn!("Device lacks GL_EXT_texture_shadow_lod. Using textureGrad fallback");
        if ty == TextureType::ColorArrayCube {
            log::warn!("textureGrad does not support ColorArrayCube. Stubbing");
            ctx.add_fmt(format!("{}=0.0f;", dst));
            return;
        }
        let d_cast = if ty == TextureType::ColorArray2D {
            "vec2"
        } else {
            "vec3"
        };
        ctx.add_fmt(format!(
            "{}=textureGrad({},{}({},{}),{}(0),{}(0));",
            dst, texture_name, cast, coords, dref, d_cast, d_cast
        ));
        return;
    }
    if !offset.is_void() {
        let off = get_offset_vec(ctx, program, &offset);
        if ctx.stage == crate::stage::Stage::Fragment {
            ctx.add_fmt(format!(
                "{}=textureOffset({},{}({},{}),{}{});",
                dst, texture_name, cast, coords, dref, off, bias_lc
            ));
        } else {
            ctx.add_fmt(format!(
                "{}=textureLodOffset({},{}({},{}),0.0,{});",
                dst, texture_name, cast, coords, dref, off
            ));
        }
    } else if ctx.stage == crate::stage::Stage::Fragment {
        if ty == TextureType::ColorArrayCube {
            ctx.add_fmt(format!(
                "{}=texture({},vec4({}),{});",
                dst, texture_name, coords, dref
            ));
        } else {
            ctx.add_fmt(format!(
                "{}=texture({},{}({},{}){});",
                dst, texture_name, cast, coords, dref, bias_lc
            ));
        }
    } else {
        ctx.add_fmt(format!(
            "{}=textureLod({},{}({},{}),0.0);",
            dst, texture_name, cast, coords, dref
        ));
    }
}

/// Port of upstream `EmitImageSampleDrefExplicitLod`.
pub fn emit_image_sample_dref_explicit_lod_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    let sparse = program
        .block(inst_ref.block)
        .inst(inst_ref.inst)
        .get_associated_pseudo(Opcode::GetSparseFromOp);
    if sparse.is_some() {
        panic!("EmitImageSampleDrefExplicitLod Sparse texture samples");
    }
    if info.has_bias {
        panic!("EmitImageSampleDrefExplicitLod Bias texture samples");
    }
    if info.has_lod_clamp {
        panic!("EmitImageSampleDrefExplicitLod Lod clamp samples");
    }
    let texture_name = texture(ctx, program, info, &inst.args[0]);
    let coords = ctx.var_alloc.consume(program, &inst.args[1]);
    let dref = ctx.var_alloc.consume(program, &inst.args[2]);
    let lod_lc = ctx.var_alloc.consume(program, &inst.args[3]);
    let offset = inst.args.get(4).cloned().unwrap_or(Value::Void);
    let dst = ctx
        .var_alloc
        .define(inst_mut(program, inst_ref), GlslVarType::F32);
    let ty = TextureType::from_u8(info.texture_type);
    let needs_shadow_ext = needs_shadow_lod_ext(ty);
    let use_grad = !ctx.profile.support_gl_texture_shadow_lod && needs_shadow_ext;
    let cast = if needs_shadow_ext { "vec4" } else { "vec3" };
    if use_grad {
        log::warn!("Device lacks GL_EXT_texture_shadow_lod. Using textureGrad fallback");
        if ty == TextureType::ColorArrayCube {
            log::warn!("textureGrad does not support ColorArrayCube. Stubbing");
            ctx.add_fmt(format!("{}=0.0f;", dst));
            return;
        }
        let d_cast = if ty == TextureType::ColorArray2D {
            "vec2"
        } else {
            "vec3"
        };
        ctx.add_fmt(format!(
            "{}=textureGrad({},{}({},{}),{}(0),{}(0));",
            dst, texture_name, cast, coords, dref, d_cast, d_cast
        ));
        return;
    }
    if !offset.is_void() {
        let off = get_offset_vec(ctx, program, &offset);
        if ty == TextureType::ColorArrayCube {
            ctx.add_fmt(format!(
                "{}=textureLodOffset({},{},{},{},{});",
                dst, texture_name, coords, dref, lod_lc, off
            ));
        } else {
            ctx.add_fmt(format!(
                "{}=textureLodOffset({},{}({},{}),{},{});",
                dst, texture_name, cast, coords, dref, lod_lc, off
            ));
        }
    } else if ty == TextureType::ColorArrayCube {
        ctx.add_fmt(format!(
            "{}=textureLod({},{},{},{});",
            dst, texture_name, coords, dref, lod_lc
        ));
    } else {
        ctx.add_fmt(format!(
            "{}=textureLod({},{}({},{}),{});",
            dst, texture_name, cast, coords, dref, lod_lc
        ));
    }
}

// ── Gather ─────────────────────────────────────────────────────────────

/// Port of upstream `EmitImageGather`.
pub fn emit_image_gather_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    let texture_name = texture(ctx, program, info, &inst.args[0]);
    let coords = ctx.var_alloc.consume(program, &inst.args[1]);
    let offset = inst.args.get(2).cloned().unwrap_or(Value::Void);
    let offset2 = inst.args.get(3).cloned().unwrap_or(Value::Void);
    let var_alloc = &mut ctx.var_alloc as *mut _;
    let (texel, sparse_name) =
        unsafe { define_with_sparse(program, &mut *var_alloc, inst_ref, GlslVarType::F32x4) };
    let supports_sparse = ctx.profile.support_gl_sparse_textures;
    if let Some(ref sn) = sparse_name {
        if !supports_sparse {
            log::warn!("Device does not support sparse texture queries. STUBBING");
            ctx.add_fmt(format!("{}=true;", sn));
        }
    }
    let coords_final = if ctx.profile.need_gather_subpixel_offset {
        image_gather_subpixel_offset(info, &texture_name, &coords)
    } else {
        coords.clone()
    };
    if sparse_name.is_none() || !supports_sparse {
        if offset.is_void() {
            ctx.add_fmt(format!(
                "{}=textureGather({},{},int({}));",
                texel, texture_name, coords_final, info.gather_component
            ));
            return;
        }
        if offset2.is_void() {
            let off = get_offset_vec(ctx, program, &offset);
            ctx.add_fmt(format!(
                "{}=textureGatherOffset({},{},{},int({}));",
                texel, texture_name, coords_final, off, info.gather_component
            ));
            return;
        }
        let offsets = ptp_offsets(program, &offset, &offset2);
        ctx.add_fmt(format!(
            "{}=textureGatherOffsets({},{},{},int({}));",
            texel, texture_name, coords_final, offsets, info.gather_component
        ));
        return;
    }
    let sn = sparse_name.unwrap();
    if offset.is_void() {
        ctx.add_fmt(format!(
            "{}=sparseTexelsResidentARB(sparseTextureGatherARB({},{},{},int({})));",
            sn, texture_name, coords_final, texel, info.gather_component
        ));
        return;
    }
    if offset2.is_void() {
        let off = get_offset_vec(ctx, program, &offset);
        ctx.add_fmt(format!(
            "{}=sparseTexelsResidentARB(sparseTextureGatherOffsetARB({},{},{},{},int({})));",
            sn,
            texture_name,
            cast_to_int_vec(&coords_final, info),
            off,
            texel,
            info.gather_component
        ));
        return;
    }
    let offsets = ptp_offsets(program, &offset, &offset2);
    ctx.add_fmt(format!(
        "{}=sparseTexelsResidentARB(sparseTextureGatherOffsetARB({},{},{},{},int({})));",
        sn,
        texture_name,
        cast_to_int_vec(&coords_final, info),
        offsets,
        texel,
        info.gather_component
    ));
}

/// Port of upstream `EmitImageGatherDref`.
pub fn emit_image_gather_dref_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    let texture_name = texture(ctx, program, info, &inst.args[0]);
    let coords = ctx.var_alloc.consume(program, &inst.args[1]);
    let offset = inst.args.get(2).cloned().unwrap_or(Value::Void);
    let offset2 = inst.args.get(3).cloned().unwrap_or(Value::Void);
    let dref = ctx.var_alloc.consume(program, &inst.args[4]);
    let var_alloc = &mut ctx.var_alloc as *mut _;
    let (texel, sparse_name) =
        unsafe { define_with_sparse(program, &mut *var_alloc, inst_ref, GlslVarType::F32x4) };
    let supports_sparse = ctx.profile.support_gl_sparse_textures;
    if let Some(ref sn) = sparse_name {
        if !supports_sparse {
            log::warn!("Device does not support sparse texture queries. STUBBING");
            ctx.add_fmt(format!("{}=true;", sn));
        }
    }
    let coords_final = if ctx.profile.need_gather_subpixel_offset {
        image_gather_subpixel_offset(info, &texture_name, &coords)
    } else {
        coords.clone()
    };
    if sparse_name.is_none() || !supports_sparse {
        if offset.is_void() {
            ctx.add_fmt(format!(
                "{}=textureGather({},{},{});",
                texel, texture_name, coords_final, dref
            ));
            return;
        }
        if offset2.is_void() {
            let off = get_offset_vec(ctx, program, &offset);
            ctx.add_fmt(format!(
                "{}=textureGatherOffset({},{},{},{});",
                texel, texture_name, coords_final, dref, off
            ));
            return;
        }
        let offsets = ptp_offsets(program, &offset, &offset2);
        ctx.add_fmt(format!(
            "{}=textureGatherOffsets({},{},{},{});",
            texel, texture_name, coords_final, dref, offsets
        ));
        return;
    }
    let sn = sparse_name.unwrap();
    if offset.is_void() {
        ctx.add_fmt(format!(
            "{}=sparseTexelsResidentARB(sparseTextureGatherARB({},{},{},{}));",
            sn, texture_name, coords_final, dref, texel
        ));
        return;
    }
    if offset2.is_void() {
        let off = get_offset_vec(ctx, program, &offset);
        ctx.add_fmt(format!(
            "{}=sparseTexelsResidentARB(sparseTextureGatherOffsetARB({},{},{},{},{}));",
            sn,
            texture_name,
            cast_to_int_vec(&coords_final, info),
            dref,
            off,
            texel
        ));
        return;
    }
    let offsets = ptp_offsets(program, &offset, &offset2);
    ctx.add_fmt(format!(
        "{}=sparseTexelsResidentARB(sparseTextureGatherOffsetARB({},{},{},{},{}));",
        sn,
        texture_name,
        cast_to_int_vec(&coords_final, info),
        dref,
        offsets,
        texel
    ));
}

// ── Fetch / Query / Gradient ───────────────────────────────────────────

/// Port of upstream `EmitImageFetch`.
pub fn emit_image_fetch_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    if info.has_bias {
        panic!("EmitImageFetch Bias texture samples");
    }
    if info.has_lod_clamp {
        panic!("EmitImageFetch Lod clamp samples");
    }
    let texture_name = texture(ctx, program, info, &inst.args[0]);
    let coords = ctx.var_alloc.consume(program, &inst.args[1]);
    let offset = inst.args.get(2).cloned().unwrap_or(Value::Void);
    let lod = ctx.var_alloc.consume(program, &inst.args[3]);
    let ms = inst
        .args
        .get(4)
        .map(|a| ctx.var_alloc.consume(program, a))
        .unwrap_or_default();
    let var_alloc = &mut ctx.var_alloc as *mut _;
    let (texel, sparse_name) =
        unsafe { define_with_sparse(program, &mut *var_alloc, inst_ref, GlslVarType::F32x4) };
    let supports_sparse = ctx.profile.support_gl_sparse_textures;
    if let Some(ref sn) = sparse_name {
        if !supports_sparse {
            log::warn!("Device does not support sparse texture queries. STUBBING");
            ctx.add_fmt(format!("{}=true;", sn));
        }
    }
    if sparse_name.is_none() || !supports_sparse {
        let int_coords = coords_cast_to_int(&coords, info);
        if !ms.is_empty() {
            ctx.add_fmt(format!(
                "{}=texelFetch({},{},int({}));",
                texel, texture_name, int_coords, ms
            ));
        } else if !offset.is_void() {
            let off = get_offset_vec(ctx, program, &offset);
            ctx.add_fmt(format!(
                "{}=texelFetchOffset({},{},int({}),{});",
                texel, texture_name, int_coords, lod, off
            ));
        } else if TextureType::from_u8(info.texture_type) == TextureType::Buffer {
            ctx.add_fmt(format!(
                "{}=texelFetch({},int({}));",
                texel, texture_name, coords
            ));
        } else {
            ctx.add_fmt(format!(
                "{}=texelFetch({},{},int({}));",
                texel, texture_name, int_coords, lod
            ));
        }
        return;
    }
    if !ms.is_empty() {
        panic!("EmitImageFetch Sparse MSAA samples");
    }
    let sn = sparse_name.unwrap();
    if !offset.is_void() {
        let off = get_offset_vec(ctx, program, &offset);
        ctx.add_fmt(format!(
            "{}=sparseTexelsResidentARB(sparseTexelFetchOffsetARB({},{},int({}),{},{}));",
            sn,
            texture_name,
            cast_to_int_vec(&coords, info),
            lod,
            off,
            texel
        ));
    } else {
        ctx.add_fmt(format!(
            "{}=sparseTexelsResidentARB(sparseTexelFetchARB({},{},int({}),{}));",
            sn,
            texture_name,
            cast_to_int_vec(&coords, info),
            lod,
            texel
        ));
    }
}

/// Port of upstream `EmitImageQueryDimensions`.
pub fn emit_image_query_dimensions_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    let texture_name = texture(ctx, program, info, &inst.args[0]);
    let lod = ctx.var_alloc.consume(program, &inst.args[1]);
    let skip_mips_val = inst.args.get(2).cloned().unwrap_or(Value::Void);
    let is_msaa = is_texture_msaa(ctx, info);
    let skip_mips = matches!(skip_mips_val, Value::ImmU1(true));
    let mips = if skip_mips {
        "0u".to_string()
    } else {
        format!("uint(textureQueryLevels({}))", texture_name)
    };
    if is_msaa && !skip_mips {
        panic!("EmitImageQueryDimensions MSAA QueryLevels");
    }
    let ty = TextureType::from_u8(info.texture_type);
    if ty == TextureType::Buffer && !skip_mips {
        panic!("EmitImageQueryDimensions TextureType::Buffer QueryLevels");
    }
    let uses_lod = !is_msaa && ty != TextureType::Buffer;
    let lod_str = if uses_lod {
        format!(",int({})", lod)
    } else {
        String::new()
    };
    let dst = ctx
        .var_alloc
        .define(inst_mut(program, inst_ref), GlslVarType::U32x4);
    match ty {
        TextureType::Color1D => ctx.add_fmt(format!(
            "{}=uvec4(uint(textureSize({}{})),0u,0u,{});",
            dst, texture_name, lod_str, mips
        )),
        TextureType::ColorArray1D
        | TextureType::Color2D
        | TextureType::ColorCube
        | TextureType::Color2DRect => ctx.add_fmt(format!(
            "{}=uvec4(uvec2(textureSize({}{})),0u,{});",
            dst, texture_name, lod_str, mips
        )),
        TextureType::ColorArray2D | TextureType::Color3D | TextureType::ColorArrayCube => ctx
            .add_fmt(format!(
                "{}=uvec4(uvec3(textureSize({}{})),{});",
                dst, texture_name, lod_str, mips
            )),
        TextureType::Buffer => ctx.add_fmt(format!(
            "{}=uvec4(uint(textureSize({})),0u,0u,{});",
            dst, texture_name, mips
        )),
    }
}

/// Port of upstream `EmitImageQueryLod`.
pub fn emit_image_query_lod_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    let texture_name = texture(ctx, program, info, &inst.args[0]);
    let coords = ctx.var_alloc.consume(program, &inst.args[1]);
    let dst = ctx
        .var_alloc
        .define(inst_mut(program, inst_ref), GlslVarType::F32x4);
    ctx.add_fmt(format!(
        "{}=vec4(textureQueryLod({},{}),0.0,0.0);",
        dst, texture_name, coords
    ));
}

/// Port of upstream `EmitImageGradient`.
pub fn emit_image_gradient_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    if info.has_lod_clamp {
        panic!("EmitImageGradient Lod clamp samples");
    }
    let sparse = program
        .block(inst_ref.block)
        .inst(inst_ref.inst)
        .get_associated_pseudo(Opcode::GetSparseFromOp);
    if sparse.is_some() {
        panic!("EmitImageGradient Sparse");
    }
    let offset = inst.args.get(3).cloned().unwrap_or(Value::Void);
    if !offset.is_void() && info.num_derivatives <= 2 {
        panic!("EmitImageGradient offset");
    }
    let texture_name = texture(ctx, program, info, &inst.args[0]);
    let coords = ctx.var_alloc.consume(program, &inst.args[1]);
    let derivatives_vec = ctx.var_alloc.consume(program, &inst.args[2]);
    let texel = ctx
        .var_alloc
        .define(inst_mut(program, inst_ref), GlslVarType::F32x4);
    let multi_component = info.num_derivatives > 1 || info.has_lod_clamp;
    if multi_component {
        if info.num_derivatives >= 3 {
            let offset_vec = ctx.var_alloc.consume(program, &offset);
            ctx.add_fmt(format!(
                "{}=textureGrad({},{},vec3({}.xz, {}.x),vec3({}.yw, {}.y));",
                texel,
                texture_name,
                coords,
                derivatives_vec,
                offset_vec,
                derivatives_vec,
                offset_vec
            ));
            return;
        }
        ctx.add_fmt(format!(
            "{}=textureGrad({},{},vec2({}.xz),vec2({}.yz));",
            texel, texture_name, coords, derivatives_vec, derivatives_vec
        ));
    } else {
        ctx.add_fmt(format!(
            "{}=textureGrad({},{},float({}.x),float({}.y));",
            texel, texture_name, coords, derivatives_vec, derivatives_vec
        ));
    }
}

// ── Storage image read/write ───────────────────────────────────────────

/// Port of upstream `EmitImageRead`.
pub fn emit_image_read_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    let sparse = program
        .block(inst_ref.block)
        .inst(inst_ref.inst)
        .get_associated_pseudo(Opcode::GetSparseFromOp);
    if sparse.is_some() {
        panic!("EmitImageRead Sparse");
    }
    let image_name = image(ctx, program, info, &inst.args[0]);
    let coords = ctx.var_alloc.consume(program, &inst.args[1]);
    let dst = ctx
        .var_alloc
        .define(inst_mut(program, inst_ref), GlslVarType::U32x4);
    ctx.add_fmt(format!(
        "{}=uvec4(imageLoad({},{}));",
        dst,
        image_name,
        coords_cast_to_int(&coords, info)
    ));
}

/// Port of upstream `EmitImageWrite`.
pub fn emit_image_write_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    _inst_ref: InstRef,
    inst: &Inst,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    let image_name = image(ctx, program, info, &inst.args[0]);
    let coords = ctx.var_alloc.consume(program, &inst.args[1]);
    let color = ctx.var_alloc.consume(program, &inst.args[2]);
    ctx.add_fmt(format!(
        "imageStore({},{},{});",
        image_name,
        coords_cast_to_int(&coords, info),
        color
    ));
}

// ── Storage image atomics ──────────────────────────────────────────────

fn emit_image_atomic_binop(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
    glsl_fn: &str,
    cast_value: Option<&str>,
) {
    let info = TextureInstInfo::from_u32(inst.flags);
    let image_name = image(ctx, program, info, &inst.args[0]);
    let coords = ctx.var_alloc.consume(program, &inst.args[1]);
    let value = ctx.var_alloc.consume(program, &inst.args[2]);
    let value_expr = match cast_value {
        Some(c) => format!("{}({})", c, value),
        None => value,
    };
    let dst = ctx
        .var_alloc
        .define(inst_mut(program, inst_ref), GlslVarType::U32);
    ctx.add_fmt(format!(
        "{}={}({},{},{});",
        dst,
        glsl_fn,
        image_name,
        coords_cast_to_int(&coords, info),
        value_expr
    ));
}

pub fn emit_image_atomic_iadd32_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_image_atomic_binop(ctx, program, inst_ref, inst, "imageAtomicAdd", None);
}

pub fn emit_image_atomic_smin32_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_image_atomic_binop(ctx, program, inst_ref, inst, "imageAtomicMin", Some("int"));
}

pub fn emit_image_atomic_umin32_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_image_atomic_binop(ctx, program, inst_ref, inst, "imageAtomicMin", Some("uint"));
}

pub fn emit_image_atomic_smax32_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_image_atomic_binop(ctx, program, inst_ref, inst, "imageAtomicMax", Some("int"));
}

pub fn emit_image_atomic_umax32_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_image_atomic_binop(ctx, program, inst_ref, inst, "imageAtomicMax", Some("uint"));
}

pub fn emit_image_atomic_inc32_inst(
    _ctx: &mut EmitContext,
    _program: &mut ir::Program,
    _inst_ref: InstRef,
    _inst: &Inst,
) {
    panic!("EmitImageAtomicInc32 not implemented");
}

pub fn emit_image_atomic_dec32_inst(
    _ctx: &mut EmitContext,
    _program: &mut ir::Program,
    _inst_ref: InstRef,
    _inst: &Inst,
) {
    panic!("EmitImageAtomicDec32 not implemented");
}

pub fn emit_image_atomic_and32_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_image_atomic_binop(ctx, program, inst_ref, inst, "imageAtomicAnd", None);
}

pub fn emit_image_atomic_or32_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_image_atomic_binop(ctx, program, inst_ref, inst, "imageAtomicOr", None);
}

pub fn emit_image_atomic_xor32_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_image_atomic_binop(ctx, program, inst_ref, inst, "imageAtomicXor", None);
}

pub fn emit_image_atomic_exchange32_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_image_atomic_binop(ctx, program, inst_ref, inst, "imageAtomicExchange", None);
}

// ── Texture / image rescaling queries ──────────────────────────────────

/// Port of upstream `EmitIsTextureScaled`.
pub fn emit_is_texture_scaled_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let index = &inst.args[0];
    if !index.is_immediate() {
        panic!("Non-constant texture rescaling");
    }
    let image_index = match index {
        Value::ImmU32(u) => *u,
        _ => panic!("Non-constant texture rescaling"),
    };
    let dst = ctx
        .var_alloc
        .define(inst_mut(program, inst_ref), GlslVarType::U1);
    ctx.add_fmt(format!(
        "{}=(ftou(scaling.x)&{})!=0;",
        dst,
        1u32 << image_index
    ));
}

/// Port of upstream `EmitIsImageScaled`.
pub fn emit_is_image_scaled_inst(
    ctx: &mut EmitContext,
    program: &mut ir::Program,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let index = &inst.args[0];
    if !index.is_immediate() {
        panic!("Non-constant texture rescaling");
    }
    let image_index = match index {
        Value::ImmU32(u) => *u,
        _ => panic!("Non-constant texture rescaling"),
    };
    let dst = ctx
        .var_alloc
        .define(inst_mut(program, inst_ref), GlslVarType::U1);
    ctx.add_fmt(format!(
        "{}=(ftou(scaling.y)&{})!=0;",
        dst,
        1u32 << image_index
    ));
}

// ── Bound/Bindless guard stubs ─────────────────────────────────────────
//
// Upstream's `EmitBoundImage*` and `EmitBindlessImage*` are all
// `NotImplemented()` because the dispatcher folds Bound/Bindless into
// the non-prefixed `EmitImage*` via an earlier IR pass. Direct dispatch
// here would indicate a pass-ordering bug. We panic with the same
// message to surface that bug immediately.

pub fn emit_bound_image_inst(_ctx: &mut EmitContext) {
    panic!("BoundImage* must be folded before GLSL emission");
}

pub fn emit_bindless_image_inst(_ctx: &mut EmitContext) {
    panic!("BindlessImage* must be folded before GLSL emission");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::basic_block::Block;
    use crate::ir::instruction::Inst;
    use crate::ir::types::{FmzMode, FpRounding, TextureInstInfo as Tii};

    #[test]
    fn fetch_accepts_absent_sample_or_lod_like_upstream() {
        for multisample in [false, true] {
            let profile = crate::profile::Profile::default();
            let runtime_info = crate::runtime_info::RuntimeInfo::default();
            let mut bindings = crate::backend::bindings::Bindings::default();
            let mut program = crate::ir::Program::new(crate::stage::Stage::Compute);
            program.info.texture_descriptors.push(crate::shader_info::TextureDescriptor {
                handle_program: u32::MAX,
                texture_type: TextureType::Color2D,
                is_depth: false,
                is_multisample: multisample,
                is_integer: false,
                has_secondary: false,
                cbuf_index: 0,
                cbuf_offset: 0,
                shift_left: 0,
                secondary_cbuf_index: 0,
                secondary_cbuf_offset: 0,
                secondary_shift_left: 0,
                count: 1,
                size_shift: 3,
            });
            program.blocks.push(Block::new());
            let mut inst = Inst::new(Opcode::ImageFetch, vec![
                Value::Void, Value::ImmU32(3), Value::Void,
                if multisample { Value::Void } else { Value::ImmU32(2) },
                if multisample { Value::ImmU32(1) } else { Value::Void },
            ]);
            inst.flags = Tii {
                texture_type: TextureType::Color2D as u8,
                ..Default::default()
            }.to_u32();
            let index = program.block_mut(0).append_inst(inst.clone());
            let mut ctx = EmitContext::new(&program, &mut bindings, &profile, &runtime_info);
            emit_image_fetch_inst(&mut ctx, &mut program, InstRef { block: 0, inst: index }, &inst);
            let argument = if multisample { "1u" } else { "2u" };
            assert!(ctx.code.contains(&format!("texelFetch(tex0,ivec2(3u),int({argument}));")), "{}", ctx.code);
        }
    }

    #[test]
    fn cast_to_int_vec_matches_upstream_per_texture_type() {
        let mut info = Tii::default();
        info.texture_type = TextureType::Color1D as u8;
        assert_eq!(cast_to_int_vec("v", info), "int(v)");
        info.texture_type = TextureType::ColorArray1D as u8;
        assert_eq!(cast_to_int_vec("v", info), "ivec2(v)");
        info.texture_type = TextureType::Color2D as u8;
        assert_eq!(cast_to_int_vec("v", info), "ivec2(v)");
        info.texture_type = TextureType::Color3D as u8;
        assert_eq!(cast_to_int_vec("v", info), "ivec3(v)");
        info.texture_type = TextureType::ColorArrayCube as u8;
        assert_eq!(cast_to_int_vec("v", info), "ivec4(v)");
    }

    #[test]
    fn coords_cast_to_int_distinguishes_array1d_vs_array2d() {
        let mut info = Tii::default();
        info.texture_type = TextureType::ColorArray1D as u8;
        // Differs from cast_to_int_vec: ColorArray1D → ivec2, ColorArray2D → ivec3
        assert_eq!(coords_cast_to_int("v", info), "ivec2(v)");
        info.texture_type = TextureType::ColorArray2D as u8;
        assert_eq!(coords_cast_to_int("v", info), "ivec3(v)");
    }

    #[test]
    fn needs_shadow_lod_ext_matches_upstream_set() {
        assert!(needs_shadow_lod_ext(TextureType::ColorArray2D));
        assert!(needs_shadow_lod_ext(TextureType::ColorCube));
        assert!(needs_shadow_lod_ext(TextureType::ColorArrayCube));
        assert!(!needs_shadow_lod_ext(TextureType::Color2D));
        assert!(!needs_shadow_lod_ext(TextureType::Color3D));
    }

    #[test]
    fn texture_type_from_u8_round_trips() {
        for i in 0u8..=8 {
            let t = TextureType::from_u8(i);
            assert_eq!(t as u8, i);
        }
    }

    #[test]
    fn image_gather_subpixel_offset_only_active_for_2d_family() {
        let mut info = Tii::default();
        info.texture_type = TextureType::Color1D as u8;
        assert_eq!(image_gather_subpixel_offset(info, "tex0", "co"), "co");
        info.texture_type = TextureType::Color2D as u8;
        assert!(image_gather_subpixel_offset(info, "tex0", "co").contains("0.001953125"));
        info.texture_type = TextureType::ColorArray2D as u8;
        let out = image_gather_subpixel_offset(info, "tex0", "co");
        assert!(out.contains(".xy") && out.contains("0.001953125"));
    }

    #[test]
    fn offset_vec_folds_signed_bitfield_extracts_from_immediates() {
        let mut program = crate::ir::Program::new(crate::stage::Stage::Fragment);
        program.blocks.push(Block::new());

        let x = program.blocks[0].append_inst(Inst::new(
            Opcode::BitFieldSExtract,
            vec![
                Value::ImmU32(0x0000_00f1),
                Value::ImmU32(0),
                Value::ImmU32(4),
            ],
        ));
        let y = program.blocks[0].append_inst(Inst::new(
            Opcode::BitFieldSExtract,
            vec![
                Value::ImmU32(0x0000_00f1),
                Value::ImmU32(4),
                Value::ImmU32(4),
            ],
        ));
        let offset = program.blocks[0].append_inst(Inst::new(
            Opcode::CompositeConstructU32x2,
            vec![
                Value::Inst(InstRef { block: 0, inst: x }),
                Value::Inst(InstRef { block: 0, inst: y }),
            ],
        ));

        assert_eq!(
            eval_const_offset_vec(
                &program,
                &Value::Inst(InstRef {
                    block: 0,
                    inst: offset
                })
            ),
            Some("ivec2(1,-1)".to_string())
        );
    }

    #[test]
    fn offset_vec_chases_identity_like_inst_recursive() {
        let mut program = crate::ir::Program::new(crate::stage::Stage::Fragment);
        program.blocks.push(Block::new());

        let construct = program.blocks[0].append_inst(Inst::new(
            Opcode::CompositeConstructU32x2,
            vec![Value::ImmU32(3), Value::ImmU32(7)],
        ));
        let identity = program.blocks[0].append_inst(Inst::new(
            Opcode::Identity,
            vec![Value::Inst(InstRef {
                block: 0,
                inst: construct,
            })],
        ));

        assert_eq!(
            eval_const_offset_vec(
                &program,
                &Value::Inst(InstRef {
                    block: 0,
                    inst: identity,
                })
            ),
            Some("ivec2(3,7)".to_string())
        );
    }

    #[test]
    fn is_texture_msaa_reads_texture_definition_like_upstream() {
        let profile = crate::profile::Profile::default();
        let runtime_info = crate::runtime_info::RuntimeInfo::default();
        let mut bindings = crate::backend::bindings::Bindings::default();
        let mut program = crate::ir::Program::new(crate::stage::Stage::Fragment);
        program
            .info
            .texture_descriptors
            .push(crate::shader_info::TextureDescriptor {
                handle_program: u32::MAX,
                texture_type: TextureType::Color2D,
                is_depth: false,
                is_multisample: true,
                is_integer: false,
                has_secondary: false,
                cbuf_index: 0,
                cbuf_offset: 0,
                shift_left: 0,
                secondary_cbuf_index: 0,
                secondary_cbuf_offset: 0,
                secondary_shift_left: 0,
                count: 1,
                size_shift: 3,
            });
        let ctx = EmitContext::new(&program, &mut bindings, &profile, &runtime_info);
        let info = Tii {
            descriptor_index: 0,
            texture_type: TextureType::Color2D as u8,
            ..Default::default()
        };

        assert!(is_texture_msaa(&ctx, info));
    }

    #[test]
    fn fp_control_unused_in_image_helpers() {
        // Sanity that FpControl import is fine (also exercises types module).
        let c = crate::ir::types::FpControl {
            no_contraction: false,
            rounding: FpRounding::DontCare,
            fmz_mode: FmzMode::None,
        };
        let _ = c.to_u32();
    }
}
