use crate::frontend::a32::decoder::DecodedArm;
use crate::frontend::a32::types::Exception;
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

fn signed_halfword_operand(ir: &mut A32IREmitter, reg: Reg, top: bool) -> Value {
    let value = ir.get_register(reg);
    if top {
        ir.ir()
            .arithmetic_shift_right_32(value, Value::ImmU8(16), Value::ImmU1(false))
    } else {
        let half = ir.ir().least_significant_half(value);
        ir.ir().sign_extend_half_to_word(half)
    }
}

fn unpredictable_instruction(ir: &mut A32IREmitter) -> bool {
    ir.exception_raised(Exception::UnpredictableInstruction);
    ir.set_term(Terminal::CheckHalt {
        else_: Box::new(Terminal::ReturnToDispatch),
    });
    false
}

/// ARM MUL.
pub fn arm_mul(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = Reg::from_u32((inst.raw >> 16) & 0xF);
    let rm = inst.rm();
    let rs = Reg::from_u32((inst.raw >> 8) & 0xF);
    let s = inst.s_flag();

    let rm_val = ir.get_register(rm);
    let rs_val = ir.get_register(rs);
    let result = ir.ir().mul_32(rm_val, rs_val);

    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nz(nz);
    }

    ir.set_register(rd, result);
    true
}

/// ARM MLA - multiply accumulate.
pub fn arm_mla(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = Reg::from_u32((inst.raw >> 16) & 0xF);
    let rn = Reg::from_u32((inst.raw >> 12) & 0xF);
    let rm = inst.rm();
    let rs = Reg::from_u32((inst.raw >> 8) & 0xF);
    let s = inst.s_flag();

    let rm_val = ir.get_register(rm);
    let rs_val = ir.get_register(rs);
    let rn_val = ir.get_register(rn);
    let product = ir.ir().mul_32(rm_val, rs_val);
    let result = ir.ir().add_32(product, rn_val, Value::ImmU1(false));

    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nz(nz);
    }

    ir.set_register(rd, result);
    true
}

/// ARM MLS - multiply and subtract.
pub fn arm_mls(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = Reg::from_u32((inst.raw >> 16) & 0xF);
    let rn = Reg::from_u32((inst.raw >> 12) & 0xF);
    let rm = inst.rm();
    let rs = Reg::from_u32((inst.raw >> 8) & 0xF);

    let rm_val = ir.get_register(rm);
    let rs_val = ir.get_register(rs);
    let rn_val = ir.get_register(rn);
    let product = ir.ir().mul_32(rm_val, rs_val);
    let result = ir.ir().sub_32(rn_val, product, Value::ImmU1(true));

    ir.set_register(rd, result);
    true
}

/// ARM UMULL - unsigned multiply long.
pub fn arm_umull(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd_hi = Reg::from_u32((inst.raw >> 16) & 0xF);
    let rd_lo = Reg::from_u32((inst.raw >> 12) & 0xF);
    let rm = inst.rm();
    let rs = Reg::from_u32((inst.raw >> 8) & 0xF);
    let s = inst.s_flag();

    let rm_val = ir.get_register(rm);
    let rs_val = ir.get_register(rs);

    // Zero-extend to 64-bit and multiply
    let rm64 = ir.ir().zero_extend_word_to_long(rm_val);
    let rs64 = ir.ir().zero_extend_word_to_long(rs_val);
    let result = ir.ir().mul_64(rm64, rs64);

    let lo = ir.ir().least_significant_word(result);
    let hi = ir.ir().most_significant_word(result).result;

    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nz(nz);
    }

    ir.set_register(rd_lo, lo);
    ir.set_register(rd_hi, hi);
    true
}

/// ARM UMLAL - unsigned multiply accumulate long.
pub fn arm_umlal(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd_hi = Reg::from_u32((inst.raw >> 16) & 0xF);
    let rd_lo = Reg::from_u32((inst.raw >> 12) & 0xF);
    let rm = inst.rm();
    let rs = Reg::from_u32((inst.raw >> 8) & 0xF);
    let s = inst.s_flag();

    let rm_val = ir.get_register(rm);
    let rs_val = ir.get_register(rs);
    let rdhi_val = ir.get_register(rd_hi);
    let rdlo_val = ir.get_register(rd_lo);

    let rm64 = ir.ir().zero_extend_word_to_long(rm_val);
    let rs64 = ir.ir().zero_extend_word_to_long(rs_val);
    let product = ir.ir().mul_64(rm64, rs64);

    let accum = ir.ir().pack_2x32_to_1x64(rdlo_val, rdhi_val);
    let result = ir.ir().add_64(product, accum, Value::ImmU1(false));

    let lo = ir.ir().least_significant_word(result);
    let hi = ir.ir().most_significant_word(result).result;

    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nz(nz);
    }

    ir.set_register(rd_lo, lo);
    ir.set_register(rd_hi, hi);
    true
}

/// ARM SMULL - signed multiply long.
pub fn arm_smull(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd_hi = Reg::from_u32((inst.raw >> 16) & 0xF);
    let rd_lo = Reg::from_u32((inst.raw >> 12) & 0xF);
    let rm = inst.rm();
    let rs = Reg::from_u32((inst.raw >> 8) & 0xF);
    let s = inst.s_flag();

    let rm_val = ir.get_register(rm);
    let rs_val = ir.get_register(rs);

    let rm64 = ir.ir().sign_extend_word_to_long(rm_val);
    let rs64 = ir.ir().sign_extend_word_to_long(rs_val);
    let result = ir.ir().mul_64(rm64, rs64);

    let lo = ir.ir().least_significant_word(result);
    let hi = ir.ir().most_significant_word(result).result;

    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nz(nz);
    }

    ir.set_register(rd_lo, lo);
    ir.set_register(rd_hi, hi);
    true
}

/// ARM SMLAL - signed multiply accumulate long.
pub fn arm_smlal(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd_hi = Reg::from_u32((inst.raw >> 16) & 0xF);
    let rd_lo = Reg::from_u32((inst.raw >> 12) & 0xF);
    let rm = inst.rm();
    let rs = Reg::from_u32((inst.raw >> 8) & 0xF);
    let s = inst.s_flag();

    let rm_val = ir.get_register(rm);
    let rs_val = ir.get_register(rs);
    let rdhi_val = ir.get_register(rd_hi);
    let rdlo_val = ir.get_register(rd_lo);

    let rm64 = ir.ir().sign_extend_word_to_long(rm_val);
    let rs64 = ir.ir().sign_extend_word_to_long(rs_val);
    let product = ir.ir().mul_64(rm64, rs64);

    let accum = ir.ir().pack_2x32_to_1x64(rdlo_val, rdhi_val);
    let result = ir.ir().add_64(product, accum, Value::ImmU1(false));

    let lo = ir.ir().least_significant_word(result);
    let hi = ir.ir().most_significant_word(result).result;

    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nz(nz);
    }

    ir.set_register(rd_lo, lo);
    ir.set_register(rd_hi, hi);
    true
}

/// ARM UMAAL - unsigned multiply accumulate accumulate long.
pub fn arm_umaal(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd_hi = Reg::from_u32((inst.raw >> 16) & 0xF);
    let rd_lo = Reg::from_u32((inst.raw >> 12) & 0xF);
    let rm = inst.rm();
    let rs = Reg::from_u32((inst.raw >> 8) & 0xF);

    let rm_val = ir.get_register(rm);
    let rs_val = ir.get_register(rs);
    let rdhi_val = ir.get_register(rd_hi);
    let rdlo_val = ir.get_register(rd_lo);

    let rm64 = ir.ir().zero_extend_word_to_long(rm_val);
    let rs64 = ir.ir().zero_extend_word_to_long(rs_val);
    let product = ir.ir().mul_64(rm64, rs64);

    let rdhi64 = ir.ir().zero_extend_word_to_long(rdhi_val);
    let rdlo64 = ir.ir().zero_extend_word_to_long(rdlo_val);
    let sum1 = ir.ir().add_64(product, rdhi64, Value::ImmU1(false));
    let result = ir.ir().add_64(sum1, rdlo64, Value::ImmU1(false));

    let lo = ir.ir().least_significant_word(result);
    let hi = ir.ir().most_significant_word(result).result;

    ir.set_register(rd_lo, lo);
    ir.set_register(rd_hi, hi);
    true
}

/// ARM SMLAL<x><y> - signed halfword multiply accumulate long.
pub fn arm_smlalxy(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd_hi = Reg::from_u32((inst.raw >> 16) & 0xF);
    let rd_lo = Reg::from_u32((inst.raw >> 12) & 0xF);
    let rm = Reg::from_u32((inst.raw >> 8) & 0xF);
    let m = (inst.raw >> 6) & 1 != 0;
    let n = (inst.raw >> 5) & 1 != 0;
    let rn = inst.rm();

    let n16 = signed_halfword_operand(ir, rn, n);
    let m16 = signed_halfword_operand(ir, rm, m);
    let product32 = ir.ir().mul_32(n16, m16);
    let product = ir.ir().sign_extend_word_to_long(product32);
    let rd_lo_val = ir.get_register(rd_lo);
    let rd_hi_val = ir.get_register(rd_hi);
    let addend = ir.ir().pack_2x32_to_1x64(rd_lo_val, rd_hi_val);
    let result = ir.ir().add_64(product, addend, Value::ImmU1(false));
    let lo = ir.ir().least_significant_word(result);
    let hi = ir.ir().most_significant_word(result).result;

    ir.set_register(rd_lo, lo);
    ir.set_register(rd_hi, hi);
    true
}

/// ARM SMLA<x><y> - signed halfword multiply accumulate.
pub fn arm_smlaxy(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = Reg::from_u32((inst.raw >> 16) & 0xF);
    let ra = Reg::from_u32((inst.raw >> 12) & 0xF);
    let rm = Reg::from_u32((inst.raw >> 8) & 0xF);
    let m = (inst.raw >> 6) & 1 != 0;
    let n = (inst.raw >> 5) & 1 != 0;
    let rn = inst.rm();

    let n16 = signed_halfword_operand(ir, rn, n);
    let m16 = signed_halfword_operand(ir, rm, m);
    let product = ir.ir().mul_32(n16, m16);
    let ra_val = ir.get_register(ra);
    let result = ir.ir().add_32(product, ra_val, Value::ImmU1(false));
    let overflow = ir.get_overflow_from(result);

    ir.set_register(rd, result);
    ir.or_q_flag(overflow);
    true
}

/// ARM SMUL<x><y> - signed halfword multiply.
pub fn arm_smulxy(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = Reg::from_u32((inst.raw >> 16) & 0xF);
    let rm = Reg::from_u32((inst.raw >> 8) & 0xF);
    let m = (inst.raw >> 6) & 1 != 0;
    let n = (inst.raw >> 5) & 1 != 0;
    let rn = inst.rm();

    let n16 = signed_halfword_operand(ir, rn, n);
    let m16 = signed_halfword_operand(ir, rm, m);
    let result = ir.ir().mul_32(n16, m16);

    ir.set_register(rd, result);
    true
}

/// ARM SMLAW<y> - signed word by halfword multiply accumulate.
pub fn arm_smlawy(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = Reg::from_u32((inst.raw >> 16) & 0xF);
    let ra = Reg::from_u32((inst.raw >> 12) & 0xF);
    let rm = Reg::from_u32((inst.raw >> 8) & 0xF);
    let m = (inst.raw >> 6) & 1 != 0;
    let rn = inst.rm();

    let rn_val = ir.get_register(rn);
    let n32 = ir.ir().sign_extend_word_to_long(rn_val);
    let m32 = ir.get_register(rm);
    let m16_src = if m {
        ir.ir()
            .logical_shift_right_32(m32, Value::ImmU8(16), Value::ImmU1(false))
    } else {
        m32
    };
    let m16_half = ir.ir().least_significant_half(m16_src);
    let m16_word = ir.ir().sign_extend_half_to_word(m16_half);
    let m16 = ir.ir().sign_extend_word_to_long(m16_word);
    let mul = ir.ir().mul_64(n32, m16);
    let shifted = ir.ir().logical_shift_right_64(mul, Value::ImmU8(16));
    let product = ir.ir().least_significant_word(shifted);
    let ra_val = ir.get_register(ra);
    let result = ir.ir().add_32(product, ra_val, Value::ImmU1(false));
    let overflow = ir.get_overflow_from(result);

    ir.set_register(rd, result);
    ir.or_q_flag(overflow);
    true
}

/// ARM SMULW<y> - signed word by halfword multiply.
pub fn arm_smulwy(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = Reg::from_u32((inst.raw >> 16) & 0xF);
    let rm = Reg::from_u32((inst.raw >> 8) & 0xF);
    let m = (inst.raw >> 6) & 1 != 0;
    let rn = inst.rm();

    let rn_val = ir.get_register(rn);
    let n32 = ir.ir().sign_extend_word_to_long(rn_val);
    let m32 = ir.get_register(rm);
    let m16_src = if m {
        ir.ir()
            .logical_shift_right_32(m32, Value::ImmU8(16), Value::ImmU1(false))
    } else {
        m32
    };
    let m16_half = ir.ir().least_significant_half(m16_src);
    let m16_word = ir.ir().sign_extend_half_to_word(m16_half);
    let m16 = ir.ir().sign_extend_word_to_long(m16_word);
    let mul = ir.ir().mul_64(n32, m16);
    let shifted = ir.ir().logical_shift_right_64(mul, Value::ImmU8(16));
    let result = ir.ir().least_significant_word(shifted);

    ir.set_register(rd, result);
    true
}

/// ARM SMMLA{R} - signed most-significant-word multiply accumulate.
pub fn arm_smmla(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = Reg::from_u32((inst.raw >> 16) & 0xF);
    let ra = Reg::from_u32((inst.raw >> 12) & 0xF);
    let rm = Reg::from_u32((inst.raw >> 8) & 0xF);
    let round = (inst.raw >> 5) & 1 != 0;
    let rn = inst.rm();

    if rd == Reg::R15 || rn == Reg::R15 || rm == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let rn32 = ir.get_register(rn);
    let rm32 = ir.get_register(rm);
    let ra32 = ir.get_register(ra);
    let rn64 = ir.ir().sign_extend_word_to_long(rn32);
    let rm64 = ir.ir().sign_extend_word_to_long(rm32);
    let ra64 = ir.ir().pack_2x32_to_1x64(Value::ImmU32(0), ra32);
    let product = ir.ir().mul_64(rn64, rm64);
    let temp = ir.ir().add_64(ra64, product, Value::ImmU1(false));
    let result_carry = ir.ir().most_significant_word(temp);
    let mut result = result_carry.result;
    if round {
        result = ir.ir().add_32(result, Value::ImmU32(0), result_carry.carry);
    }

    ir.set_register(rd, result);
    true
}

/// ARM SMMLS{R} - signed most-significant-word multiply subtract.
pub fn arm_smmls(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = Reg::from_u32((inst.raw >> 16) & 0xF);
    let ra = Reg::from_u32((inst.raw >> 12) & 0xF);
    let rm = Reg::from_u32((inst.raw >> 8) & 0xF);
    let round = (inst.raw >> 5) & 1 != 0;
    let rn = inst.rm();

    if rd == Reg::R15 || rn == Reg::R15 || rm == Reg::R15 || ra == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let rn32 = ir.get_register(rn);
    let rm32 = ir.get_register(rm);
    let ra32 = ir.get_register(ra);
    let rn64 = ir.ir().sign_extend_word_to_long(rn32);
    let rm64 = ir.ir().sign_extend_word_to_long(rm32);
    let ra64 = ir.ir().pack_2x32_to_1x64(Value::ImmU32(0), ra32);
    let product = ir.ir().mul_64(rn64, rm64);
    let temp = ir.ir().sub_64(ra64, product, Value::ImmU1(true));
    let result_carry = ir.ir().most_significant_word(temp);
    let mut result = result_carry.result;
    if round {
        result = ir.ir().add_32(result, Value::ImmU32(0), result_carry.carry);
    }

    ir.set_register(rd, result);
    true
}

/// ARM SMMUL{R} - signed most-significant-word multiply.
pub fn arm_smmul(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = Reg::from_u32((inst.raw >> 16) & 0xF);
    let rm = Reg::from_u32((inst.raw >> 8) & 0xF);
    let round = (inst.raw >> 5) & 1 != 0;
    let rn = inst.rm();

    if rd == Reg::R15 || rn == Reg::R15 || rm == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let rn32 = ir.get_register(rn);
    let rm32 = ir.get_register(rm);
    let rn64 = ir.ir().sign_extend_word_to_long(rn32);
    let rm64 = ir.ir().sign_extend_word_to_long(rm32);
    let product = ir.ir().mul_64(rn64, rm64);
    let result_carry = ir.ir().most_significant_word(product);
    let mut result = result_carry.result;
    if round {
        result = ir.ir().add_32(result, Value::ImmU32(0), result_carry.carry);
    }

    ir.set_register(rd, result);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder::ArmInstId;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;

    #[test]
    fn flag_setting_multiply_uses_nz_extractor() {
        let loc = A32LocationDescriptor::at(0x4000);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);
        assert!(arm_mul(
            &mut ir,
            &DecodedArm {
                raw: (1 << 20) | (2 << 16) | (1 << 8),
                id: ArmInstId::MUL,
            },
        ));

        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::GetNZFromOp));
        assert!(!block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::GetNZCVFromOp));
    }
}
