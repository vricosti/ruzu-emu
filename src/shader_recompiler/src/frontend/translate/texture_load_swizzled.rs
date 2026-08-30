// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/frontend/maxwell/translate/impl/texture_load_swizzled.cpp
//!
//! Implements TLDS (texture load swizzled, compact dual-destination encoding).

use super::{field, TranslatorVisitor};
use crate::ir::types::TextureInstInfo;
use crate::ir::value::Value;
use crate::shader_info::TextureType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
enum Precision {
    F16 = 0,
    F32 = 1,
}

const R: u32 = 1;
const G: u32 = 2;
const B: u32 = 4;
const A: u32 = 8;
const RG_LUT: [u32; 8] = [R, G, B, A, R | G, R | A, G | A, B | A];
const RGBA_LUT: [u32; 5] = [R | G | B, R | G | A, R | B | A, G | B | A, R | G | B | A];

fn precision(insn: u64) -> Precision {
    match field(insn, 59, 1) {
        0 => Precision::F16,
        _ => Precision::F32,
    }
}

fn check_alignment(reg: u32, alignment: u32) {
    if reg % alignment != 0 {
        panic!("Unaligned source register {}", reg);
    }
}

fn add_reg(reg: u32, offset: u32) -> u32 {
    reg.checked_add(offset)
        .filter(|&value| value <= 255)
        .unwrap_or_else(|| panic!("Register overflow {} + {}", reg, offset))
}

fn make_offset(v: &mut TranslatorVisitor, reg: u32) -> Value {
    let value = v.x(reg);
    let x =
        v.ir.bit_field_s_extract(value, Value::ImmU32(0), Value::ImmU32(4));
    let y =
        v.ir.bit_field_s_extract(value, Value::ImmU32(4), Value::ImmU32(4));
    v.ir.composite_construct_u32x2(x, y)
}

fn sample(v: &mut TranslatorVisitor, insn: u64) -> Value {
    let cbuf_offset = field(insn, 36, 13) * 4;
    let handle = Value::ImmU32(cbuf_offset);
    let reg_a = field(insn, 8, 8);
    let reg_b = field(insn, 20, 8);
    let encoding = field(insn, 53, 4);

    let mut lod = Value::ImmU32(0);
    let mut offset = Value::Void;
    let mut multisample = Value::Void;
    let (texture_type, coords) = match encoding {
        0 => (TextureType::Color1D, v.x(reg_a)),
        1 => {
            lod = v.x(reg_b);
            (TextureType::Color1D, v.x(reg_a))
        }
        2 => {
            let x = v.x(reg_a);
            let y = v.x(reg_b);
            (TextureType::Color2D, v.ir.composite_construct_u32x2(x, y))
        }
        4 => {
            check_alignment(reg_a, 2);
            let x = v.x(reg_a);
            let y = v.x(add_reg(reg_a, 1));
            offset = make_offset(v, reg_b);
            (TextureType::Color2D, v.ir.composite_construct_u32x2(x, y))
        }
        5 => {
            check_alignment(reg_a, 2);
            let x = v.x(reg_a);
            let y = v.x(add_reg(reg_a, 1));
            lod = v.x(reg_b);
            (TextureType::Color2D, v.ir.composite_construct_u32x2(x, y))
        }
        6 => {
            check_alignment(reg_a, 2);
            let x = v.x(reg_a);
            let y = v.x(add_reg(reg_a, 1));
            multisample = v.x(reg_b);
            (TextureType::Color2D, v.ir.composite_construct_u32x2(x, y))
        }
        7 => {
            check_alignment(reg_a, 2);
            let x = v.x(reg_a);
            let y = v.x(add_reg(reg_a, 1));
            let z = v.x(reg_b);
            (
                TextureType::Color3D,
                v.ir.composite_construct_u32x3(x, y, z),
            )
        }
        8 => {
            check_alignment(reg_b, 2);
            let raw_array = v.x(reg_a);
            let array =
                v.ir.bit_field_u_extract(raw_array, Value::ImmU32(0), Value::ImmU32(16));
            let x = v.x(reg_b);
            let y = v.x(add_reg(reg_b, 1));
            (
                TextureType::ColorArray2D,
                v.ir.composite_construct_u32x3(x, y, array),
            )
        }
        12 => {
            check_alignment(reg_a, 2);
            check_alignment(reg_b, 2);
            let x = v.x(reg_a);
            let y = v.x(add_reg(reg_a, 1));
            lod = v.x(reg_b);
            offset = make_offset(v, add_reg(reg_b, 1));
            (TextureType::Color2D, v.ir.composite_construct_u32x2(x, y))
        }
        _ => panic!("Illegal encoding {}", encoding),
    };

    let info = TextureInstInfo {
        relaxed_precision: precision(insn) == Precision::F16,
        texture_type: texture_type as u8,
        ..Default::default()
    };
    v.ir.image_fetch_full(handle, coords, offset, lod, multisample, info.to_u32())
}

fn swizzle(insn: u64) -> u32 {
    let encoding = field(insn, 50, 3) as usize;
    let dest_reg_b = field(insn, 28, 8);
    if dest_reg_b == 255 {
        *RG_LUT
            .get(encoding)
            .unwrap_or_else(|| panic!("Illegal RG encoding {}", encoding))
    } else {
        *RGBA_LUT
            .get(encoding)
            .unwrap_or_else(|| panic!("Illegal RGBA encoding {}", encoding))
    }
}

fn extract(v: &mut TranslatorVisitor, sample: Value, component: u32) -> Value {
    v.ir.composite_extract_f32x4(sample, Value::ImmU32(component))
}

fn reg_store_component32(insn: u64, index: u32) -> u32 {
    let dest_reg_a = field(insn, 0, 8);
    let dest_reg_b = field(insn, 28, 8);
    match index {
        0 => dest_reg_a,
        1 => {
            check_alignment(dest_reg_a, 2);
            add_reg(dest_reg_a, 1)
        }
        2 => dest_reg_b,
        3 => {
            check_alignment(dest_reg_b, 2);
            add_reg(dest_reg_b, 1)
        }
        _ => panic!("Invalid store index {}", index),
    }
}

fn store32(v: &mut TranslatorVisitor, insn: u64, sample: Value) {
    let swizzle = swizzle(insn);
    let mut store_index = 0;
    for component in 0..4 {
        if ((swizzle >> component) & 1) == 0 {
            continue;
        }
        let dest = reg_store_component32(insn, store_index);
        let value = extract(v, sample, component);
        v.set_f(dest, value);
        store_index += 1;
    }
}

fn pack(v: &mut TranslatorVisitor, lhs: Value, rhs: Value) -> Value {
    let pair = v.ir.composite_construct_f32x2(lhs, rhs);
    v.ir.pack_half_2x16(pair)
}

fn store16(v: &mut TranslatorVisitor, insn: u64, sample: Value) {
    let swizzle = swizzle(insn);
    let mut store_index = 0usize;
    let mut swizzled = [Value::Void; 4];
    for component in 0..4 {
        if ((swizzle >> component) & 1) == 0 {
            continue;
        }
        swizzled[store_index] = extract(v, sample, component);
        store_index += 1;
    }
    let zero = Value::ImmF32(0.0);
    let dest_reg_a = field(insn, 0, 8);
    let dest_reg_b = field(insn, 28, 8);
    match store_index {
        1 => {
            let packed = pack(v, swizzled[0], zero);
            v.set_x(dest_reg_a, packed);
        }
        2..=4 => {
            let packed_a = pack(v, swizzled[0], swizzled[1]);
            v.set_x(dest_reg_a, packed_a);
            match store_index {
                2 => {}
                3 => {
                    let packed_b = pack(v, swizzled[2], zero);
                    v.set_x(dest_reg_b, packed_b);
                }
                4 => {
                    let packed_b = pack(v, swizzled[2], swizzled[3]);
                    v.set_x(dest_reg_b, packed_b);
                }
                _ => unreachable!(),
            }
        }
        _ => {}
    }
}

/// TLDS — Texture Load Swizzled.
///
/// Like TLD but with the TEXS-style compact encoding that packs two
/// destination registers and a swizzle mask into one instruction word.
/// Uses integer coordinates and an explicit LOD.
///
/// Upstream: `TranslatorVisitor::TLDS(u64 insn)`
pub fn tlds(tv: &mut TranslatorVisitor, insn: u64) {
    let sample = sample(tv, insn);
    if precision(insn) == Precision::F32 {
        store32(tv, insn, sample);
    } else {
        store16(tv, insn, sample);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::basic_block::Block;
    use crate::ir::opcodes::Opcode;
    use crate::ir::program::Program;
    use crate::ir::types::ShaderStage;

    fn fresh_program() -> Program {
        let mut program = Program::new(ShaderStage::Fragment);
        program.blocks.push(Block::new());
        program
    }

    #[test]
    fn tlds_f32_encoding12_preserves_lod_offset_and_swizzle() {
        let mut program = fresh_program();
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        let insn = 4u64
            | (8u64 << 8)
            | (20u64 << 20)
            | (12u64 << 28)
            | (7u64 << 36)
            | (4u64 << 50)
            | (12u64 << 53)
            | (1u64 << 59);

        tlds(&mut tv, insn);

        let fetch = tv.ir.program.blocks[0]
            .iter()
            .find(|inst| inst.opcode == Opcode::BoundImageFetch)
            .expect("TLDS should emit BoundImageFetch before texture pass");
        assert_eq!(fetch.args.len(), 5);
        assert_eq!(fetch.args[0], Value::ImmU32(28));
        assert!(!fetch.args[2].is_void(), "AOFFI offset must be preserved");
        assert!(!fetch.args[3].is_void(), "LOD operand must be preserved");
        assert!(fetch.args[4].is_void(), "encoding 12 is not multisample");

        let stores = tv.ir.program.blocks[0]
            .iter()
            .filter(|inst| inst.opcode == Opcode::SetRegister)
            .count();
        assert_eq!(stores, 4);
    }

    #[test]
    fn tlds_f16_single_component_packs_with_zero() {
        let mut program = fresh_program();
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        let insn = 4u64 | (8u64 << 8) | (20u64 << 20) | (255u64 << 28);

        tlds(&mut tv, insn);

        let pack_count = tv.ir.program.blocks[0]
            .iter()
            .filter(|inst| inst.opcode == Opcode::PackHalf2x16)
            .count();
        assert_eq!(pack_count, 1);
    }
}
