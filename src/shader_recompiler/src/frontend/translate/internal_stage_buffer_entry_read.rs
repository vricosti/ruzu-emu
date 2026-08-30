// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's
//! `frontend/maxwell/translate/impl/internal_stage_buffer_entry_read.cpp`.

use super::{bit, field, TranslatorVisitor};
use crate::ir::value::{Attribute, Patch, Reg, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Default,
    Patch,
    Prim,
    Attr,
}

impl Mode {
    fn from_bits(bits: u32) -> Self {
        match bits {
            0 => Self::Default,
            1 => Self::Patch,
            2 => Self::Prim,
            3 => Self::Attr,
            _ => unreachable!(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SizeRead {
    U8,
    U16,
    U32,
    F32,
}

impl SizeRead {
    fn from_bits(bits: u32) -> Self {
        match bits {
            0 => Self::U8,
            1 => Self::U16,
            2 => Self::U32,
            3 => Self::F32,
            _ => panic!("Invalid ISBERD size {bits}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shift {
    Default,
    U16,
    B32,
}

impl Shift {
    fn from_bits(bits: u32) -> Self {
        match bits {
            0 => Self::Default,
            1 => Self::U16,
            2 => Self::B32,
            _ => panic!("Invalid ISBERD shift {bits}"),
        }
    }
}

fn scale_index(tv: &mut TranslatorVisitor<'_>, index: Value, shift: Shift) -> Value {
    match shift {
        Shift::Default => index,
        Shift::U16 => tv.ir.shift_left_logical_32(index, Value::ImmU32(1)),
        Shift::B32 => tv.ir.shift_left_logical_32(index, Value::ImmU32(2)),
    }
}

fn skew_bytes(tv: &mut TranslatorVisitor<'_>, size_read: SizeRead) -> Value {
    let lane = tv.ir.lane_id();
    match size_read {
        SizeRead::U8 => lane,
        SizeRead::U16 => tv.ir.shift_left_logical_32(lane, Value::ImmU32(1)),
        SizeRead::U32 | SizeRead::F32 => tv.ir.shift_left_logical_32(lane, Value::ImmU32(2)),
    }
}

fn immediate_index(index: Value, mode: Mode) -> u32 {
    match index {
        Value::ImmU32(value) => value,
        _ => panic!("ISBERD {mode:?} index is not immediate"),
    }
}

impl<'a> TranslatorVisitor<'a> {
    /// Port of upstream `TranslatorVisitor::ISBERD(u64)`.
    pub fn translate_isberd(&mut self, insn: u64) {
        log::debug!("ISBERD called with insn={insn:#x}");

        let dst = self.dst_reg(insn);
        let src = self.src_a_reg(insn);
        let src_reg_num = field(insn, 8, 8);
        let immediate = field(insn, 24, 8);
        let skew = bit(insn, 31);
        let global = bit(insn, 32);
        let mode = Mode::from_bits(field(insn, 33, 2));
        let size_read = SizeRead::from_bits(field(insn, 36, 4));
        let shift = Shift::from_bits(field(insn, 47, 2));

        let mut index = if src_reg_num == Reg::RZ.0 as u32 {
            Value::ImmU32(immediate)
        } else {
            let source = self.x(src);
            let scaled_index = scale_index(self, source, shift);
            self.ir.iadd_32(scaled_index, Value::ImmU32(immediate))
        };

        if global {
            if skew {
                let skew = skew_bytes(self, size_read);
                index = self.ir.iadd_32(index, skew);
            }

            let index64 = self.ir.uconvert_u64_from_u32(index);
            let loaded = match size_read {
                SizeRead::U8 => self.ir.load_global_u8(index64),
                SizeRead::U16 => self.ir.load_global_u16(index64),
                SizeRead::U32 | SizeRead::F32 => self.ir.load_global_32(index64),
            };
            self.set_x(dst, loaded);
            return;
        }

        if mode != Mode::Default {
            if skew {
                let skew = skew_bytes(self, SizeRead::U32);
                index = self.ir.iadd_32(index, skew);
            }

            let float_index = match mode {
                Mode::Patch => self.ir.get_patch(Patch(immediate_index(index, mode))),
                Mode::Prim => self
                    .ir
                    .get_attribute(Attribute(immediate_index(index, mode)), Value::ImmU32(0)),
                Mode::Attr => self.ir.get_attribute_indexed(index, Value::ImmU32(0)),
                Mode::Default => unreachable!(),
            };
            let value = self.ir.bit_cast_u32_f32(float_index);
            self.set_x(dst, value);
            return;
        }

        if skew {
            let source = self.x(src);
            let lane = self.ir.lane_id();
            let value = self.ir.iadd_32(source, lane);
            self.set_x(dst, value);
            return;
        }

        let value = self.x(src);
        self.set_x(dst, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::opcodes::Opcode;
    use crate::ir::program::Program;
    use crate::ir::types::ShaderStage;

    fn translate(insn: u64) -> Vec<Opcode> {
        let mut program = Program::new(ShaderStage::Geometry);
        let block = program.add_block();
        {
            let mut visitor = TranslatorVisitor::new(&mut program, block);
            visitor.translate_isberd(insn);
        }
        program.blocks[block as usize]
            .iter()
            .map(|inst| inst.opcode)
            .collect()
    }

    #[test]
    fn fallback_copy_matches_upstream() {
        let insn = 1 | (2 << 8) | (2 << 36);
        assert_eq!(
            translate(insn),
            vec![
                Opcode::GetRegister,
                Opcode::IAdd32,
                Opcode::GetRegister,
                Opcode::SetRegister,
            ]
        );
    }

    #[test]
    fn global_u16_skew_uses_lane_scaled_address_and_zero_extension() {
        let insn = 1 | ((Reg::RZ.0 as u64) << 8) | (0x80 << 24) | (1 << 31) | (1 << 32) | (1 << 36);
        let opcodes = translate(insn);
        assert!(opcodes.contains(&Opcode::LaneId));
        assert!(opcodes.contains(&Opcode::ShiftLeftLogical32));
        assert!(opcodes.contains(&Opcode::IAdd32));
        assert!(opcodes.contains(&Opcode::ConvertU64U32));
        assert!(opcodes.contains(&Opcode::LoadGlobalU16));
        assert_eq!(opcodes.last(), Some(&Opcode::SetRegister));
    }

    #[test]
    fn indexed_attribute_mode_applies_b32_shift_and_bitcasts_result() {
        let insn = 1 | (2 << 8) | (4 << 24) | (3 << 33) | (2 << 36) | (2 << 47);
        let opcodes = translate(insn);
        assert!(opcodes.contains(&Opcode::ShiftLeftLogical32));
        assert!(opcodes.contains(&Opcode::IAdd32));
        assert!(opcodes.contains(&Opcode::GetAttributeIndexed));
        assert!(opcodes.contains(&Opcode::BitCastU32F32));
        assert_eq!(opcodes.last(), Some(&Opcode::SetRegister));
    }

    #[test]
    fn default_skew_adds_lane_to_the_unscaled_source_like_upstream() {
        let insn = 1 | (2 << 8) | (1 << 31) | (2 << 36) | (2 << 47);
        let opcodes = translate(insn);
        assert_eq!(
            opcodes,
            vec![
                Opcode::GetRegister,
                Opcode::ShiftLeftLogical32,
                Opcode::IAdd32,
                Opcode::GetRegister,
                Opcode::LaneId,
                Opcode::IAdd32,
                Opcode::SetRegister,
            ]
        );
    }
}
