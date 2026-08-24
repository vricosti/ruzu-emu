//! Thumb32 multiply, multiply-accumulate, and absolute-difference translation.
//!
//! Upstream owner: `frontend/A32/translate/impl/thumb32_multiply.cpp`.

use crate::frontend::a32::decoder_thumb32::DecodedThumb32;
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::value::Value;

pub fn thumb32_mla(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let a = inst.ra();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC || a == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_a = ir.get_register(a);
    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let product = ir.ir().mul_32(reg_n, reg_m);
    let result = ir.ir().add_32(product, reg_a, Value::ImmU1(false));
    ir.set_register(d, result);
    true
}

pub fn thumb32_mls(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let a = inst.ra();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC || a == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_a = ir.get_register(a);
    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let product = ir.ir().mul_32(reg_n, reg_m);
    let result = ir.ir().sub_32(reg_a, product, Value::ImmU1(true));
    ir.set_register(d, result);
    true
}

pub fn thumb32_mul(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().mul_32(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_smlad(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let a = inst.ra();
    let d = inst.rd();
    let x = inst.raw & (1 << 4) != 0;
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC || a == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let n32 = ir.get_register(n);
    let m32 = ir.get_register(m);
    let n_lo = ir.ir().least_significant_half(n32);
    let n_lo = ir.ir().sign_extend_half_to_word(n_lo);
    let n_hi = ir
        .ir()
        .arithmetic_shift_right_32(n32, Value::ImmU8(16), Value::ImmU1(false));
    let mut m_lo = ir.ir().least_significant_half(m32);
    m_lo = ir.ir().sign_extend_half_to_word(m_lo);
    let mut m_hi = ir
        .ir()
        .arithmetic_shift_right_32(m32, Value::ImmU8(16), Value::ImmU1(false));
    if x {
        std::mem::swap(&mut m_lo, &mut m_hi);
    }

    let product_lo = ir.ir().mul_32(n_lo, m_lo);
    let product_hi = ir.ir().mul_32(n_hi, m_hi);
    let addend = ir.get_register(a);
    let mut result = ir.ir().add_32(product_lo, product_hi, Value::ImmU1(false));
    let overflow = ir.get_overflow_from(result);
    ir.or_q_flag(overflow);
    result = ir.ir().add_32(result, addend, Value::ImmU1(false));
    ir.set_register(d, result);
    let overflow = ir.get_overflow_from(result);
    ir.or_q_flag(overflow);
    true
}

pub fn thumb32_smlsd(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let a = inst.ra();
    let d = inst.rd();
    let x = inst.raw & (1 << 4) != 0;
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC || a == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let n32 = ir.get_register(n);
    let m32 = ir.get_register(m);
    let n_lo = ir.ir().least_significant_half(n32);
    let n_lo = ir.ir().sign_extend_half_to_word(n_lo);
    let n_hi = ir
        .ir()
        .arithmetic_shift_right_32(n32, Value::ImmU8(16), Value::ImmU1(false));
    let mut m_lo = ir.ir().least_significant_half(m32);
    m_lo = ir.ir().sign_extend_half_to_word(m_lo);
    let mut m_hi = ir
        .ir()
        .arithmetic_shift_right_32(m32, Value::ImmU8(16), Value::ImmU1(false));
    if x {
        std::mem::swap(&mut m_lo, &mut m_hi);
    }

    let product_lo = ir.ir().mul_32(n_lo, m_lo);
    let product_hi = ir.ir().mul_32(n_hi, m_hi);
    let addend = ir.get_register(a);
    let product = ir.ir().sub_32(product_lo, product_hi, Value::ImmU1(true));
    let result = ir.ir().add_32(product, addend, Value::ImmU1(false));
    ir.set_register(d, result);
    let overflow = ir.get_overflow_from(result);
    ir.or_q_flag(overflow);
    true
}

pub fn thumb32_smlaxy(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let a = inst.ra();
    let d = inst.rd();
    let select_n_high = inst.raw & (1 << 5) != 0;
    let select_m_high = inst.raw & (1 << 4) != 0;
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC || a == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let n32 = ir.get_register(n);
    let m32 = ir.get_register(m);
    let n16 = if select_n_high {
        ir.ir()
            .arithmetic_shift_right_32(n32, Value::ImmU8(16), Value::ImmU1(false))
    } else {
        let n16 = ir.ir().least_significant_half(n32);
        ir.ir().sign_extend_half_to_word(n16)
    };
    let m16 = if select_m_high {
        ir.ir()
            .arithmetic_shift_right_32(m32, Value::ImmU8(16), Value::ImmU1(false))
    } else {
        let m16 = ir.ir().least_significant_half(m32);
        ir.ir().sign_extend_half_to_word(m16)
    };
    let product = ir.ir().mul_32(n16, m16);
    let reg_a = ir.get_register(a);
    let result = ir.ir().add_32(product, reg_a, Value::ImmU1(false));
    ir.set_register(d, result);
    let overflow = ir.get_overflow_from(result);
    ir.or_q_flag(overflow);
    true
}

pub fn thumb32_smmla(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let a = inst.ra();
    let d = inst.rd();
    let round = inst.raw & (1 << 4) != 0;
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC || a == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let n32 = ir.get_register(n);
    let n64 = ir.ir().sign_extend_word_to_long(n32);
    let m32 = ir.get_register(m);
    let m64 = ir.ir().sign_extend_word_to_long(m32);
    let reg_a = ir.get_register(a);
    let a64 = ir.ir().pack_2x32_to_1x64(Value::ImmU32(0), reg_a);
    let product = ir.ir().mul_64(n64, m64);
    let temp = ir.ir().add_64(a64, product, Value::ImmU1(false));
    let result_carry = ir.ir().most_significant_word(temp);
    let mut result = result_carry.result;
    if round {
        result = ir.ir().add_32(result, Value::ImmU32(0), result_carry.carry);
    }
    ir.set_register(d, result);
    true
}

pub fn thumb32_smmls(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let a = inst.ra();
    let d = inst.rd();
    let round = inst.raw & (1 << 4) != 0;
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC || a == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let n32 = ir.get_register(n);
    let n64 = ir.ir().sign_extend_word_to_long(n32);
    let m32 = ir.get_register(m);
    let m64 = ir.ir().sign_extend_word_to_long(m32);
    let reg_a = ir.get_register(a);
    let a64 = ir.ir().pack_2x32_to_1x64(Value::ImmU32(0), reg_a);
    let product = ir.ir().mul_64(n64, m64);
    let temp = ir.ir().sub_64(a64, product, Value::ImmU1(true));
    let result_carry = ir.ir().most_significant_word(temp);
    let mut result = result_carry.result;
    if round {
        result = ir.ir().add_32(result, Value::ImmU32(0), result_carry.carry);
    }
    ir.set_register(d, result);
    true
}

pub fn thumb32_smmul(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let round = inst.raw & (1 << 4) != 0;
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let n32 = ir.get_register(n);
    let n64 = ir.ir().sign_extend_word_to_long(n32);
    let m32 = ir.get_register(m);
    let m64 = ir.ir().sign_extend_word_to_long(m32);
    let product = ir.ir().mul_64(n64, m64);
    let result_carry = ir.ir().most_significant_word(product);
    let mut result = result_carry.result;
    if round {
        result = ir.ir().add_32(result, Value::ImmU32(0), result_carry.carry);
    }
    ir.set_register(d, result);
    true
}

pub fn thumb32_smuad(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let exchange = inst.raw & (1 << 4) != 0;
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let n32 = ir.get_register(n);
    let m32 = ir.get_register(m);
    let n_lo = ir.ir().least_significant_half(n32);
    let n_lo = ir.ir().sign_extend_half_to_word(n_lo);
    let n_hi = ir
        .ir()
        .arithmetic_shift_right_32(n32, Value::ImmU8(16), Value::ImmU1(false));
    let mut m_lo = ir.ir().least_significant_half(m32);
    m_lo = ir.ir().sign_extend_half_to_word(m_lo);
    let mut m_hi = ir
        .ir()
        .arithmetic_shift_right_32(m32, Value::ImmU8(16), Value::ImmU1(false));
    if exchange {
        std::mem::swap(&mut m_lo, &mut m_hi);
    }
    let product_lo = ir.ir().mul_32(n_lo, m_lo);
    let product_hi = ir.ir().mul_32(n_hi, m_hi);
    let result = ir.ir().add_32(product_lo, product_hi, Value::ImmU1(false));
    ir.set_register(d, result);
    let overflow = ir.get_overflow_from(result);
    ir.or_q_flag(overflow);
    true
}

pub fn thumb32_smusd(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let exchange = inst.raw & (1 << 4) != 0;
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let n32 = ir.get_register(n);
    let m32 = ir.get_register(m);
    let n_lo = ir.ir().least_significant_half(n32);
    let n_lo = ir.ir().sign_extend_half_to_word(n_lo);
    let n_hi = ir
        .ir()
        .arithmetic_shift_right_32(n32, Value::ImmU8(16), Value::ImmU1(false));
    let mut m_lo = ir.ir().least_significant_half(m32);
    m_lo = ir.ir().sign_extend_half_to_word(m_lo);
    let mut m_hi = ir
        .ir()
        .arithmetic_shift_right_32(m32, Value::ImmU8(16), Value::ImmU1(false));
    if exchange {
        std::mem::swap(&mut m_lo, &mut m_hi);
    }
    let product_lo = ir.ir().mul_32(n_lo, m_lo);
    let product_hi = ir.ir().mul_32(n_hi, m_hi);
    let result = ir.ir().sub_32(product_lo, product_hi, Value::ImmU1(true));
    ir.set_register(d, result);
    true
}

pub fn thumb32_smulxy(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let select_n_high = inst.raw & (1 << 5) != 0;
    let select_m_high = inst.raw & (1 << 4) != 0;
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let n32 = ir.get_register(n);
    let m32 = ir.get_register(m);
    let n16 = if select_n_high {
        ir.ir()
            .arithmetic_shift_right_32(n32, Value::ImmU8(16), Value::ImmU1(false))
    } else {
        let n16 = ir.ir().least_significant_half(n32);
        ir.ir().sign_extend_half_to_word(n16)
    };
    let m16 = if select_m_high {
        ir.ir()
            .arithmetic_shift_right_32(m32, Value::ImmU8(16), Value::ImmU1(false))
    } else {
        let m16 = ir.ir().least_significant_half(m32);
        ir.ir().sign_extend_half_to_word(m16)
    };
    let result = ir.ir().mul_32(n16, m16);
    ir.set_register(d, result);
    true
}

pub fn thumb32_smlawy(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let a = inst.ra();
    let d = inst.rd();
    let select_m_high = inst.raw & (1 << 4) != 0;
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC || a == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let n32 = ir.get_register(n);
    let n32 = ir.ir().sign_extend_word_to_long(n32);
    let mut m32 = ir.get_register(m);
    if select_m_high {
        m32 = ir
            .ir()
            .logical_shift_right_32(m32, Value::ImmU8(16), Value::ImmU1(false));
    }
    let m16 = ir.ir().least_significant_half(m32);
    let m16 = ir.ir().sign_extend_half_to_word(m16);
    let m16 = ir.ir().sign_extend_word_to_long(m16);
    let product = ir.ir().mul_64(n32, m16);
    let product = ir.ir().logical_shift_right_64(product, Value::ImmU8(16));
    let product = ir.ir().least_significant_word(product);
    let reg_a = ir.get_register(a);
    let result = ir.ir().add_32(product, reg_a, Value::ImmU1(false));
    ir.set_register(d, result);
    let overflow = ir.get_overflow_from(result);
    ir.or_q_flag(overflow);
    true
}

pub fn thumb32_smulwy(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let select_m_high = inst.raw & (1 << 4) != 0;
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let n32 = ir.get_register(n);
    let n32 = ir.ir().sign_extend_word_to_long(n32);
    let mut m32 = ir.get_register(m);
    if select_m_high {
        m32 = ir
            .ir()
            .logical_shift_right_32(m32, Value::ImmU8(16), Value::ImmU1(false));
    }
    let m16 = ir.ir().least_significant_half(m32);
    let m16 = ir.ir().sign_extend_half_to_word(m16);
    let m16 = ir.ir().sign_extend_word_to_long(m16);
    let product = ir.ir().mul_64(n32, m16);
    let result = ir.ir().logical_shift_right_64(product, Value::ImmU8(16));
    let result = ir.ir().least_significant_word(result);
    ir.set_register(d, result);
    true
}

pub fn thumb32_usad8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_abs_diff_sum_u8(reg_n, reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_usada8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let a = inst.ra();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC || a == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_a = ir.get_register(a);
    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let tmp = ir.ir().packed_abs_diff_sum_u8(reg_n, reg_m);
    let result = ir.ir().add_32(reg_a, tmp, Value::ImmU1(false));
    ir.set_register(d, result);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder_thumb32::{decode_thumb32, Thumb32InstId};
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;

    fn location() -> A32LocationDescriptor {
        A32LocationDescriptor::new(0x1000, PSR::default(), FPSCR::default(), false)
    }

    fn translate(raw: u32) -> (Thumb32InstId, Block, bool) {
        let loc = location();
        let mut block = Block::new(loc.to_location());
        let inst = decode_thumb32((raw >> 16) as u16, raw as u16);
        let result = {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            match inst.id {
                Thumb32InstId::MLA => thumb32_mla(&mut ir, &inst),
                Thumb32InstId::MLS => thumb32_mls(&mut ir, &inst),
                Thumb32InstId::MUL => thumb32_mul(&mut ir, &inst),
                Thumb32InstId::SMLAD => thumb32_smlad(&mut ir, &inst),
                Thumb32InstId::SMLAXY => thumb32_smlaxy(&mut ir, &inst),
                Thumb32InstId::SMLAWY => thumb32_smlawy(&mut ir, &inst),
                Thumb32InstId::SMLSD => thumb32_smlsd(&mut ir, &inst),
                Thumb32InstId::SMMLA => thumb32_smmla(&mut ir, &inst),
                Thumb32InstId::SMMLS => thumb32_smmls(&mut ir, &inst),
                Thumb32InstId::SMMUL => thumb32_smmul(&mut ir, &inst),
                Thumb32InstId::SMUAD => thumb32_smuad(&mut ir, &inst),
                Thumb32InstId::SMUSD => thumb32_smusd(&mut ir, &inst),
                Thumb32InstId::SMULXY => thumb32_smulxy(&mut ir, &inst),
                Thumb32InstId::SMULWY => thumb32_smulwy(&mut ir, &inst),
                Thumb32InstId::USAD8 => thumb32_usad8(&mut ir, &inst),
                Thumb32InstId::USADA8 => thumb32_usada8(&mut ir, &inst),
                other => panic!("unexpected decoder result {other:?}"),
            }
        };
        (inst.id, block, result)
    }

    #[test]
    fn all_upstream_multiply_visitors_translate() {
        for (raw, expected) in [
            (0xFB01_F203u32, Thumb32InstId::MUL),
            (0xFB01_4203, Thumb32InstId::MLA),
            (0xFB01_4213, Thumb32InstId::MLS),
            (0xFB11_F233, Thumb32InstId::SMULXY),
            (0xFB11_4233, Thumb32InstId::SMLAXY),
            (0xFB21_F213, Thumb32InstId::SMUAD),
            (0xFB21_4213, Thumb32InstId::SMLAD),
            (0xFB31_F213, Thumb32InstId::SMULWY),
            (0xFB31_4213, Thumb32InstId::SMLAWY),
            (0xFB41_F213, Thumb32InstId::SMUSD),
            (0xFB41_4213, Thumb32InstId::SMLSD),
            (0xFB51_F213, Thumb32InstId::SMMUL),
            (0xFB51_4213, Thumb32InstId::SMMLA),
            (0xFB61_4213, Thumb32InstId::SMMLS),
            (0xFB71_F203, Thumb32InstId::USAD8),
            (0xFB71_4203, Thumb32InstId::USADA8),
        ] {
            let (id, block, result) = translate(raw);
            assert_eq!(id, expected, "raw={raw:08X}");
            assert!(result, "raw={raw:08X}");
            assert!(!block
                .instructions
                .iter()
                .any(|inst| inst.opcode == Opcode::A32ExceptionRaised));
        }
    }

    #[test]
    fn mla_and_usada8_preserve_upstream_register_read_order() {
        for (raw, expected) in [
            (0xFB01_4203u32, [Reg::R4, Reg::R3, Reg::R1]),
            (0xFB71_4203, [Reg::R4, Reg::R3, Reg::R1]),
        ] {
            let (_, block, result) = translate(raw);
            assert!(result);
            let reads = block
                .instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::A32GetRegister)
                .map(|inst| inst.args[0])
                .collect::<Vec<_>>();
            assert_eq!(
                reads,
                expected.map(Value::ImmA32Reg).to_vec(),
                "raw={raw:08X}"
            );
        }
    }

    #[test]
    fn invalid_registers_raise_before_any_operand_read() {
        let loc = location();
        let mut block = Block::new(loc.to_location());
        let inst = DecodedThumb32 {
            raw: 0xFB0F_4203,
            id: Thumb32InstId::MLA,
        };
        {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            assert!(!thumb32_mla(&mut ir, &inst));
        }
        assert!(!block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32GetRegister));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32ExceptionRaised));
    }

    #[test]
    fn smlad_updates_q_before_and_after_accumulation_in_upstream_order() {
        let (_, block, result) = translate(0xFB21_4213);
        assert!(result);
        let add = block
            .instructions
            .iter()
            .enumerate()
            .filter(|(_, inst)| inst.opcode == Opcode::Add32)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let set = block
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::A32SetRegister)
            .expect("destination write");
        let q = block
            .instructions
            .iter()
            .enumerate()
            .filter(|(_, inst)| inst.opcode == Opcode::A32OrQFlag)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        assert_eq!(add.len(), 2);
        assert_eq!(q.len(), 2);
        assert!(add[0] < q[0] && q[0] < add[1]);
        assert!(add[1] < set && set < q[1]);
    }

    #[test]
    fn rounding_and_absolute_difference_use_dedicated_upstream_ir() {
        let (_, rounded, _) = translate(0xFB51_F213);
        assert!(rounded
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::GetCarryFromOp));

        let (_, abs_diff, _) = translate(0xFB71_F203);
        assert_eq!(
            abs_diff
                .instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::PackedAbsDiffSumU8)
                .count(),
            1
        );
    }
}
