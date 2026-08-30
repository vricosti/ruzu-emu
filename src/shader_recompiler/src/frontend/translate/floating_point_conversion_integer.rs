// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/frontend/maxwell/translate/impl/floating_point_conversion_integer.cpp

use super::{bit, field, sfield, TranslatorVisitor};
use crate::frontend::maxwell_opcodes::MaxwellOpcode;
use crate::ir::types::{FmzMode, FpControl, FpRounding};
use crate::ir::value::Value;

const DEST_FORMAT_I16: u32 = 1;
const DEST_FORMAT_I32: u32 = 2;
const DEST_FORMAT_I64: u32 = 3;
const SRC_FORMAT_F16: u32 = 1;
const SRC_FORMAT_F32: u32 = 2;
const SRC_FORMAT_F64: u32 = 3;

fn bit_size(dest_format: u32) -> u32 {
    match dest_format {
        DEST_FORMAT_I16 => 16,
        DEST_FORMAT_I32 => 32,
        DEST_FORMAT_I64 => 64,
        _ => panic!("invalid F2I destination format {dest_format}"),
    }
}

fn clamp_bounds(dest_format: u32, is_signed: bool) -> (f64, f64) {
    match (dest_format, is_signed) {
        (DEST_FORMAT_I16, true) => (i16::MAX as f64, i16::MIN as f64),
        (DEST_FORMAT_I32, true) => (i32::MAX as f64, i32::MIN as f64),
        (DEST_FORMAT_I64, true) => (i64::MAX as f64, i64::MIN as f64),
        (DEST_FORMAT_I16, false) => (u16::MAX as f64, u16::MIN as f64),
        (DEST_FORMAT_I32, false) => (u32::MAX as f64, u32::MIN as f64),
        (DEST_FORMAT_I64, false) => (u64::MAX as f64, u64::MIN as f64),
        _ => panic!("invalid F2I destination format {dest_format}"),
    }
}

fn unpack_cbuf_f64(tv: &mut TranslatorVisitor, insn: u64) -> Value {
    let offset = sfield(insn, 20, 14);
    let binding = field(insn, 34, 5);
    if binding >= 18 {
        panic!("out of bounds F2I constant buffer binding {binding}");
    }
    if !(0..0x4000).contains(&offset) {
        panic!("out of bounds F2I constant buffer offset {}", offset * 4);
    }
    if offset % 2 != 0 {
        panic!("unaligned F64 constant buffer offset {}", offset * 4);
    }
    let cbuf_data = tv
        .ir
        .get_cbuf_u32(Value::ImmU32(binding), Value::ImmU32(offset as u32 * 4 + 4));
    let vector = tv.ir.composite_construct_u32x2(Value::ImmU32(0), cbuf_data);
    tv.ir.pack_double_2x32(vector)
}

fn round_source(
    tv: &mut TranslatorVisitor,
    value: Value,
    src_bits: u32,
    rounding: u32,
    control: FpControl,
) -> Value {
    match (src_bits, rounding) {
        (16, 0) => tv.ir.fp_round_even_16_with_control(value, control),
        (16, 1) => tv.ir.fp_floor_16_with_control(value, control),
        (16, 2) => tv.ir.fp_ceil_16_with_control(value, control),
        (16, 3) => tv.ir.fp_trunc_16_with_control(value, control),
        (32, 0) => tv.ir.fp_round_even_32_with_control(value, control),
        (32, 1) => tv.ir.fp_floor_32_with_control(value, control),
        (32, 2) => tv.ir.fp_ceil_32_with_control(value, control),
        (32, 3) => tv.ir.fp_trunc_32_with_control(value, control),
        (64, 0) => tv.ir.fp_round_even_64_with_control(value, control),
        (64, 1) => tv.ir.fp_floor_64_with_control(value, control),
        (64, 2) => tv.ir.fp_ceil_64_with_control(value, control),
        (64, 3) => tv.ir.fp_trunc_64_with_control(value, control),
        _ => panic!("invalid F2I rounding {rounding}"),
    }
}

fn translate_f2i(tv: &mut TranslatorVisitor, insn: u64, src_a: Value) {
    let dest_reg = field(insn, 0, 8);
    let dest_format = field(insn, 8, 2);
    let src_format = field(insn, 10, 2);
    let is_signed = bit(insn, 12);
    let rounding = field(insn, 39, 2);
    let ftz = bit(insn, 44);
    let abs = bit(insn, 45);
    let cc = bit(insn, 47);
    let neg = bit(insn, 49);
    let src_bits = match src_format {
        SRC_FORMAT_F16 => 16,
        SRC_FORMAT_F32 => 32,
        SRC_FORMAT_F64 => 64,
        _ => panic!("invalid F2I source format {src_format}"),
    };
    let denorm_cares = src_format != SRC_FORMAT_F16
        && src_format != SRC_FORMAT_F64
        && dest_format != DEST_FORMAT_I64;
    let control = FpControl {
        no_contraction: true,
        rounding: FpRounding::DontCare,
        fmz_mode: if denorm_cares {
            if ftz {
                FmzMode::FTZ
            } else {
                FmzMode::None
            }
        } else {
            FmzMode::DontCare
        },
    };
    let op_a = match src_bits {
        16 => tv.ir.fp_abs_neg_16(src_a, abs, neg),
        32 => tv.ir.fp_abs_neg_32(src_a, abs, neg),
        64 => tv.ir.fp_abs_neg_64(src_a, abs, neg),
        _ => unreachable!(),
    };
    let rounded = round_source(tv, op_a, src_bits, rounding, control);
    let (max_bound, min_bound) = clamp_bounds(dest_format, is_signed);
    let intermediate = match src_bits {
        16 => {
            let max = tv.ir.fp_convert(
                16,
                Value::ImmF32(max_bound as f32),
                32,
                FpControl::default(),
            );
            let min = tv.ir.fp_convert(
                16,
                Value::ImmF32(min_bound as f32),
                32,
                FpControl::default(),
            );
            tv.ir.fp_clamp_16(rounded, min, max)
        }
        32 => tv.ir.fp_clamp_32(
            rounded,
            Value::ImmF32(min_bound as f32),
            Value::ImmF32(max_bound as f32),
        ),
        64 => tv
            .ir
            .fp_clamp_64(rounded, Value::ImmF64(min_bound), Value::ImmF64(max_bound)),
        _ => unreachable!(),
    };

    let result_bits = bit_size(dest_format).max(32);
    let mut result = tv
        .ir
        .convert_f_to_i(result_bits, src_bits, is_signed, intermediate);
    let special_nan_cases = (src_format == SRC_FORMAT_F64) != (dest_format == DEST_FORMAT_I64);
    let mut handled_special_case = false;
    if special_nan_cases {
        let is_nan = match src_bits {
            16 => tv.ir.fp_is_nan_16(op_a),
            32 => tv.ir.fp_is_nan_32(op_a),
            64 => tv.ir.fp_is_nan_64(op_a),
            _ => unreachable!(),
        };
        if dest_format == DEST_FORMAT_I32 {
            handled_special_case = true;
            result = tv.ir.select_u32(is_nan, Value::ImmU32(0x8000_0000), result);
        } else if dest_format == DEST_FORMAT_I64 {
            handled_special_case = true;
            result = tv
                .ir
                .select_u64(is_nan, Value::ImmU64(0x8000_0000_0000_0000), result);
        }
    }
    if !handled_special_case && is_signed {
        let is_nan = match src_bits {
            16 => tv.ir.fp_is_nan_16(op_a),
            32 => tv.ir.fp_is_nan_32(op_a),
            64 => tv.ir.fp_is_nan_64(op_a),
            _ => unreachable!(),
        };
        result = if result_bits == 64 {
            tv.ir.select_u64(is_nan, Value::ImmU64(0), result)
        } else {
            tv.ir.select_u32(is_nan, Value::ImmU32(0), result)
        };
    }

    if result_bits == 64 {
        tv.set_l(dest_reg, result);
    } else {
        tv.set_x(dest_reg, result);
    }
    if cc {
        panic!("F2I CC not implemented upstream");
    }
}

pub fn f2i(tv: &mut TranslatorVisitor, insn: u64, opcode: MaxwellOpcode) {
    let src_format = field(insn, 10, 2);
    let half = field(insn, 41, 1);
    let src = match opcode {
        MaxwellOpcode::F2I_reg => {
            let src_reg = field(insn, 20, 8);
            match src_format {
                SRC_FORMAT_F16 => {
                    let packed = tv.x(src_reg);
                    let vector = tv.ir.unpack_float_2x16(packed);
                    tv.ir.composite_extract_f16x2(vector, half)
                }
                SRC_FORMAT_F32 => tv.f(src_reg),
                SRC_FORMAT_F64 => {
                    let lo = tv.x(src_reg);
                    let hi = tv.x(src_reg + 1);
                    let vector = tv.ir.composite_construct_u32x2(lo, hi);
                    tv.ir.pack_double_2x32(vector)
                }
                _ => panic!("invalid F2I source format {src_format}"),
            }
        }
        MaxwellOpcode::F2I_cbuf => match src_format {
            SRC_FORMAT_F16 => {
                let packed = tv.get_cbuf(insn);
                let vector = tv.ir.unpack_float_2x16(packed);
                tv.ir.composite_extract_f16x2(vector, half)
            }
            SRC_FORMAT_F32 => tv.get_float_cbuf(insn),
            SRC_FORMAT_F64 => unpack_cbuf_f64(tv, insn),
            _ => panic!("invalid F2I source format {src_format}"),
        },
        MaxwellOpcode::F2I_imm => panic!("F2I_imm not implemented upstream"),
        _ => unreachable!("invalid F2I opcode {opcode:?}"),
    };
    translate_f2i(tv, insn, src);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::basic_block::Block;
    use crate::ir::opcodes::Opcode;
    use crate::ir::program::Program;
    use crate::ir::types::ShaderStage;

    fn translate(insn: u64, opcode: MaxwellOpcode) -> Vec<Opcode> {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        let mut visitor = TranslatorVisitor::new(&mut program, 0);
        f2i(&mut visitor, insn, opcode);
        program.blocks[0].iter().map(|inst| inst.opcode).collect()
    }

    #[test]
    fn f2i_f16_uses_native_clamp_and_conversion() {
        let insn = 1u64
            | (DEST_FORMAT_I32 as u64) << 8
            | (SRC_FORMAT_F16 as u64) << 10
            | 1u64 << 12
            | 2u64 << 20;
        let opcodes = translate(insn, MaxwellOpcode::F2I_reg);
        assert!(opcodes.contains(&Opcode::FPClamp16));
        assert!(opcodes.contains(&Opcode::ConvertS32F16));
    }

    #[test]
    fn f2i_f64_to_i64_writes_a_register_pair() {
        let insn = 2u64
            | (DEST_FORMAT_I64 as u64) << 8
            | (SRC_FORMAT_F64 as u64) << 10
            | 1u64 << 12
            | 4u64 << 20;
        let opcodes = translate(insn, MaxwellOpcode::F2I_reg);
        assert!(opcodes.contains(&Opcode::FPClamp64));
        assert!(opcodes.contains(&Opcode::ConvertS64F64));
        assert!(opcodes.contains(&Opcode::UnpackUint2x32));
    }

    #[test]
    #[should_panic(expected = "F2I_imm not implemented upstream")]
    fn f2i_immediate_matches_upstream_rejection() {
        let _ = translate(0, MaxwellOpcode::F2I_imm);
    }
}
