// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `frontend/maxwell/translate/impl/atomic_operations_global_memory.cpp`.

use super::{bit, field, sfield, TranslatorVisitor};
use crate::ir::types::{FmzMode, FpControl, FpRounding};
use crate::ir::value::{Reg, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtomOp {
    Add,
    Min,
    Max,
    Inc,
    Dec,
    And,
    Or,
    Xor,
    Exch,
    SafeAdd,
}

impl AtomOp {
    fn from_bits(bits: u32) -> Self {
        match bits {
            0 => Self::Add,
            1 => Self::Min,
            2 => Self::Max,
            3 => Self::Inc,
            4 => Self::Dec,
            5 => Self::And,
            6 => Self::Or,
            7 => Self::Xor,
            8 => Self::Exch,
            9 => Self::SafeAdd,
            _ => panic!("Invalid AtomOp {}", bits),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtomSize {
    U32,
    S32,
    U64,
    F32,
    F16x2,
    S64,
}

impl AtomSize {
    fn from_bits(bits: u32) -> Self {
        match bits {
            0 => Self::U32,
            1 => Self::S32,
            2 => Self::U64,
            3 => Self::F32,
            4 => Self::F16x2,
            5 => Self::S64,
            _ => panic!("Invalid AtomSize {}", bits),
        }
    }
}

fn u32_to_u64(tv: &mut TranslatorVisitor<'_>, value: Value) -> Value {
    let words = tv.ir.composite_construct_u32x2(value, Value::ImmU32(0));
    tv.ir.pack_uint_2x32(words)
}

fn apply_integer_atom_op(
    tv: &mut TranslatorVisitor<'_>,
    offset: Value,
    op_b: Value,
    op: AtomOp,
    is_signed: bool,
    is_64_bit: bool,
) -> Value {
    match op {
        AtomOp::Add => tv.ir.global_atomic_iadd(offset, op_b, is_64_bit),
        AtomOp::Min => tv.ir.global_atomic_imin(offset, op_b, is_signed, is_64_bit),
        AtomOp::Max => tv.ir.global_atomic_imax(offset, op_b, is_signed, is_64_bit),
        AtomOp::Inc => tv.ir.global_atomic_inc_32(offset, op_b),
        AtomOp::Dec => tv.ir.global_atomic_dec_32(offset, op_b),
        AtomOp::And => tv.ir.global_atomic_and(offset, op_b, is_64_bit),
        AtomOp::Or => tv.ir.global_atomic_or(offset, op_b, is_64_bit),
        AtomOp::Xor => tv.ir.global_atomic_xor(offset, op_b, is_64_bit),
        AtomOp::Exch => tv.ir.global_atomic_exchange(offset, op_b, is_64_bit),
        AtomOp::SafeAdd => panic!("Integer Atom Operation SafeAdd"),
    }
}

fn apply_fp_atom_op(
    tv: &mut TranslatorVisitor<'_>,
    offset: Value,
    op_b: Value,
    op: AtomOp,
    size: AtomSize,
) -> Value {
    const F16_CONTROL: FpControl = FpControl {
        no_contraction: false,
        rounding: FpRounding::RN,
        fmz_mode: FmzMode::DontCare,
    };
    const F32_CONTROL: FpControl = FpControl {
        no_contraction: false,
        rounding: FpRounding::RN,
        fmz_mode: FmzMode::FTZ,
    };
    match op {
        AtomOp::Add if size == AtomSize::F32 => {
            tv.ir.global_atomic_f32_add(offset, op_b, F32_CONTROL)
        }
        AtomOp::Add => tv.ir.global_atomic_f16x2_add(offset, op_b, F16_CONTROL),
        AtomOp::Min => tv.ir.global_atomic_f16x2_min(offset, op_b, F16_CONTROL),
        AtomOp::Max => tv.ir.global_atomic_f16x2_max(offset, op_b, F16_CONTROL),
        _ => panic!("FP Atom Operation {:?}", op),
    }
}

fn atom_offset(tv: &mut TranslatorVisitor<'_>, insn: u64) -> Value {
    let addr_reg = field(insn, 8, 8);
    let address = if bit(insn, 48) {
        tv.l(addr_reg)
    } else {
        let addr = tv.x(addr_reg);
        u32_to_u64(tv, addr)
    };
    let addr_offset = if addr_reg == Reg::RZ.0 as u32 {
        u64::from(field(insn, 28, 20))
    } else {
        sfield(insn, 28, 20) as i64 as u64
    };
    tv.ir.iadd_64(address, Value::ImmU64(addr_offset))
}

fn atom_op_not_applicable(size: AtomSize, op: AtomOp) -> bool {
    match size {
        AtomSize::U32 | AtomSize::S32 | AtomSize::U64 => matches!(op, AtomOp::Inc | AtomOp::Dec),
        AtomSize::S64 => matches!(op, AtomOp::Add | AtomOp::Inc | AtomOp::Dec),
        AtomSize::F32 => op != AtomOp::Add,
        AtomSize::F16x2 => !matches!(op, AtomOp::Add | AtomOp::Min | AtomOp::Max),
    }
}

fn load_global(tv: &mut TranslatorVisitor<'_>, offset: Value, size: AtomSize) -> Value {
    match size {
        AtomSize::U32 | AtomSize::S32 | AtomSize::F32 | AtomSize::F16x2 => {
            tv.ir.load_global_32(offset)
        }
        AtomSize::U64 | AtomSize::S64 => {
            let words = tv.ir.load_global_64(offset);
            tv.ir.pack_uint_2x32(words)
        }
    }
}

fn store_result(tv: &mut TranslatorVisitor<'_>, dest_reg: u32, result: Value, size: AtomSize) {
    match size {
        AtomSize::U32 | AtomSize::S32 | AtomSize::F16x2 => tv.set_x(dest_reg, result),
        AtomSize::U64 | AtomSize::S64 => tv.set_l(dest_reg, result),
        AtomSize::F32 => tv.set_f(dest_reg, result),
    }
}

fn apply_atom_op(
    tv: &mut TranslatorVisitor<'_>,
    operand_reg: u32,
    offset: Value,
    size: AtomSize,
    op: AtomOp,
) -> Value {
    match size {
        AtomSize::U32 | AtomSize::S32 => {
            let op_b = tv.x(operand_reg);
            apply_integer_atom_op(tv, offset, op_b, op, size == AtomSize::S32, false)
        }
        AtomSize::U64 | AtomSize::S64 => {
            let op_b = tv.l(operand_reg);
            apply_integer_atom_op(tv, offset, op_b, op, size == AtomSize::S64, true)
        }
        AtomSize::F32 => {
            let op_b = tv.f(operand_reg);
            apply_fp_atom_op(tv, offset, op_b, op, size)
        }
        AtomSize::F16x2 => {
            let packed = tv.x(operand_reg);
            let op_b = tv.ir.unpack_float_2x16(packed);
            apply_fp_atom_op(tv, offset, op_b, op, size)
        }
    }
}

fn global_atomic(
    tv: &mut TranslatorVisitor<'_>,
    dest_reg: u32,
    operand_reg: u32,
    offset: Value,
    size: AtomSize,
    op: AtomOp,
    write_dest: bool,
) {
    let result = if atom_op_not_applicable(size, op) {
        load_global(tv, offset, size)
    } else {
        apply_atom_op(tv, operand_reg, offset, size, op)
    };
    if write_dest {
        store_result(tv, dest_reg, result, size);
    }
}

/// Port of upstream `TranslatorVisitor::ATOM`.
pub fn atom(tv: &mut TranslatorVisitor<'_>, insn: u64) {
    let dest_reg = field(insn, 0, 8);
    let operand_reg = field(insn, 20, 8);
    let size = AtomSize::from_bits(field(insn, 49, 3));
    let op = AtomOp::from_bits(field(insn, 52, 4));
    let offset = atom_offset(tv, insn);
    global_atomic(tv, dest_reg, operand_reg, offset, size, op, true);
}

/// Port of upstream `TranslatorVisitor::RED`.
pub fn red(tv: &mut TranslatorVisitor<'_>, insn: u64) {
    let operand_reg = field(insn, 0, 8);
    let size = AtomSize::from_bits(field(insn, 20, 3));
    let op = AtomOp::from_bits(field(insn, 23, 3));
    let offset = atom_offset(tv, insn);
    global_atomic(tv, Reg::RZ.0 as u32, operand_reg, offset, size, op, true);
}

impl TranslatorVisitor<'_> {
    /// Ruzu extension beyond Eden's not_implemented.cpp. Encoding follows
    /// Mesa NAK sm50.rs, OpAtom::legalize/encode: comparator then replacement
    /// in a packed register tuple, bit 49 selects width, bits 50..52 layout.
    /// Only packed layout is known to work on Maxwell hardware.
    pub fn translate_atom_cas(&mut self, insn: u64) {
        if field(insn, 50, 2) != 0 {
            std::panic::panic_any(crate::exception::NotImplementedException::new(
                "ATOM.CAS non-packed operand layout",
            ));
        }
        let wide = bit(insn, 49);
        let src = field(insn, 20, 8);
        let dst = field(insn, 0, 8);
        let rz = u32::from(Reg::RZ.0);
        if wide && ((src != rz && (src & 1 != 0 || src > 252)) || (dst != rz && dst & 1 != 0)) {
            std::panic::panic_any(crate::exception::InvalidArgument::new(
                "ATOM.CAS invalid 64-bit register tuple",
            ));
        }
        let offset = if bit(insn, 48) && field(insn, 8, 8) == rz {
            Value::ImmU64(u64::from(field(insn, 28, 20)))
        } else {
            atom_offset(self, insn)
        };
        // Read all inputs before writing a destination that may alias them.
        let (compare, replacement) = if src == rz {
            let zero = if wide {
                Value::ImmU64(0)
            } else {
                Value::ImmU32(0)
            };
            (zero, zero)
        } else if wide {
            (self.l(src), self.l(src + 2))
        } else {
            (self.x(src), self.x(src + 1))
        };
        let result = self
            .ir
            .global_atomic_compare_exchange(offset, compare, replacement, wide);
        if dst != rz {
            if wide {
                self.set_l(dst, result);
            } else {
                self.set_x(dst, result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::basic_block::Block;
    use crate::ir::opcodes::Opcode;
    use crate::ir::program::Program;
    use crate::ir::types::ShaderStage;

    #[test]
    fn cas_decodes_packed_registers_for_both_widths() {
        for wide in [false, true] {
            let mut program = Program::new(ShaderStage::Compute);
            program.blocks.push(Block::new());
            let insn = 0xeef0000000000000 | ((wide as u64) << 49) | (4 << 20) | (8 << 8) | 4;
            TranslatorVisitor::new(&mut program, 0).translate_atom_cas(insn);
            let opcode = if wide {
                Opcode::GlobalAtomicCompareExchange64
            } else {
                Opcode::GlobalAtomicCompareExchange32
            };
            let instructions: Vec<_> = program.block(0).iter().collect();
            let cas_index = instructions
                .iter()
                .position(|i| i.opcode == opcode)
                .unwrap();
            assert_eq!(instructions[cas_index].args.len(), 3);
            assert!(instructions[..cas_index]
                .iter()
                .all(|i| i.opcode != Opcode::SetRegister));
            let reads: Vec<_> = instructions[..cas_index]
                .iter()
                .filter(|i| i.opcode == Opcode::GetRegister)
                .map(|i| i.args[0])
                .collect();
            let registers = if wide {
                vec![8, 4, 5, 6, 7]
            } else {
                vec![8, 4, 5]
            };
            assert_eq!(
                reads,
                registers
                    .into_iter()
                    .map(|r| Value::Reg(Reg(r)))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn cas_zero_destination_still_emits_memory_side_effect() {
        for wide in [false, true] {
            let mut program = Program::new(ShaderStage::Compute);
            program.blocks.push(Block::new());
            let insn = 0xeef0000000000000 | ((wide as u64) << 49) | (255 << 20) | (255 << 8) | 255;
            TranslatorVisitor::new(&mut program, 0).translate_atom_cas(insn);
            let instructions: Vec<_> = program.block(0).iter().collect();
            assert!(instructions
                .iter()
                .any(|i| i.opcode.may_have_side_effects()));
            assert!(!instructions.iter().any(|i| i.opcode == Opcode::SetRegister));
        }
    }

    #[test]
    fn cas_reserved_layout_is_a_typed_shader_error() {
        let mut program = Program::new(ShaderStage::Compute);
        program.blocks.push(Block::new());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            TranslatorVisitor::new(&mut program, 0).translate_atom_cas(0xeefc000000000000);
        }));
        assert!(result
            .unwrap_err()
            .is::<crate::exception::NotImplementedException>());
    }

    fn translate_atom(size: AtomSize, op: AtomOp) -> Vec<Opcode> {
        let mut program = Program::new(ShaderStage::Compute);
        program.blocks.push(Block::new());
        let mut visitor = TranslatorVisitor::new(&mut program, 0);
        let insn = 2u64 << 20 | (size as u64) << 49 | (op as u64) << 52;

        atom(&mut visitor, insn);

        program
            .block(0)
            .iter()
            .map(|instruction| instruction.opcode)
            .collect()
    }

    #[test]
    fn atom_integer_sizes_select_upstream_opcodes() {
        assert!(translate_atom(AtomSize::U32, AtomOp::Add).contains(&Opcode::GlobalAtomicIAdd32));
        assert!(translate_atom(AtomSize::S64, AtomOp::Min).contains(&Opcode::GlobalAtomicSMin64));
        assert!(translate_atom(AtomSize::U32, AtomOp::Inc).contains(&Opcode::LoadGlobal32));
        assert!(translate_atom(AtomSize::S64, AtomOp::And).contains(&Opcode::GlobalAtomicAnd64));
    }

    #[test]
    fn atom_float_sizes_select_upstream_opcodes() {
        assert!(translate_atom(AtomSize::F32, AtomOp::Add).contains(&Opcode::GlobalAtomicAddF32));
        assert!(
            translate_atom(AtomSize::F16x2, AtomOp::Max).contains(&Opcode::GlobalAtomicMaxF16x2)
        );
    }

    #[test]
    fn unsupported_operation_degrades_to_load_like_upstream() {
        let opcodes = translate_atom(AtomSize::F32, AtomOp::Min);
        assert!(opcodes.contains(&Opcode::LoadGlobal32));
        assert!(!opcodes.contains(&Opcode::GlobalAtomicMinF16x2));
    }
}
