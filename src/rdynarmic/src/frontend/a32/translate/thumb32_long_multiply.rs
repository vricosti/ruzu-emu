//! Thumb32 long multiply, long multiply-accumulate, and divide translation.
//!
//! Upstream owner: `frontend/A32/translate/impl/thumb32_long_multiply.cpp`.

use crate::frontend::a32::decoder_thumb32::DecodedThumb32;
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::emitter::IREmitter;
use crate::ir::value::Value;

type DivideFunction = for<'a> fn(&mut IREmitter<'a>, Value, Value) -> Value;

fn signed_divide(ir: &mut IREmitter<'_>, operand1: Value, operand2: Value) -> Value {
    ir.signed_div_32(operand1, operand2)
}

fn unsigned_divide(ir: &mut IREmitter<'_>, operand1: Value, operand2: Value) -> Value {
    ir.unsigned_div_32(operand1, operand2)
}

fn divide_operation(
    ir: &mut A32IREmitter<'_>,
    d: Reg,
    m: Reg,
    n: Reg,
    function: DivideFunction,
) -> bool {
    if d == Reg::PC || m == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let operand1 = ir.get_register(n);
    let operand2 = ir.get_register(m);
    let result = function(ir.ir(), operand1, operand2);

    ir.set_register(d, result);
    true
}

pub fn thumb32_sdiv(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    divide_operation(ir, inst.rd(), inst.rm(), inst.rn(), signed_divide)
}

pub fn thumb32_smlal(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d_lo = inst.rd_lo();
    let d_hi = inst.rd_hi();
    let m = inst.rm();
    if d_lo == Reg::PC || d_hi == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    if d_hi == d_lo {
        return super::unpredictable_instruction(ir);
    }

    let n32 = ir.get_register(n);
    let n64 = ir.ir().sign_extend_word_to_long(n32);
    let m32 = ir.get_register(m);
    let m64 = ir.ir().sign_extend_word_to_long(m32);
    let product = ir.ir().mul_64(n64, m64);
    let addend_lo = ir.get_register(d_lo);
    let addend_hi = ir.get_register(d_hi);
    let addend = ir.ir().pack_2x32_to_1x64(addend_lo, addend_hi);
    let result = ir.ir().add_64(product, addend, Value::ImmU1(false));
    let lo = ir.ir().least_significant_word(result);
    let hi = ir.ir().most_significant_word(result).result;

    ir.set_register(d_lo, lo);
    ir.set_register(d_hi, hi);
    true
}

pub fn thumb32_smlald(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d_lo = inst.rd_lo();
    let d_hi = inst.rd_hi();
    let exchange = inst.raw & (1 << 4) != 0;
    let m = inst.rm();
    if d_lo == Reg::PC || d_hi == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    if d_hi == d_lo {
        return super::unpredictable_instruction(ir);
    }

    let n32 = ir.get_register(n);
    let m32 = ir.get_register(m);
    let n_lo_half = ir.ir().least_significant_half(n32);
    let n_lo = ir.ir().sign_extend_half_to_word(n_lo_half);
    let n_hi = ir
        .ir()
        .arithmetic_shift_right_32(n32, Value::ImmU8(16), Value::ImmU1(false));

    let m_lo_half = ir.ir().least_significant_half(m32);
    let mut m_lo = ir.ir().sign_extend_half_to_word(m_lo_half);
    let mut m_hi = ir
        .ir()
        .arithmetic_shift_right_32(m32, Value::ImmU8(16), Value::ImmU1(false));
    if exchange {
        std::mem::swap(&mut m_lo, &mut m_hi);
    }

    let product_lo = ir.ir().mul_32(n_lo, m_lo);
    let product_lo = ir.ir().sign_extend_word_to_long(product_lo);
    let product_hi = ir.ir().mul_32(n_hi, m_hi);
    let product_hi = ir.ir().sign_extend_word_to_long(product_hi);
    let addend_lo = ir.get_register(d_lo);
    let addend_hi = ir.get_register(d_hi);
    let addend = ir.ir().pack_2x32_to_1x64(addend_lo, addend_hi);
    let products = ir.ir().add_64(product_lo, product_hi, Value::ImmU1(false));
    let result = ir.ir().add_64(products, addend, Value::ImmU1(false));

    let lo = ir.ir().least_significant_word(result);
    ir.set_register(d_lo, lo);
    let hi = ir.ir().most_significant_word(result).result;
    ir.set_register(d_hi, hi);
    true
}

pub fn thumb32_smlalxy(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d_lo = inst.rd_lo();
    let d_hi = inst.rd_hi();
    let select_n_high = inst.raw & (1 << 5) != 0;
    let select_m_high = inst.raw & (1 << 4) != 0;
    let m = inst.rm();
    if d_lo == Reg::PC || d_hi == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    if d_hi == d_lo {
        return super::unpredictable_instruction(ir);
    }

    let n32 = ir.get_register(n);
    let m32 = ir.get_register(m);
    let n16 = if select_n_high {
        ir.ir()
            .arithmetic_shift_right_32(n32, Value::ImmU8(16), Value::ImmU1(false))
    } else {
        let half = ir.ir().least_significant_half(n32);
        ir.ir().sign_extend_half_to_word(half)
    };
    let m16 = if select_m_high {
        ir.ir()
            .arithmetic_shift_right_32(m32, Value::ImmU8(16), Value::ImmU1(false))
    } else {
        let half = ir.ir().least_significant_half(m32);
        ir.ir().sign_extend_half_to_word(half)
    };
    let product = ir.ir().mul_32(n16, m16);
    let product = ir.ir().sign_extend_word_to_long(product);
    let addend_lo = ir.get_register(d_lo);
    let addend_hi = ir.get_register(d_hi);
    let addend = ir.ir().pack_2x32_to_1x64(addend_lo, addend_hi);
    let result = ir.ir().add_64(product, addend, Value::ImmU1(false));

    let lo = ir.ir().least_significant_word(result);
    ir.set_register(d_lo, lo);
    let hi = ir.ir().most_significant_word(result).result;
    ir.set_register(d_hi, hi);
    true
}

pub fn thumb32_smlsld(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d_lo = inst.rd_lo();
    let d_hi = inst.rd_hi();
    let exchange = inst.raw & (1 << 4) != 0;
    let m = inst.rm();
    if d_lo == Reg::PC || d_hi == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    if d_hi == d_lo {
        return super::unpredictable_instruction(ir);
    }

    let n32 = ir.get_register(n);
    let m32 = ir.get_register(m);
    let n_lo_half = ir.ir().least_significant_half(n32);
    let n_lo = ir.ir().sign_extend_half_to_word(n_lo_half);
    let n_hi = ir
        .ir()
        .arithmetic_shift_right_32(n32, Value::ImmU8(16), Value::ImmU1(false));

    let m_lo_half = ir.ir().least_significant_half(m32);
    let mut m_lo = ir.ir().sign_extend_half_to_word(m_lo_half);
    let mut m_hi = ir
        .ir()
        .arithmetic_shift_right_32(m32, Value::ImmU8(16), Value::ImmU1(false));
    if exchange {
        std::mem::swap(&mut m_lo, &mut m_hi);
    }

    let product_lo = ir.ir().mul_32(n_lo, m_lo);
    let product_lo = ir.ir().sign_extend_word_to_long(product_lo);
    let product_hi = ir.ir().mul_32(n_hi, m_hi);
    let product_hi = ir.ir().sign_extend_word_to_long(product_hi);
    let addend_lo = ir.get_register(d_lo);
    let addend_hi = ir.get_register(d_hi);
    let addend = ir.ir().pack_2x32_to_1x64(addend_lo, addend_hi);
    let products = ir.ir().sub_64(product_lo, product_hi, Value::ImmU1(true));
    let result = ir.ir().add_64(products, addend, Value::ImmU1(false));

    let lo = ir.ir().least_significant_word(result);
    ir.set_register(d_lo, lo);
    let hi = ir.ir().most_significant_word(result).result;
    ir.set_register(d_hi, hi);
    true
}

pub fn thumb32_smull(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d_lo = inst.rd_lo();
    let d_hi = inst.rd_hi();
    let m = inst.rm();
    if d_lo == Reg::PC || d_hi == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    if d_hi == d_lo {
        return super::unpredictable_instruction(ir);
    }

    let n32 = ir.get_register(n);
    let n64 = ir.ir().sign_extend_word_to_long(n32);
    let m32 = ir.get_register(m);
    let m64 = ir.ir().sign_extend_word_to_long(m32);
    let result = ir.ir().mul_64(n64, m64);
    let lo = ir.ir().least_significant_word(result);
    let hi = ir.ir().most_significant_word(result).result;

    ir.set_register(d_lo, lo);
    ir.set_register(d_hi, hi);
    true
}

pub fn thumb32_udiv(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    divide_operation(ir, inst.rd(), inst.rm(), inst.rn(), unsigned_divide)
}

pub fn thumb32_umlal(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d_lo = inst.rd_lo();
    let d_hi = inst.rd_hi();
    let m = inst.rm();
    if d_lo == Reg::PC || d_hi == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    if d_hi == d_lo {
        return super::unpredictable_instruction(ir);
    }

    let n32 = ir.get_register(n);
    let n64 = ir.ir().zero_extend_word_to_long(n32);
    let m32 = ir.get_register(m);
    let m64 = ir.ir().zero_extend_word_to_long(m32);
    let product = ir.ir().mul_64(n64, m64);
    let addend_lo = ir.get_register(d_lo);
    let addend_hi = ir.get_register(d_hi);
    let addend = ir.ir().pack_2x32_to_1x64(addend_lo, addend_hi);
    let result = ir.ir().add_64(product, addend, Value::ImmU1(false));
    let lo = ir.ir().least_significant_word(result);
    let hi = ir.ir().most_significant_word(result).result;

    ir.set_register(d_lo, lo);
    ir.set_register(d_hi, hi);
    true
}

pub fn thumb32_umull(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d_lo = inst.rd_lo();
    let d_hi = inst.rd_hi();
    let m = inst.rm();
    if d_lo == Reg::PC || d_hi == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    if d_hi == d_lo {
        return super::unpredictable_instruction(ir);
    }

    let n32 = ir.get_register(n);
    let n64 = ir.ir().zero_extend_word_to_long(n32);
    let m32 = ir.get_register(m);
    let m64 = ir.ir().zero_extend_word_to_long(m32);
    let result = ir.ir().mul_64(n64, m64);
    let lo = ir.ir().least_significant_word(result);
    let hi = ir.ir().most_significant_word(result).result;

    ir.set_register(d_lo, lo);
    ir.set_register(d_hi, hi);
    true
}

pub fn thumb32_umaal(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d_lo = inst.rd_lo();
    let d_hi = inst.rd_hi();
    let m = inst.rm();
    if d_lo == Reg::PC || d_hi == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    if d_hi == d_lo {
        return super::unpredictable_instruction(ir);
    }

    let lo32 = ir.get_register(d_lo);
    let lo64 = ir.ir().zero_extend_word_to_long(lo32);
    let hi32 = ir.get_register(d_hi);
    let hi64 = ir.ir().zero_extend_word_to_long(hi32);
    let n32 = ir.get_register(n);
    let n64 = ir.ir().zero_extend_word_to_long(n32);
    let m32 = ir.get_register(m);
    let m64 = ir.ir().zero_extend_word_to_long(m32);
    let product = ir.ir().mul_64(n64, m64);
    let product_and_hi = ir.ir().add_64(product, hi64, Value::ImmU1(false));
    let result = ir.ir().add_64(product_and_hi, lo64, Value::ImmU1(false));

    let lo = ir.ir().least_significant_word(result);
    ir.set_register(d_lo, lo);
    let hi = ir.ir().most_significant_word(result).result;
    ir.set_register(d_hi, hi);
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
        let mut psr = PSR::default();
        psr.set_t(true);
        A32LocationDescriptor::new(0x1000, psr, FPSCR::default(), false)
    }

    fn translate(raw: u32) -> Block {
        let loc = location();
        let inst = decode_thumb32((raw >> 16) as u16, raw as u16);
        let mut block = Block::new(loc.to_location());
        {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            assert!(super::super::thumb32::translate_thumb32(
                &mut ir,
                &inst,
                super::super::TranslationOptions::default(),
            ));
        }
        block
    }

    fn get_registers(block: &Block) -> Vec<Reg> {
        block
            .instructions
            .iter()
            .filter(|inst| inst.opcode == Opcode::A32GetRegister)
            .map(|inst| match inst.args[0] {
                Value::ImmA32Reg(reg) => reg,
                value => panic!("unexpected GetRegister argument {value:?}"),
            })
            .collect()
    }

    #[test]
    fn all_upstream_long_multiply_patterns_translate() {
        for (raw, expected) in [
            (0xFB81_2303, Thumb32InstId::SMULL),
            (0xFB91_F2F3, Thumb32InstId::SDIV),
            (0xFBA1_2303, Thumb32InstId::UMULL),
            (0xFBB1_F2F3, Thumb32InstId::UDIV),
            (0xFBC1_2303, Thumb32InstId::SMLAL),
            (0xFBC1_23B3, Thumb32InstId::SMLALXY),
            (0xFBC1_23D3, Thumb32InstId::SMLALD),
            (0xFBD1_23D3, Thumb32InstId::SMLSLD),
            (0xFBE1_2303, Thumb32InstId::UMLAL),
            (0xFBE1_2363, Thumb32InstId::UMAAL),
        ] {
            let inst = decode_thumb32((raw >> 16) as u16, raw as u16);
            assert_eq!(inst.id, expected, "raw={raw:08X}");
            let _ = translate(raw);
        }
    }

    #[test]
    fn invalid_registers_are_rejected_before_operand_reads() {
        for raw in [0xFB8F_2303u32, 0xFB81_2203, 0xFB9F_F2F3] {
            let loc = location();
            let inst = decode_thumb32((raw >> 16) as u16, raw as u16);
            let mut block = Block::new(loc.to_location());
            {
                let mut ir = A32IREmitter::with_location(&mut block, loc);
                assert!(!super::super::thumb32::translate_thumb32(
                    &mut ir,
                    &inst,
                    super::super::TranslationOptions::default(),
                ));
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
    }

    #[test]
    fn smlal_and_umaal_preserve_upstream_operand_read_order() {
        assert_eq!(
            get_registers(&translate(0xFBC1_2304)),
            vec![Reg::R1, Reg::R4, Reg::R2, Reg::R3]
        );
        assert_eq!(
            get_registers(&translate(0xFBE1_2364)),
            vec![Reg::R2, Reg::R3, Reg::R1, Reg::R4]
        );
    }

    #[test]
    fn smlald_sets_low_register_before_extracting_high_word() {
        let block = translate(0xFBC1_23D4);
        let low_set = block
            .instructions
            .iter()
            .position(|inst| {
                inst.opcode == Opcode::A32SetRegister && inst.args[0] == Value::ImmA32Reg(Reg::R2)
            })
            .expect("low-register write");
        let high_extract = block
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::MostSignificantWord)
            .expect("high-word extraction");
        assert!(low_set < high_extract);
    }
}
