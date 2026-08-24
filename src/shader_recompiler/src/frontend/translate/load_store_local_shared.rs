// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/frontend/maxwell/translate/impl/load_store_local_shared.cpp

use super::{field, sfield, TranslatorVisitor};
use crate::ir::reg::Reg as IrReg;
use crate::ir::value::Value;

fn offset(tv: &mut TranslatorVisitor<'_>, insn: u64) -> Value {
    let offset_reg = IrReg::from_index(field(insn, 8, 8) as u8);
    if offset_reg.is_zero() {
        Value::ImmU32(field(insn, 20, 24))
    } else {
        let base = tv.x(offset_reg.index() as u32);
        tv.ir
            .iadd_32(base, Value::ImmU32(sfield(insn, 20, 24) as u32))
    }
}

fn reg(insn: u64) -> IrReg {
    IrReg::from_index(field(insn, 0, 8) as u8)
}

fn word_offset(tv: &mut TranslatorVisitor<'_>, insn: u64) -> (Value, Value) {
    let byte_offset = offset(tv, insn);
    let word_offset = if byte_offset.is_immediate() {
        Value::ImmU32(byte_offset.imm_u32() / 4)
    } else {
        tv.ir
            .shift_right_arithmetic_32(byte_offset.clone(), Value::ImmU32(2))
    };
    (word_offset, byte_offset)
}

fn get_size(insn: u64) -> (u32, bool) {
    match field(insn, 48, 3) {
        0 => (8, false),
        1 => (8, true),
        2 => (16, false),
        3 => (16, true),
        4 => (32, false),
        5 => (64, false),
        6 => (128, false),
        size => panic!("Invalid local/shared memory size {size}"),
    }
}

fn byte_offset(tv: &mut TranslatorVisitor<'_>, offset: Value) -> Value {
    let shifted = tv.ir.shift_left_logical_32(offset, Value::ImmU32(3));
    tv.ir.bitwise_and_32(shifted, Value::ImmU32(24))
}

fn short_offset(tv: &mut TranslatorVisitor<'_>, offset: Value) -> Value {
    let shifted = tv.ir.shift_left_logical_32(offset, Value::ImmU32(3));
    tv.ir.bitwise_and_32(shifted, Value::ImmU32(16))
}

fn local_memory_size(tv: &TranslatorVisitor<'_>) -> u32 {
    tv.env
        .map(crate::environment::Environment::local_memory_size)
        .or_else(|| {
            tv.sph
                .as_ref()
                .map(|sph| sph.local_memory_size() as u32)
        })
        .unwrap_or(tv.ir.program.local_memory_size)
}

fn load_local(tv: &mut TranslatorVisitor<'_>, word_offset: Value, offset: Value) -> Value {
    let size = Value::ImmU32(local_memory_size(tv));
    let in_bounds = tv.ir.u_less_than(offset, size);
    let value = tv.ir.load_local(word_offset);
    tv.ir.select_u32(in_bounds, value, Value::ImmU32(0))
}

pub fn ldl(tv: &mut TranslatorVisitor<'_>, insn: u64) {
    let (word_offset, offset) = word_offset(tv, insn);
    let word = load_local(tv, word_offset.clone(), offset.clone());
    let dest = reg(insn);
    let (bit_size, is_signed) = get_size(insn);

    match bit_size {
        8 => {
            let bit = byte_offset(tv, offset);
            let value = if is_signed {
                tv.ir.bit_field_s_extract(word, bit, Value::ImmU32(8))
            } else {
                tv.ir.bit_field_u_extract(word, bit, Value::ImmU32(8))
            };
            tv.set_x(dest.index() as u32, value);
        }
        16 => {
            let bit = short_offset(tv, offset);
            let value = if is_signed {
                tv.ir.bit_field_s_extract(word, bit, Value::ImmU32(16))
            } else {
                tv.ir.bit_field_u_extract(word, bit, Value::ImmU32(16))
            };
            tv.set_x(dest.index() as u32, value);
        }
        32 | 64 | 128 => {
            let words = bit_size / 32;
            if !dest.is_aligned(words as usize) {
                panic!("Unaligned LDL destination register {dest}");
            }
            tv.set_x(dest.index() as u32, word);
            for index in 1..words {
                let sub_word_offset = tv.ir.iadd_32(word_offset.clone(), Value::ImmU32(index));
                let sub_offset = tv.ir.iadd_32(offset.clone(), Value::ImmU32(index * 4));
                let value = load_local(tv, sub_word_offset, sub_offset);
                tv.set_x((dest + index as i32).index() as u32, value);
            }
        }
        _ => unreachable!("validated local memory size"),
    }
}

pub fn lds(tv: &mut TranslatorVisitor<'_>, insn: u64) {
    let offset = offset(tv, insn);
    let dest = reg(insn);
    let (bit_size, is_signed) = get_size(insn);

    match bit_size {
        8 => {
            let value = if is_signed {
                tv.ir.load_shared_s8(offset)
            } else {
                tv.ir.load_shared_u8(offset)
            };
            tv.set_x(dest.index() as u32, value);
        }
        16 => {
            let value = if is_signed {
                tv.ir.load_shared_s16(offset)
            } else {
                tv.ir.load_shared_u16(offset)
            };
            tv.set_x(dest.index() as u32, value);
        }
        32 => {
            let value = tv.ir.load_shared_u32(offset);
            tv.set_x(dest.index() as u32, value);
        }
        64 => {
            if !dest.is_aligned(2) {
                panic!("Unaligned LDS destination register {dest}");
            }
            let value = tv.ir.load_shared_u64(offset);
            for index in 0..2 {
                let element = tv.ir.composite_extract_u32x2_idx(value.clone(), index);
                tv.set_x((dest + index as i32).index() as u32, element);
            }
        }
        128 => {
            if !dest.is_aligned(4) {
                panic!("Unaligned LDS destination register {dest}");
            }
            let value = tv.ir.load_shared_u128(offset);
            for index in 0..4 {
                let element = tv
                    .ir
                    .composite_extract_u32x4(value.clone(), Value::ImmU32(index));
                tv.set_x((dest + index as i32).index() as u32, element);
            }
        }
        _ => unreachable!("validated shared memory size"),
    }
}

pub fn stl(tv: &mut TranslatorVisitor<'_>, insn: u64) {
    let (word_offset, offset) = word_offset(tv, insn);
    if offset.is_immediate() && offset.imm_u32() >= local_memory_size(tv) {
        log::warn!(
            "Storing local memory at 0x{:x} with a size of 0x{:x}, dropping",
            offset.imm_u32(),
            local_memory_size(tv)
        );
        return;
    }
    let reg = reg(insn);
    let src = tv.x(reg.index() as u32);
    let (bit_size, _) = get_size(insn);

    match bit_size {
        8 => {
            let bit = byte_offset(tv, offset);
            let old = tv.ir.load_local(word_offset.clone());
            let value = tv.ir.bit_field_insert(old, src, bit, Value::ImmU32(8));
            tv.ir.write_local(word_offset, value);
        }
        16 => {
            let bit = short_offset(tv, offset);
            let old = tv.ir.load_local(word_offset.clone());
            let value = tv.ir.bit_field_insert(old, src, bit, Value::ImmU32(16));
            tv.ir.write_local(word_offset, value);
        }
        32 | 64 | 128 => {
            let words = bit_size / 32;
            if !reg.is_aligned(words as usize) {
                panic!("Unaligned STL source register {reg}");
            }
            tv.ir.write_local(word_offset.clone(), src);
            for index in 1..words {
                let address = tv.ir.iadd_32(word_offset.clone(), Value::ImmU32(index));
                let value = tv.x((reg + index as i32).index() as u32);
                tv.ir.write_local(address, value);
            }
        }
        _ => unreachable!("validated local memory size"),
    }
}

pub fn sts(tv: &mut TranslatorVisitor<'_>, insn: u64) {
    let offset = offset(tv, insn);
    let reg = reg(insn);
    let (bit_size, _) = get_size(insn);

    match bit_size {
        8 => {
            let value = tv.x(reg.index() as u32);
            tv.ir.write_shared_u8(offset, value);
        }
        16 => {
            let value = tv.x(reg.index() as u32);
            tv.ir.write_shared_u16(offset, value);
        }
        32 => {
            let value = tv.x(reg.index() as u32);
            tv.ir.write_shared_u32(offset, value);
        }
        64 => {
            if !reg.is_aligned(2) {
                panic!("Unaligned STS source register {reg}");
            }
            let lo = tv.x(reg.index() as u32);
            let hi = tv.x((reg + 1).index() as u32);
            let value = tv.ir.composite_construct_u32x2(lo, hi);
            tv.ir.write_shared_u64(offset, value);
        }
        128 => {
            if !reg.is_aligned(2) {
                panic!("Unaligned STS source register {reg}");
            }
            let x = tv.x(reg.index() as u32);
            let y = tv.x((reg + 1).index() as u32);
            let z = tv.x((reg + 2).index() as u32);
            let w = tv.x((reg + 3).index() as u32);
            let value = tv.ir.composite_construct_u32x4(x, y, z, w);
            tv.ir.write_shared_u128(offset, value);
        }
        _ => unreachable!("validated shared memory size"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::basic_block::Block;
    use crate::ir::opcodes::Opcode;
    use crate::ir::program::Program;
    use crate::ir::types::ShaderStage;

    fn encode_shared(reg: u32, offset_reg: u32, size: u32) -> u64 {
        u64::from(reg) | (u64::from(offset_reg) << 8) | (u64::from(size) << 48)
    }

    #[test]
    fn sts_wide_from_rz_is_aligned_and_keeps_all_sources_zero() {
        for (size, expected_write) in [(5, Opcode::WriteSharedU64), (6, Opcode::WriteSharedU128)] {
            let mut program = Program::new(ShaderStage::Compute);
            program.blocks.push(Block::new());
            let mut tv = TranslatorVisitor::new(&mut program, 0);

            sts(&mut tv, encode_shared(255, 255, size));

            let opcodes = program.blocks[0]
                .iter()
                .map(|inst| inst.opcode)
                .collect::<Vec<_>>();
            assert!(opcodes.contains(&expected_write));
            assert!(!opcodes.contains(&Opcode::GetRegister));
        }
    }
}
