// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/frontend/maxwell/translate/impl/texture_gather_swizzled.cpp
//!
//! Upstream `TLD4S` translates into `ImageGather`/`ImageGatherDref` and
//! stores either four F32 values or two packed F16 pairs.

use super::{field, TranslatorVisitor};
use crate::ir::types::TextureInstInfo;
use crate::ir::value::Value;
use crate::shader_info::TextureType;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
enum Precision {
    F32 = 0,
    F16 = 1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
enum ComponentType {
    R = 0,
    G = 1,
    B = 2,
    A = 3,
}

fn precision(insn: u64) -> Precision {
    match field(insn, 55, 1) {
        0 => Precision::F32,
        _ => Precision::F16,
    }
}

fn component_type(insn: u64) -> ComponentType {
    match field(insn, 52, 2) {
        0 => ComponentType::R,
        1 => ComponentType::G,
        2 => ComponentType::B,
        _ => ComponentType::A,
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
        v.ir.bit_field_s_extract(value, Value::ImmU32(0), Value::ImmU32(6));
    let y =
        v.ir.bit_field_s_extract(value, Value::ImmU32(8), Value::ImmU32(6));
    v.ir.composite_construct_u32x2(x, y)
}

fn sample(v: &mut TranslatorVisitor, insn: u64) -> Value {
    let handle = Value::ImmU32(field(insn, 36, 13) * 4);
    let reg_a = field(insn, 8, 8);
    let reg_b = field(insn, 20, 8);
    let aoffi = field(insn, 51, 1) != 0;
    let dc = field(insn, 50, 1) != 0;
    let texture_type = TextureType::Color2D;
    let info = TextureInstInfo {
        relaxed_precision: precision(insn) == Precision::F16,
        gather_component: component_type(insn) as u8,
        texture_type: texture_type as u8,
        is_depth: dc,
        ..Default::default()
    };
    if aoffi {
        check_alignment(reg_a, 2);
        let coords = {
            let x = v.f(reg_a);
            let y = v.f(add_reg(reg_a, 1));
            v.ir.composite_construct_f32x2(x, y)
        };
        let offset = make_offset(v, reg_b);
        if dc {
            check_alignment(reg_b, 2);
            let dref = v.f(add_reg(reg_b, 1));
            return v.ir.image_gather_dref_full(
                handle,
                coords,
                offset,
                Value::Void,
                dref,
                info.to_u32(),
            );
        }
        return v
            .ir
            .image_gather_full(handle, coords, offset, Value::Void, info.to_u32());
    }

    if dc {
        check_alignment(reg_a, 2);
        let coords = {
            let x = v.f(reg_a);
            let y = v.f(add_reg(reg_a, 1));
            v.ir.composite_construct_f32x2(x, y)
        };
        let dref = v.f(reg_b);
        return v.ir.image_gather_dref_full(
            handle,
            coords,
            Value::Void,
            Value::Void,
            dref,
            info.to_u32(),
        );
    }

    let coords = {
        let x = v.f(reg_a);
        let y = v.f(reg_b);
        v.ir.composite_construct_f32x2(x, y)
    };
    v.ir.image_gather_full(handle, coords, Value::Void, Value::Void, info.to_u32())
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
        _ => panic!("Invalid TLD4S store index {}", index),
    }
}

fn store32(v: &mut TranslatorVisitor, insn: u64, sample: Value) {
    for component in 0..4 {
        let dest = reg_store_component32(insn, component);
        let value =
            v.ir.composite_extract_f32x4(sample, Value::ImmU32(component));
        v.set_f(dest, value);
    }
}

fn pack(v: &mut TranslatorVisitor, lhs: Value, rhs: Value) -> Value {
    let pair = v.ir.composite_construct_f32x2(lhs, rhs);
    v.ir.pack_half_2x16(pair)
}

fn store16(v: &mut TranslatorVisitor, insn: u64, sample: Value) {
    let mut swizzled = [Value::Void; 4];
    for component in 0..4 {
        swizzled[component as usize] =
            v.ir.composite_extract_f32x4(sample, Value::ImmU32(component));
    }
    let dest_reg_a = field(insn, 0, 8);
    let dest_reg_b = field(insn, 28, 8);
    let packed_a = pack(v, swizzled[0], swizzled[1]);
    v.set_x(dest_reg_a, packed_a);
    let packed_b = pack(v, swizzled[2], swizzled[3]);
    v.set_x(dest_reg_b, packed_b);
}

/// TLD4S — Texture Gather Swizzled.
///
/// Upstream: `TranslatorVisitor::TLD4S(u64 insn)`.
pub fn tld4s(tv: &mut TranslatorVisitor, insn: u64) {
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
    fn tld4s_f32_depth_aoffi_emits_full_gather_dref() {
        let mut program = fresh_program();
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        let insn = 4u64
            | (8u64 << 8)
            | (20u64 << 20)
            | (12u64 << 28)
            | (7u64 << 36)
            | (1u64 << 50)
            | (1u64 << 51)
            | (2u64 << 52);

        tld4s(&mut tv, insn);

        let gather = tv.ir.program.blocks[0]
            .iter()
            .find(|inst| inst.opcode == Opcode::BoundImageGatherDref)
            .expect("TLD4S depth+offset should emit BoundImageGatherDref");
        assert_eq!(gather.args.len(), 5);
        assert_eq!(gather.args[0], Value::ImmU32(28));
        assert!(!gather.args[2].is_void(), "AOFFI offset must be preserved");
        assert!(gather.args[3].is_void(), "TLD4S has no PTP offset2 path");
        assert!(!gather.args[4].is_void(), "DREF operand must be preserved");
    }

    #[test]
    fn tld4s_f16_packs_two_dest_registers() {
        let mut program = fresh_program();
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        let insn = 4u64 | (8u64 << 8) | (20u64 << 20) | (12u64 << 28) | (1u64 << 55);

        tld4s(&mut tv, insn);

        let pack_count = tv.ir.program.blocks[0]
            .iter()
            .filter(|inst| inst.opcode == Opcode::PackHalf2x16)
            .count();
        assert_eq!(pack_count, 2);
    }
}
