// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SPIR-V conversion emission — maps to zuyu's
//! `backend/spirv/emit_spirv_convert.cpp`.

use super::spirv_emit_context::SpirvEmitContext;
use rspirv::spirv::Word;

// ── Helpers matching upstream anonymous namespace ─────────────────────────

/// Extract lower 16 bits as unsigned.
fn extract_u16(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    if ctx.profile.support_int16 {
        ctx.builder.u_convert(ctx.u16_type, None, value).unwrap()
    } else {
        let zero = ctx.const_zero_u32;
        let sixteen = ctx.constant_u32(16);
        ctx.builder
            .bit_field_u_extract(ctx.u32_type, None, value, zero, sixteen)
            .unwrap()
    }
}

/// Extract lower 16 bits as signed (sign-extend).
fn extract_s16(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    if ctx.profile.support_int16 {
        ctx.builder.s_convert(ctx.i16_type, None, value).unwrap()
    } else {
        let zero = ctx.const_zero_u32;
        let sixteen = ctx.constant_u32(16);
        ctx.builder
            .bit_field_s_extract(ctx.u32_type, None, value, zero, sixteen)
            .unwrap()
    }
}

/// Extract lower 8 bits as unsigned.
fn extract_u8(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    if ctx.profile.support_int8 {
        ctx.builder.u_convert(ctx.u8_type, None, value).unwrap()
    } else {
        let zero = ctx.const_zero_u32;
        let eight = ctx.constant_u32(8);
        ctx.builder
            .bit_field_u_extract(ctx.u32_type, None, value, zero, eight)
            .unwrap()
    }
}

/// Extract lower 8 bits as signed (sign-extend).
fn extract_s8(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    if ctx.profile.support_int8 {
        ctx.builder.s_convert(ctx.i8_type, None, value).unwrap()
    } else {
        let zero = ctx.const_zero_u32;
        let eight = ctx.constant_u32(8);
        ctx.builder
            .bit_field_s_extract(ctx.u32_type, None, value, zero, eight)
            .unwrap()
    }
}

fn emit_convert_s16(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    if ctx.profile.support_int16 {
        let converted = ctx
            .builder
            .convert_f_to_s(ctx.u16_type, None, value)
            .unwrap();
        ctx.builder
            .s_convert(ctx.u32_type, None, converted)
            .unwrap()
    } else {
        let converted = ctx
            .builder
            .convert_f_to_s(ctx.u32_type, None, value)
            .unwrap();
        extract_s16(ctx, converted)
    }
}

fn emit_convert_u16(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    if ctx.profile.support_int16 {
        let converted = ctx
            .builder
            .convert_f_to_u(ctx.u16_type, None, value)
            .unwrap();
        ctx.builder
            .u_convert(ctx.u32_type, None, converted)
            .unwrap()
    } else {
        let converted = ctx
            .builder
            .convert_f_to_u(ctx.u32_type, None, value)
            .unwrap();
        extract_u16(ctx, converted)
    }
}

// ── Signed float-to-integer conversions ──────────────────────────────────

pub fn emit_convert_s16_f16(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    emit_convert_s16(ctx, value)
}

/// ConvertS32F32: `OpConvertFToS` F32 -> S32.
pub fn emit_convert_s32_f32(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    if ctx.profile.has_broken_signed_operations {
        let signed = ctx
            .builder
            .convert_f_to_s(ctx.i32_type, None, value)
            .unwrap();
        ctx.builder.bitcast(ctx.u32_type, None, signed).unwrap()
    } else {
        ctx.builder
            .convert_f_to_s(ctx.u32_type, None, value)
            .unwrap()
    }
}

/// ConvertS32F64: `OpConvertFToS` F64 -> S32.
pub fn emit_convert_s32_f64(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_f_to_s(ctx.u32_type, None, value)
        .unwrap()
}

/// ConvertS16F32: `OpConvertFToS` F32 -> S16 (via S32 + extract).
pub fn emit_convert_s16_f32(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    emit_convert_s16(ctx, value)
}

pub fn emit_convert_s16_f64(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    emit_convert_s16(ctx, value)
}

/// ConvertS32F16: `OpConvertFToS` F16 -> S32.
pub fn emit_convert_s32_f16(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_f_to_s(ctx.u32_type, None, value)
        .unwrap()
}

/// ConvertS64F32: `OpConvertFToS` F32 -> S64.
pub fn emit_convert_s64_f32(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_f_to_s(ctx.u64_type, None, value)
        .unwrap()
}

/// ConvertS64F64: `OpConvertFToS` F64 -> S64.
pub fn emit_convert_s64_f64(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_f_to_s(ctx.u64_type, None, value)
        .unwrap()
}

/// ConvertS64F16: `OpConvertFToS` F16 -> S64.
pub fn emit_convert_s64_f16(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_f_to_s(ctx.u64_type, None, value)
        .unwrap()
}

// ── Unsigned float-to-integer conversions ────────────────────────────────

pub fn emit_convert_u16_f16(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    emit_convert_u16(ctx, value)
}

/// ConvertU32F32: `OpConvertFToU` F32 -> U32.
pub fn emit_convert_u32_f32(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_f_to_u(ctx.u32_type, None, value)
        .unwrap()
}

/// ConvertU32F64: `OpConvertFToU` F64 -> U32.
pub fn emit_convert_u32_f64(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_f_to_u(ctx.u32_type, None, value)
        .unwrap()
}

/// ConvertU16F32: `OpConvertFToU` F32 -> U16 (via U32 + extract).
pub fn emit_convert_u16_f32(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    emit_convert_u16(ctx, value)
}

pub fn emit_convert_u16_f64(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    emit_convert_u16(ctx, value)
}

/// ConvertU32F16: `OpConvertFToU` F16 -> U32.
pub fn emit_convert_u32_f16(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_f_to_u(ctx.u32_type, None, value)
        .unwrap()
}

/// ConvertU64F32: `OpConvertFToU` F32 -> U64.
pub fn emit_convert_u64_f32(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_f_to_u(ctx.u64_type, None, value)
        .unwrap()
}

/// ConvertU64F64: `OpConvertFToU` F64 -> U64.
pub fn emit_convert_u64_f64(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_f_to_u(ctx.u64_type, None, value)
        .unwrap()
}

/// ConvertU64F16: `OpConvertFToU` F16 -> U64.
pub fn emit_convert_u64_f16(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_f_to_u(ctx.u64_type, None, value)
        .unwrap()
}

// ── Integer width conversions ────────────────────────────────────────────

/// ConvertU64U32: `OpUConvert` U32 -> U64.
pub fn emit_convert_u64_u32(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder.u_convert(ctx.u64_type, None, value).unwrap()
}

/// ConvertU32U64: `OpUConvert` U64 -> U32.
pub fn emit_convert_u32_u64(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder.u_convert(ctx.u32_type, None, value).unwrap()
}

// ── Signed integer-to-float conversions ──────────────────────────────────

pub fn emit_convert_f16_s8(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    let value = extract_s8(ctx, value);
    ctx.builder
        .convert_s_to_f(ctx.f16_type, None, value)
        .unwrap()
}

pub fn emit_convert_f16_s16(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    let value = extract_s16(ctx, value);
    ctx.builder
        .convert_s_to_f(ctx.f16_type, None, value)
        .unwrap()
}

pub fn emit_convert_f16_s32(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_s_to_f(ctx.f16_type, None, value)
        .unwrap()
}

pub fn emit_convert_f16_s64(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_s_to_f(ctx.f16_type, None, value)
        .unwrap()
}

/// ConvertF32S8: `OpConvertSToF` S8 -> F32 (via sign-extend + convert).
pub fn emit_convert_f32_s8(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    let s_val = extract_s8(ctx, value);
    ctx.builder
        .convert_s_to_f(ctx.f32_type, None, s_val)
        .unwrap()
}

/// ConvertF32S16: `OpConvertSToF` S16 -> F32 (via sign-extend + convert).
pub fn emit_convert_f32_s16(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    let s_val = extract_s16(ctx, value);
    ctx.builder
        .convert_s_to_f(ctx.f32_type, None, s_val)
        .unwrap()
}

/// ConvertF32S32: `OpConvertSToF` S32 -> F32.
pub fn emit_convert_f32_s32(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    let value = if ctx.profile.has_broken_signed_operations {
        ctx.builder.bitcast(ctx.i32_type, None, value).unwrap()
    } else {
        value
    };
    ctx.builder
        .convert_s_to_f(ctx.f32_type, None, value)
        .unwrap()
}

/// ConvertF32S64: `OpConvertSToF` S64 -> F32.
pub fn emit_convert_f32_s64(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_s_to_f(ctx.f32_type, None, value)
        .unwrap()
}

/// ConvertF64S32: `OpConvertSToF` S32 -> F64.
pub fn emit_convert_f64_s32(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    let value = if ctx.profile.has_broken_signed_operations {
        ctx.builder.bitcast(ctx.i32_type, None, value).unwrap()
    } else {
        value
    };
    ctx.builder
        .convert_s_to_f(ctx.f64_type, None, value)
        .unwrap()
}

pub fn emit_convert_f64_s8(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    let value = extract_s8(ctx, value);
    ctx.builder
        .convert_s_to_f(ctx.f64_type, None, value)
        .unwrap()
}

pub fn emit_convert_f64_s16(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    let value = extract_s16(ctx, value);
    ctx.builder
        .convert_s_to_f(ctx.f64_type, None, value)
        .unwrap()
}

pub fn emit_convert_f64_s64(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_s_to_f(ctx.f64_type, None, value)
        .unwrap()
}

// ── Unsigned integer-to-float conversions ────────────────────────────────

pub fn emit_convert_f16_u8(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    let value = extract_u8(ctx, value);
    ctx.builder
        .convert_u_to_f(ctx.f16_type, None, value)
        .unwrap()
}

pub fn emit_convert_f16_u16(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    let value = extract_u16(ctx, value);
    ctx.builder
        .convert_u_to_f(ctx.f16_type, None, value)
        .unwrap()
}

pub fn emit_convert_f16_u32(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_u_to_f(ctx.f16_type, None, value)
        .unwrap()
}

pub fn emit_convert_f16_u64(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_u_to_f(ctx.f16_type, None, value)
        .unwrap()
}

/// ConvertF32U8: `OpConvertUToF` U8 -> F32 (via extract + convert).
pub fn emit_convert_f32_u8(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    let u_val = extract_u8(ctx, value);
    ctx.builder
        .convert_u_to_f(ctx.f32_type, None, u_val)
        .unwrap()
}

/// ConvertF32U16: `OpConvertUToF` U16 -> F32 (via extract + convert).
pub fn emit_convert_f32_u16(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    let u_val = extract_u16(ctx, value);
    ctx.builder
        .convert_u_to_f(ctx.f32_type, None, u_val)
        .unwrap()
}

/// ConvertF32U32: `OpConvertUToF` U32 -> F32.
pub fn emit_convert_f32_u32(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_u_to_f(ctx.f32_type, None, value)
        .unwrap()
}

/// ConvertF32U64: `OpConvertUToF` U64 -> F32.
pub fn emit_convert_f32_u64(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_u_to_f(ctx.f32_type, None, value)
        .unwrap()
}

pub fn emit_convert_f64_u8(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    let value = extract_u8(ctx, value);
    ctx.builder
        .convert_u_to_f(ctx.f64_type, None, value)
        .unwrap()
}

pub fn emit_convert_f64_u16(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    let value = extract_u16(ctx, value);
    ctx.builder
        .convert_u_to_f(ctx.f64_type, None, value)
        .unwrap()
}

pub fn emit_convert_f64_u32(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_u_to_f(ctx.f64_type, None, value)
        .unwrap()
}

pub fn emit_convert_f64_u64(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder
        .convert_u_to_f(ctx.f64_type, None, value)
        .unwrap()
}

// ── Float-to-float conversions ───────────────────────────────────────────

/// ConvertF16F32: `OpFConvert` F32 -> F16, with upstream's non-Android
/// overflow normalization.
pub fn emit_convert_f16_f32(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    let result = ctx.builder.f_convert(ctx.f16_type, None, value).unwrap();
    #[cfg(target_os = "android")]
    {
        result
    }
    #[cfg(not(target_os = "android"))]
    {
        let is_overflowing = ctx.builder.is_nan(ctx.bool_type, None, result).unwrap();
        let zero = ctx.builder.constant_bit32(ctx.f16_type, 0);
        ctx.builder
            .select(ctx.f16_type, None, is_overflowing, zero, result)
            .unwrap()
    }
}

/// ConvertF32F16: `OpFConvert` F16 -> F32.
pub fn emit_convert_f32_f16(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder.f_convert(ctx.f32_type, None, value).unwrap()
}

/// ConvertF32F64: `OpFConvert` F64 -> F32.
pub fn emit_convert_f32_f64(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder.f_convert(ctx.f32_type, None, value).unwrap()
}

/// ConvertF64F32: `OpFConvert` F32 -> F64.
pub fn emit_convert_f64_f32(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder.f_convert(ctx.f64_type, None, value).unwrap()
}
