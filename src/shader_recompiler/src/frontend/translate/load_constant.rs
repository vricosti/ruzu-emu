// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/frontend/maxwell/translate/impl/load_constant.cpp
//!
//! Handles the LDC instruction for loading from constant buffers with
//! various addressing modes.

use super::{field, sfield, TranslatorVisitor};
use crate::ir::value::Value;

/// LDC addressing mode.
/// Matches upstream `enum class Mode : u64` in `load_constant.h`.
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LdcMode {
    Default = 0,
    Il = 1,
    Is = 2,
    Isl = 3,
}

impl LdcMode {
    fn from_bits(v: u64) -> Self {
        match v {
            0 => LdcMode::Default,
            1 => LdcMode::Il,
            2 => LdcMode::Is,
            3 => LdcMode::Isl,
            _ => LdcMode::Default,
        }
    }
}

/// LDC operand size.
/// Matches upstream `enum class Size : u64` in `load_constant.h`.
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LdcSize {
    U8 = 0,
    S8 = 1,
    U16 = 2,
    S16 = 3,
    B32 = 4,
    B64 = 5,
}

impl LdcSize {
    fn from_bits(v: u64) -> Self {
        match v {
            0 => LdcSize::U8,
            1 => LdcSize::S8,
            2 => LdcSize::U16,
            3 => LdcSize::S16,
            4 => LdcSize::B32,
            5 => LdcSize::B64,
            _ => LdcSize::B32,
        }
    }
}

/// LDC — Load from constant buffer.
///
/// Matches upstream `TranslatorVisitor::LDC(u64)`.
///
/// Encoding (from `load_constant.h`):
/// - bits [7:0]:   dest_reg
/// - bits [15:8]:  src_reg
/// - bits [35:20]: offset (s16, shifted by 2 to form byte offset)
/// - bits [40:36]: index (cbuf index)
/// - bits [45:44]: mode
/// - bits [50:48]: size
pub fn ldc(tv: &mut TranslatorVisitor, insn: u64) {
    let dest_reg = field(insn, 0, 8);
    let src_reg = field(insn, 8, 8);
    let offset_raw = sfield(insn, 20, 16); // 16-bit signed offset
    let index = field(insn, 36, 5);
    let mode = LdcMode::from_bits(field(insn, 44, 2) as u64);
    let size = LdcSize::from_bits(field(insn, 48, 3) as u64);

    let imm_index = Value::ImmU32(index);
    let reg_val = tv.x(src_reg);
    let imm_offset = Value::ImmU32(offset_raw as u32);

    // Compute (cb_index, byte_offset) pair based on mode.
    // Matches upstream `Slot()` function in load_constant.cpp.
    let (cb_index, byte_offset) = match mode {
        LdcMode::Default => {
            let offset = tv.ir.iadd_32(reg_val, imm_offset);
            (imm_index, offset)
        }
        LdcMode::Is => {
            // IS mode: flat address space segmentation
            // address = reg + imm_offset
            // cb_index = address[31:16] + imm_index
            // cb_offset = address[15:0]
            let address = tv.ir.iadd_32(reg_val, imm_offset);
            let seg_index =
                tv.ir
                    .bit_field_u_extract(address, Value::ImmU32(16), Value::ImmU32(16));
            let seg_offset =
                tv.ir
                    .bit_field_u_extract(address, Value::ImmU32(0), Value::ImmU32(16));
            let final_index = tv.ir.iadd_32(seg_index, imm_index);
            (final_index, seg_offset)
        }
        // Upstream `LDC` throws NotImplementedException for IL / ISL
        // modes (indirect-load with size encoding) — they need NVN-style
        // descriptor array support that isn't wired.
        LdcMode::Il | LdcMode::Isl => {
            panic!("LDC: mode {:?} not implemented", mode);
        }
    };

    let result = match size {
        LdcSize::U8 => tv.ir.get_cbuf_u8(cb_index, byte_offset),
        LdcSize::S8 => tv.ir.get_cbuf_s8(cb_index, byte_offset),
        LdcSize::U16 => tv.ir.get_cbuf_u16(cb_index, byte_offset),
        LdcSize::S16 => tv.ir.get_cbuf_s16(cb_index, byte_offset),
        LdcSize::B32 => tv.ir.get_cbuf_u32(cb_index, byte_offset),
        LdcSize::B64 => {
            if dest_reg & 1 != 0 {
                panic!("LDC.B64: unaligned destination register {}", dest_reg);
            }
            let vector = tv.ir.get_cbuf_u32x2(cb_index, byte_offset);
            let lo = tv
                .ir
                .composite_extract_u32x2(vector.clone(), Value::ImmU32(0));
            let hi = tv.ir.composite_extract_u32x2(vector, Value::ImmU32(1));
            tv.set_x(dest_reg, lo);
            tv.set_x(dest_reg + 1, hi);
            return;
        }
    };

    tv.set_x(dest_reg, result);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{basic_block::Block, opcodes::Opcode, program::Program, types::ShaderStage};

    #[test]
    fn ldc_b64_uses_one_vector_cbuf_load_like_upstream() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        let insn =
            2u64 | (255u64 << 8) | (0x20u64 << 20) | (3u64 << 36) | (LdcSize::B64 as u64) << 48;

        ldc(&mut tv, insn);

        let opcodes: Vec<_> = tv.ir.program.blocks[0]
            .iter()
            .map(|inst| inst.opcode)
            .collect();
        assert_eq!(
            opcodes
                .iter()
                .filter(|opcode| **opcode == Opcode::GetCbufU32x2)
                .count(),
            1
        );
        assert_eq!(
            opcodes
                .iter()
                .filter(|opcode| **opcode == Opcode::CompositeExtractU32x2)
                .count(),
            2
        );
        assert!(!opcodes.contains(&Opcode::GetCbufU32));
    }
}
