use crate::frontend::a32::decoder::DecodedArm;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

use super::asimd::to_vector_reg;

fn decode_two_reg_shift(inst: &DecodedArm) -> (bool, bool, u32, u32, bool, bool, bool, u32) {
    let u = ((inst.raw >> 24) & 1) != 0;
    let d = ((inst.raw >> 22) & 1) != 0;
    let imm6 = (inst.raw >> 16) & 0x3F;
    let vd = (inst.raw >> 12) & 0xF;
    let l = ((inst.raw >> 7) & 1) != 0;
    let q = ((inst.raw >> 6) & 1) != 0;
    let m = ((inst.raw >> 5) & 1) != 0;
    let vm = inst.raw & 0xF;
    (u, d, imm6, vd, l, q, m, vm)
}

fn highest_set_bit(v: u32) -> u32 {
    if v == 0 {
        0
    } else {
        31 - v.leading_zeros()
    }
}

fn element_size_and_shift_amount(right_shift: bool, l: bool, imm6: u32) -> (usize, usize) {
    if right_shift {
        if l {
            return (64, 64 - imm6 as usize);
        }
        let esize = 8usize << highest_set_bit(imm6 >> 3);
        let shift_amount = (esize * 2) - imm6 as usize;
        (esize, shift_amount)
    } else {
        if l {
            return (64, imm6 as usize);
        }
        let esize = 8usize << highest_set_bit(imm6 >> 3);
        let shift_amount = imm6 as usize - esize;
        (esize, shift_amount)
    }
}

pub fn arm_asimd_shr(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (u, d, imm6, vd, l, q, m, vm) = decode_two_reg_shift(inst);
    if !l && (imm6 >> 3) == 0 {
        return false;
    }
    if q && ((vd & 1) != 0 || (vm & 1) != 0) {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    let (esize, shift_amount) = element_size_and_shift_amount(true, l, imm6);
    let Some(d_reg) = to_vector_reg(q, d, vd) else {
        return false;
    };
    let Some(m_reg) = to_vector_reg(q, m, vm) else {
        return false;
    };
    let reg_m = ir.get_vector(m_reg);
    let result = if u {
        ir.ir()
            .vector_logical_shift_right(esize, reg_m, shift_amount as u8)
    } else {
        ir.ir()
            .vector_arithmetic_shift_right(esize, reg_m, shift_amount as u8)
    };
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_sra(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (u, d, imm6, vd, l, q, m, vm) = decode_two_reg_shift(inst);
    if !l && (imm6 >> 3) == 0 {
        return false;
    }
    if q && ((vd & 1) != 0 || (vm & 1) != 0) {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    let (esize, shift_amount) = element_size_and_shift_amount(true, l, imm6);
    let Some(d_reg) = to_vector_reg(q, d, vd) else {
        return false;
    };
    let Some(m_reg) = to_vector_reg(q, m, vm) else {
        return false;
    };
    let reg_m = ir.get_vector(m_reg);
    let shifted = if u {
        ir.ir()
            .vector_logical_shift_right(esize, reg_m, shift_amount as u8)
    } else {
        ir.ir()
            .vector_arithmetic_shift_right(esize, reg_m, shift_amount as u8)
    };
    let reg_d = ir.get_vector(d_reg);
    let result = ir.ir().vector_add(esize, shifted, reg_d);
    ir.set_vector(d_reg, result);
    true
}

/// Port of upstream `TranslatorVisitor::asimd_VSHRN` through
/// `ShiftRightNarrowing(..., Truncation, Unsigned)`.
pub fn arm_asimd_vshrn(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (_u, d, imm6, vd, _l, _q, m, vm) = decode_two_reg_shift(inst);
    if (imm6 >> 3) == 0 {
        return super::decode_error(ir);
    }
    if (vm & 1) != 0 {
        return super::undefined_instruction(ir);
    }

    let (esize, shift_amount) = element_size_and_shift_amount(true, false, imm6);
    let source_esize = 2 * esize;
    let Some(d_reg) = to_vector_reg(false, d, vd) else {
        return super::undefined_instruction(ir);
    };
    let Some(m_reg) = to_vector_reg(true, m, vm) else {
        return super::undefined_instruction(ir);
    };

    let reg_m = ir.get_vector(m_reg);
    let wide_result = ir
        .ir()
        .vector_logical_shift_right(source_esize, reg_m, shift_amount as u8);
    let result = ir.ir().vector_narrow(source_esize, wide_result);
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vshl_imm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (_u, d, imm6, vd, l, q, m, vm) = decode_two_reg_shift(inst);
    if !l && (imm6 >> 3) == 0 {
        return false;
    }
    if q && ((vd & 1) != 0 || (vm & 1) != 0) {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    let (esize, shift_amount) = element_size_and_shift_amount(false, l, imm6);
    let Some(d_reg) = to_vector_reg(q, d, vd) else {
        return false;
    };
    let Some(m_reg) = to_vector_reg(q, m, vm) else {
        return false;
    };
    let reg_m = ir.get_vector(m_reg);
    let result = ir
        .ir()
        .vector_logical_shift_left(esize, reg_m, shift_amount as u8);
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vsri(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (_u, d, imm6, vd, l, q, m, vm) = decode_two_reg_shift(inst);
    if !l && (imm6 >> 3) == 0 {
        return false;
    }
    if q && ((vd & 1) != 0 || (vm & 1) != 0) {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    let (esize, shift_amount) = element_size_and_shift_amount(true, l, imm6);
    let mask = if shift_amount == esize {
        0
    } else {
        ((1u64 << esize) - 1) >> shift_amount
    };
    let Some(d_reg) = to_vector_reg(q, d, vd) else {
        return false;
    };
    let Some(m_reg) = to_vector_reg(q, m, vm) else {
        return false;
    };
    let reg_m = ir.get_vector(m_reg);
    let reg_d = ir.get_vector(d_reg);
    let shifted = ir
        .ir()
        .vector_logical_shift_right(esize, reg_m, shift_amount as u8);
    let mask_vec = ir.ir().vector_broadcast(esize, Value::ImmU64(mask));
    let masked_d = ir.ir().vector_and_not(reg_d, mask_vec);
    let result = ir.ir().vector_or(masked_d, shifted);
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vsli(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (_u, d, imm6, vd, l, q, m, vm) = decode_two_reg_shift(inst);
    if !l && (imm6 >> 3) == 0 {
        return false;
    }
    if q && ((vd & 1) != 0 || (vm & 1) != 0) {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    let (esize, shift_amount) = element_size_and_shift_amount(false, l, imm6);
    let mask = ((1u64 << esize) - 1) << shift_amount;
    let Some(d_reg) = to_vector_reg(q, d, vd) else {
        return false;
    };
    let Some(m_reg) = to_vector_reg(q, m, vm) else {
        return false;
    };
    let reg_m = ir.get_vector(m_reg);
    let reg_d = ir.get_vector(d_reg);
    let shifted = ir
        .ir()
        .vector_logical_shift_left(esize, reg_m, shift_amount as u8);
    let mask_vec = ir.ir().vector_broadcast(esize, Value::ImmU64(mask));
    let masked_d = ir.ir().vector_and_not(reg_d, mask_vec);
    let result = ir.ir().vector_or(masked_d, shifted);
    ir.set_vector(d_reg, result);
    true
}

pub fn arm_asimd_vqshl_imm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let (u, d, imm6, vd, l, q, m, vm) = decode_two_reg_shift(inst);
    let op = ((inst.raw >> 8) & 1) != 0;
    if !l && (imm6 >> 3) == 0 {
        return false;
    }
    if q && ((vd & 1) != 0 || (vm & 1) != 0) {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    if !u && !op {
        ir.exception_raised(crate::frontend::a32::types::Exception::UndefinedInstruction);
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        });
        return false;
    }
    let (esize, shift_amount) = element_size_and_shift_amount(false, l, imm6);
    let Some(d_reg) = to_vector_reg(q, d, vd) else {
        return false;
    };
    let Some(m_reg) = to_vector_reg(q, m, vm) else {
        return false;
    };
    let reg_m = ir.get_vector(m_reg);
    let shift_vec = ir
        .ir()
        .vector_broadcast(esize, Value::ImmU64(shift_amount as u64));
    let result = if u {
        if op {
            ir.ir()
                .vector_unsigned_saturated_shift_left(esize, reg_m, shift_vec)
        } else {
            ir.ir()
                .vector_signed_saturated_shift_left_unsigned(esize, reg_m, shift_amount as u8)
        }
    } else if op {
        ir.ir()
            .vector_signed_saturated_shift_left(esize, reg_m, shift_vec)
    } else {
        unreachable!()
    };
    ir.set_vector(d_reg, result);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder::ArmInstId;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;

    fn translate_with(
        inst: DecodedArm,
        f: fn(&mut A32IREmitter, &DecodedArm) -> bool,
    ) -> Vec<Opcode> {
        let loc = A32LocationDescriptor::at(0x2000);
        let mut block = Block::new(loc.to_location());
        let ok = {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            f(&mut ir, &inst)
        };
        assert!(ok);
        block.instructions.iter().map(|inst| inst.opcode).collect()
    }

    #[test]
    fn vsri_emits_shift_and_andnot() {
        let opcodes = translate_with(
            DecodedArm {
                raw: 0xF389_0410,
                id: ArmInstId::AsimdVsri,
            },
            arm_asimd_vsri,
        );
        assert!(opcodes.contains(&Opcode::VectorLogicalShiftRight8));
        assert!(opcodes.contains(&Opcode::VectorAndNot));
        assert!(opcodes.contains(&Opcode::VectorOr));
    }

    #[test]
    fn vsli_emits_shift_and_andnot() {
        let opcodes = translate_with(
            DecodedArm {
                raw: 0xF389_0510,
                id: ArmInstId::AsimdVsli,
            },
            arm_asimd_vsli,
        );
        assert!(opcodes.contains(&Opcode::VectorLogicalShiftLeft8));
        assert!(opcodes.contains(&Opcode::VectorAndNot));
        assert!(opcodes.contains(&Opcode::VectorOr));
    }

    #[test]
    fn vqshl_imm_emits_unsigned_saturated_shift_left() {
        let opcodes = translate_with(
            DecodedArm {
                raw: 0xF389_0710,
                id: ArmInstId::AsimdVqshlImm,
            },
            arm_asimd_vqshl_imm,
        );
        assert!(opcodes.contains(&Opcode::VectorUnsignedSaturatedShiftLeft8));
        assert_eq!(opcodes.last(), Some(&Opcode::A32SetVector));
    }

    #[test]
    fn observed_vshrn_i64_emits_shift_then_narrow() {
        let opcodes = translate_with(
            DecodedArm {
                raw: 0xF2E0_3830,
                id: ArmInstId::AsimdVshrn,
            },
            arm_asimd_vshrn,
        );
        assert!(opcodes.contains(&Opcode::VectorLogicalShiftRight64));
        assert!(opcodes.contains(&Opcode::VectorNarrow64));
        assert_eq!(opcodes.last(), Some(&Opcode::A32SetVector));
    }
}
