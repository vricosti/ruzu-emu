// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/frontend/maxwell/translate/impl/atomic_operations_shared_memory.cpp
//!
//! ATOMS — Atomic Operation on Shared Memory. Mirrors upstream's
//! anonymous-namespace `ApplyAtomsOp`/`AtomsOffset`/`StoreResult`
//! helpers + the public `TranslatorVisitor::ATOMS` entry.

use super::{field, sfield, TranslatorVisitor};
use crate::ir::value::{Reg, Value};

/// Maxwell AtomOp encoding (4-bit field at [55:52]). Mirrors upstream
/// `AtomOp` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtomOp {
    Add = 0,
    Min = 1,
    Max = 2,
    Inc = 3,
    Dec = 4,
    And = 5,
    Or = 6,
    Xor = 7,
    Exch = 8,
}

impl AtomOp {
    fn from_bits(v: u32) -> Self {
        match v & 0xF {
            0 => AtomOp::Add,
            1 => AtomOp::Min,
            2 => AtomOp::Max,
            3 => AtomOp::Inc,
            4 => AtomOp::Dec,
            5 => AtomOp::And,
            6 => AtomOp::Or,
            7 => AtomOp::Xor,
            8 => AtomOp::Exch,
            other => std::panic::panic_any(crate::exception::NotImplementedException::new(
                format!("Integer Atoms Operation {other}"),
            )),
        }
    }
}

/// Maxwell AtomsSize encoding (2-bit field at [29:28]). Mirrors upstream
/// `AtomsSize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtomsSize {
    U32 = 0,
    S32 = 1,
    U64 = 2,
    // C++ permits this raw enum value: ATOMS treats it as unsigned 32-bit,
    // while StoreResult's default case leaves the destination untouched.
    Reserved = 3,
}

impl AtomsSize {
    fn from_bits(v: u32) -> Self {
        match v & 0x3 {
            0 => AtomsSize::U32,
            1 => AtomsSize::S32,
            2 => AtomsSize::U64,
            _ => AtomsSize::Reserved,
        }
    }
}

/// Port of upstream `AtomsOffset(TranslatorVisitor& v, u64 insn)`.
fn atoms_offset(tv: &mut TranslatorVisitor, insn: u64) -> Value {
    let offset_reg = field(insn, 8, 8);
    if offset_reg == Reg::RZ.0 as u32 {
        let absolute = (field(insn, 30, 22) << 2) as u32;
        Value::ImmU32(absolute)
    } else {
        let relative = (sfield(insn, 30, 22) << 2) as i32;
        let reg_val = tv.x(offset_reg);
        tv.ir.iadd_32(reg_val, Value::ImmU32(relative as u32))
    }
}

/// Port of upstream `ApplyAtomsOp` — dispatches to the right IR helper
/// based on the 4-bit AtomOp encoding.
fn apply_atoms_op(
    tv: &mut TranslatorVisitor,
    offset: Value,
    op_b: Value,
    op: AtomOp,
    is_signed: bool,
) -> Value {
    match op {
        AtomOp::Add => tv.ir.shared_atomic_iadd_32(offset, op_b),
        AtomOp::Min => tv.ir.shared_atomic_imin_32(offset, op_b, is_signed),
        AtomOp::Max => tv.ir.shared_atomic_imax_32(offset, op_b, is_signed),
        AtomOp::Inc => tv.ir.shared_atomic_inc_32(offset, op_b),
        AtomOp::Dec => tv.ir.shared_atomic_dec_32(offset, op_b),
        AtomOp::And => tv.ir.shared_atomic_and_32(offset, op_b),
        AtomOp::Or => tv.ir.shared_atomic_or_32(offset, op_b),
        AtomOp::Xor => tv.ir.shared_atomic_xor_32(offset, op_b),
        AtomOp::Exch => tv.ir.shared_atomic_exchange_32(offset, op_b),
    }
}

/// Port of upstream `StoreResult` (including its no-write default case).
fn store_result(tv: &mut TranslatorVisitor, dest_reg: u32, result: Value, size: AtomsSize) {
    match size {
        AtomsSize::U32 | AtomsSize::S32 => tv.set_x(dest_reg, result),
        AtomsSize::U64 => tv.set_l(dest_reg, result),
        AtomsSize::Reserved => {}
    }
}

/// ATOMS — Atomic Operation on Shared Memory.
///
/// Port of upstream `TranslatorVisitor::ATOMS(u64 insn)`.
pub fn atoms(tv: &mut TranslatorVisitor<'_>, insn: u64) {
    let dest_reg = tv.dst_reg(insn);
    let src_reg_b = field(insn, 20, 8);
    let size = AtomsSize::from_bits(field(insn, 28, 2));
    let op = AtomOp::from_bits(field(insn, 52, 4));

    let size_64 = size == AtomsSize::U64;
    if size_64 && op != AtomOp::Exch {
        std::panic::panic_any(crate::exception::NotImplementedException::new(
            format!("64-bit Atoms Operation {}", op as u32),
        ));
    }
    let is_signed = size == AtomsSize::S32;
    let offset = atoms_offset(tv, insn);

    if size_64 {
        let op_b = tv.l(src_reg_b);
        let result = tv.ir.shared_atomic_exchange_64(offset, op_b);
        store_result(tv, dest_reg, result, size);
    } else {
        let op_b = tv.x(src_reg_b);
        let result = apply_atoms_op(tv, offset, op_b, op, is_signed);
        store_result(tv, dest_reg, result, size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::basic_block::Block;
    use crate::ir::opcodes::Opcode;
    use crate::ir::program::Program;
    use crate::ir::types::ShaderStage;

    fn translate_op(op: u32, size: AtomsSize) -> Vec<Opcode> {
        let mut program = Program::new(ShaderStage::Compute);
        program.blocks.push(Block::new());
        let mut visitor = TranslatorVisitor::new(&mut program, 0);
        let insn = u64::from(Reg::RZ.0) << 8 | u64::from(op) << 52 | (size as u64) << 28 | 2 << 20;

        atoms(&mut visitor, insn);

        program
            .block(0)
            .iter()
            .map(|instruction| instruction.opcode)
            .collect()
    }

    #[test]
    fn atoms_reserved_size_executes_atomic_without_writing_destination() {
        let opcodes = translate_op(AtomOp::Add as u32, AtomsSize::Reserved);
        assert!(opcodes.contains(&Opcode::SharedAtomicIAdd32));
        assert!(!opcodes.contains(&Opcode::SetRegister));
        let normal = translate_op(AtomOp::Add as u32, AtomsSize::U32);
        assert!(normal.contains(&Opcode::SharedAtomicIAdd32));
        assert!(normal.contains(&Opcode::SetRegister));
    }

    #[test]
    fn unsupported_atoms_operations_raise_typed_shader_exceptions() {
        for (op, size) in [(9, AtomsSize::U32), (0, AtomsSize::U64)] {
            let error = std::panic::catch_unwind(|| translate_op(op, size)).unwrap_err();
            assert!(error.is::<crate::exception::NotImplementedException>());
        }
    }

    #[test]
    fn atoms_inc_dec_match_upstream_ir_opcodes() {
        assert!(
            translate_op(AtomOp::Inc as u32, AtomsSize::U32).contains(&Opcode::SharedAtomicInc32)
        );
        assert!(
            translate_op(AtomOp::Dec as u32, AtomsSize::U32).contains(&Opcode::SharedAtomicDec32)
        );
    }

    #[test]
    fn atoms_u64_exchange_uses_register_pairs() {
        let opcodes = translate_op(AtomOp::Exch as u32, AtomsSize::U64);
        assert!(opcodes.contains(&Opcode::PackUint2x32));
        assert!(opcodes.contains(&Opcode::SharedAtomicExchange64));
        assert!(opcodes.contains(&Opcode::UnpackUint2x32));
    }
}
