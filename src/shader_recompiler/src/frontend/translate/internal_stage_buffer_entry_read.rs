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
    /// Pinned Eglantine ISBE_nx2 pseudo instruction. Native lowering remains
    /// gated until task/mesh dispatch integration can supply its stage state.
    pub fn translate_isbe_nx2(&mut self, insn: u64) {
        let info = &self.ir.program.info;
        if !(info.is_task ^ info.is_mesh) {
            std::panic::panic_any(crate::exception::NotImplementedException::new(
                "Stage buffer access outside task and mesh programs",
            ));
        }
        let reg = self.dst_reg(insn);
        let address_reg = self.src_a_reg(insn);
        let base = self.x(address_reg);
        let address = self.ir.iadd_32(base, Value::ImmU32(field(insn, 20, 24)));
        if !bit(insn, 48) {
            let value = self.ir.isbe_read(address);
            self.set_x(reg, value);
            return;
        }
        let value = self.x(reg);
        if bit(insn, 45) {
            if !bit(insn, 44) {
                std::panic::panic_any(crate::exception::NotImplementedException::new(
                    "U8 stage buffer attribute write",
                ));
            }
            self.ir.isbe_write_attribute(address, value);
        } else if bit(insn, 44) {
            self.ir.isbe_write_map_u32(address, value);
        } else {
            self.ir.isbe_write_map_u8(address, value);
        }
    }

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

    #[test]
    fn nx2_decode_dispatch_preserves_address_and_selects_each_region_operation() {
        for task in [false, true] {
            for (mode, expected) in [
                (0, Opcode::IsbeRead),
                (
                    (1 << 48) | (1 << 45) | (1 << 44),
                    Opcode::IsbeWriteAttribute,
                ),
                (1 << 48, Opcode::IsbeWriteMapU8),
                ((1 << 48) | (1 << 44), Opcode::IsbeWriteMapU32),
            ] {
                let insn =
                    0xefc0_0000_0000_0000u64 | mode | (0xabcdef << 20) | (7 << 16) | (19 << 8) | 13;
                let mut program = Program::new(ShaderStage::Compute);
                program.info.is_task = task;
                program.info.is_mesh = !task;
                let block = program.add_block();
                TranslatorVisitor::new(&mut program, block).translate_instruction(insn);
                let instructions: Vec<_> = program.blocks[block as usize].iter().collect();
                let add = instructions
                    .iter()
                    .find(|i| i.opcode == Opcode::IAdd32)
                    .unwrap();
                assert_eq!(add.arg(1), &Value::ImmU32(0xabcdef));
                let registers: Vec<_> = instructions
                    .iter()
                    .filter(|i| i.opcode == Opcode::GetRegister)
                    .map(|i| *i.arg(0))
                    .collect();
                assert_eq!(
                    registers,
                    if expected == Opcode::IsbeRead {
                        vec![Value::Reg(Reg(19))]
                    } else {
                        vec![Value::Reg(Reg(19)), Value::Reg(Reg(13))]
                    }
                );
                let address = instructions
                    .iter()
                    .find(|i| i.opcode == expected)
                    .unwrap()
                    .arg(0);
                let Value::Inst(reference) = address else {
                    panic!("address addition");
                };
                assert_eq!(
                    program.blocks[reference.block as usize]
                        .inst(reference.inst)
                        .opcode,
                    Opcode::IAdd32
                );
                assert!(!instructions.iter().any(|i| i.opcode == Opcode::LaneId));
                assert_eq!(
                    instructions.last().unwrap().opcode,
                    if expected == Opcode::IsbeRead {
                        Opcode::SetRegister
                    } else {
                        expected
                    }
                );
            }
        }
    }

    #[test]
    fn nx2_count_address_reduces_to_the_required_immediate_byte_three() {
        let mut program = Program::new(ShaderStage::Compute);
        program.info.is_mesh = true;
        let block = program.add_block();
        let insn = 0xefc0_0000_0000_0000u64
            | (1 << 48)
            | (1 << 44)
            | (3 << 20)
            | (7 << 16)
            | (255 << 8)
            | 255;
        TranslatorVisitor::new(&mut program, block).translate_instruction(insn);
        crate::ir_opt::constant_propagation_pass::constant_propagation_pass(&mut program);
        crate::ir_opt::dead_code_elimination_pass::dead_code_elimination_pass(&mut program);
        let count = program.blocks[block as usize]
            .iter()
            .find(|i| i.opcode == Opcode::IsbeWriteMapU32)
            .unwrap();
        assert_eq!(count.arg(0), &Value::ImmU32(3));
        assert_eq!(count.arg(1), &Value::ImmU32(0));
    }

    #[test]
    fn nx2_rejects_ordinary_stages_and_byte_attribute_writes() {
        for mesh in [false, true] {
            let mut program = Program::new(ShaderStage::Compute);
            program.info.is_mesh = mesh;
            let block = program.add_block();
            let insn = 0xefc0_0000_0000_0000u64 | (1 << 48) | (1 << 45) | (7 << 16);
            let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                TranslatorVisitor::new(&mut program, block).translate_instruction(insn)
            }))
            .unwrap_err();
            assert!(error.is::<crate::exception::NotImplementedException>());
        }
    }

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
