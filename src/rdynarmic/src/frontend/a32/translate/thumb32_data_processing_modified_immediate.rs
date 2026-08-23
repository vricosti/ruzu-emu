//! Thumb32 data-processing translation for modified immediates.
//!
//! Upstream owner:
//! `frontend/A32/translate/impl/thumb32_data_processing_modified_immediate.cpp`.

use crate::frontend::a32::decoder_thumb32::DecodedThumb32;
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::value::Value;

pub fn thumb32_tst_imm(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    if n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let carry_in = ir.get_c_flag();
    let imm_carry = super::thumb_expand_imm_c(inst.thumb_expand_imm_bits(), carry_in);
    let reg_n = ir.get_register(n);
    let result = ir.ir().and_32(reg_n, Value::ImmU32(imm_carry.imm32));

    let nz = ir.nz_from(result);
    ir.set_cpsr_nzc(nz, imm_carry.carry);
    true
}

pub fn thumb32_and_imm(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    assert!(!(d == Reg::PC && s), "Decode error");
    if (d == Reg::PC && !s) || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let carry_in = ir.get_c_flag();
    let imm_carry = super::thumb_expand_imm_c(inst.thumb_expand_imm_bits(), carry_in);
    let reg_n = ir.get_register(n);
    let result = ir.ir().and_32(reg_n, Value::ImmU32(imm_carry.imm32));

    ir.set_register(d, result);
    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nzc(nz, imm_carry.carry);
    }
    true
}

pub fn thumb32_bic_imm(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    if d == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let carry_in = ir.get_c_flag();
    let imm_carry = super::thumb_expand_imm_c(inst.thumb_expand_imm_bits(), carry_in);
    let reg_n = ir.get_register(n);
    let result = ir.ir().and_not_32(reg_n, Value::ImmU32(imm_carry.imm32));

    ir.set_register(d, result);
    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nzc(nz, imm_carry.carry);
    }
    true
}

pub fn thumb32_mov_imm(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let d = inst.rd();
    if d == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let carry_in = ir.get_c_flag();
    let imm_carry = super::thumb_expand_imm_c(inst.thumb_expand_imm_bits(), carry_in);
    let result = Value::ImmU32(imm_carry.imm32);

    ir.set_register(d, result);
    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nzc(nz, imm_carry.carry);
    }
    true
}

pub fn thumb32_orr_imm(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    assert!(n != Reg::PC, "Decode error");
    if d == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let carry_in = ir.get_c_flag();
    let imm_carry = super::thumb_expand_imm_c(inst.thumb_expand_imm_bits(), carry_in);
    let reg_n = ir.get_register(n);
    let result = ir.ir().or_32(reg_n, Value::ImmU32(imm_carry.imm32));

    ir.set_register(d, result);
    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nzc(nz, imm_carry.carry);
    }
    true
}

pub fn thumb32_mvn_imm(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let d = inst.rd();
    if d == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let carry_in = ir.get_c_flag();
    let imm_carry = super::thumb_expand_imm_c(inst.thumb_expand_imm_bits(), carry_in);
    let result = Value::ImmU32(!imm_carry.imm32);

    ir.set_register(d, result);
    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nzc(nz, imm_carry.carry);
    }
    true
}

pub fn thumb32_orn_imm(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    assert!(n != Reg::PC, "Decode error");
    if d == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let carry_in = ir.get_c_flag();
    let imm_carry = super::thumb_expand_imm_c(inst.thumb_expand_imm_bits(), carry_in);
    let reg_n = ir.get_register(n);
    let result = ir.ir().or_32(reg_n, Value::ImmU32(!imm_carry.imm32));

    ir.set_register(d, result);
    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nzc(nz, imm_carry.carry);
    }
    true
}

pub fn thumb32_teq_imm(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    if n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let carry_in = ir.get_c_flag();
    let imm_carry = super::thumb_expand_imm_c(inst.thumb_expand_imm_bits(), carry_in);
    let reg_n = ir.get_register(n);
    let result = ir.ir().eor_32(reg_n, Value::ImmU32(imm_carry.imm32));

    let nz = ir.nz_from(result);
    ir.set_cpsr_nzc(nz, imm_carry.carry);
    true
}

pub fn thumb32_eor_imm(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    assert!(!(d == Reg::PC && s), "Decode error");
    if (d == Reg::PC && !s) || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let carry_in = ir.get_c_flag();
    let imm_carry = super::thumb_expand_imm_c(inst.thumb_expand_imm_bits(), carry_in);
    let reg_n = ir.get_register(n);
    let result = ir.ir().eor_32(reg_n, Value::ImmU32(imm_carry.imm32));

    ir.set_register(d, result);
    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nzc(nz, imm_carry.carry);
    }
    true
}

pub fn thumb32_cmn_imm(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    if n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let imm32 = super::thumb_expand_imm(inst.thumb_expand_imm_bits());
    let reg_n = ir.get_register(n);
    let result = ir
        .ir()
        .add_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(false));

    let nzcv = ir.nzcv_from(result);
    ir.set_cpsr_nzcv(nzcv);
    true
}

pub fn thumb32_add_imm_1(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    assert!(!(d == Reg::PC && s), "Decode error");
    if (d == Reg::PC && !s) || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let imm32 = super::thumb_expand_imm(inst.thumb_expand_imm_bits());
    let reg_n = ir.get_register(n);
    let result = ir
        .ir()
        .add_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(false));

    ir.set_register(d, result);
    if s {
        let nzcv = ir.nzcv_from(result);
        ir.set_cpsr_nzcv(nzcv);
    }
    true
}

pub fn thumb32_adc_imm(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    if d == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let imm32 = super::thumb_expand_imm(inst.thumb_expand_imm_bits());
    let reg_n = ir.get_register(n);
    let carry = ir.get_c_flag();
    let result = ir.ir().add_32(reg_n, Value::ImmU32(imm32), carry);

    ir.set_register(d, result);
    if s {
        let nzcv = ir.nzcv_from(result);
        ir.set_cpsr_nzcv(nzcv);
    }
    true
}

pub fn thumb32_sbc_imm(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    if d == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let imm32 = super::thumb_expand_imm(inst.thumb_expand_imm_bits());
    let reg_n = ir.get_register(n);
    let carry = ir.get_c_flag();
    let result = ir.ir().sub_32(reg_n, Value::ImmU32(imm32), carry);

    ir.set_register(d, result);
    if s {
        let nzcv = ir.nzcv_from(result);
        ir.set_cpsr_nzcv(nzcv);
    }
    true
}

pub fn thumb32_cmp_imm(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    if n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let imm32 = super::thumb_expand_imm(inst.thumb_expand_imm_bits());
    let reg_n = ir.get_register(n);
    let result = ir
        .ir()
        .sub_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(true));

    let nzcv = ir.nzcv_from(result);
    ir.set_cpsr_nzcv(nzcv);
    true
}

pub fn thumb32_sub_imm_1(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    assert!(!(d == Reg::PC && s), "Decode error");
    if (d == Reg::PC && !s) || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let imm32 = super::thumb_expand_imm(inst.thumb_expand_imm_bits());
    let reg_n = ir.get_register(n);
    let result = ir
        .ir()
        .sub_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(true));

    ir.set_register(d, result);
    if s {
        let nzcv = ir.nzcv_from(result);
        ir.set_cpsr_nzcv(nzcv);
    }
    true
}

pub fn thumb32_rsb_imm(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    if d == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let imm32 = super::thumb_expand_imm(inst.thumb_expand_imm_bits());
    let reg_n = ir.get_register(n);
    let result = ir
        .ir()
        .sub_32(Value::ImmU32(imm32), reg_n, Value::ImmU1(true));

    ir.set_register(d, result);
    if s {
        let nzcv = ir.nzcv_from(result);
        ir.set_cpsr_nzcv(nzcv);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder_thumb32::{decode_thumb32, Thumb32InstId};
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::frontend::a32::types::Exception;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;

    const PATTERNS: &[(u32, u32, Thumb32InstId)] = &[
        (0xfbf0_8f00, 0xf010_0f00, Thumb32InstId::TST_imm),
        (0xfbe0_8000, 0xf000_0000, Thumb32InstId::AND_imm),
        (0xfbe0_8000, 0xf020_0000, Thumb32InstId::BIC_imm),
        (0xfbef_8000, 0xf04f_0000, Thumb32InstId::MOV_imm),
        (0xfbe0_8000, 0xf040_0000, Thumb32InstId::ORR_imm),
        (0xfbef_8000, 0xf06f_0000, Thumb32InstId::MVN_imm),
        (0xfbe0_8000, 0xf060_0000, Thumb32InstId::ORN_imm),
        (0xfbf0_8f00, 0xf090_0f00, Thumb32InstId::TEQ_imm),
        (0xfbe0_8000, 0xf080_0000, Thumb32InstId::EOR_imm),
        (0xfbf0_8f00, 0xf110_0f00, Thumb32InstId::CMN_imm),
        (0xfbe0_8000, 0xf100_0000, Thumb32InstId::ADD_imm_1),
        (0xfbe0_8000, 0xf140_0000, Thumb32InstId::ADC_imm),
        (0xfbe0_8000, 0xf160_0000, Thumb32InstId::SBC_imm),
        (0xfbf0_8f00, 0xf1b0_0f00, Thumb32InstId::CMP_imm),
        (0xfbe0_8000, 0xf1a0_0000, Thumb32InstId::SUB_imm_1),
        (0xfbe0_8000, 0xf1c0_0000, Thumb32InstId::RSB_imm),
    ];

    fn location() -> A32LocationDescriptor {
        let mut psr = PSR::default();
        psr.set_t(true);
        A32LocationDescriptor::new(0x1000, psr, FPSCR::default(), false)
    }

    fn translate(raw: u32) -> Block {
        let location = location();
        let inst = decode_thumb32((raw >> 16) as u16, raw as u16);
        let mut block = Block::new(location.to_location());
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(super::super::thumb32::translate_thumb32(
                &mut ir,
                &inst,
                super::super::TranslationOptions::default(),
            ));
        }
        block
    }

    #[test]
    fn all_sixteen_upstream_patterns_decode_and_translate() {
        let variable_bits = 0x0401_2255;
        for &(mask, expected, id) in PATTERNS {
            let raw = expected | (variable_bits & !mask);
            let inst = decode_thumb32((raw >> 16) as u16, raw as u16);
            assert_eq!(inst.id, id, "raw={raw:08X}");
            let _ = translate(raw);
        }
    }

    #[test]
    fn invalid_registers_raise_before_carry_or_operand_reads() {
        for raw in [0xF001_0F55u32, 0xF00F_0255, 0xF021_0F55, 0xF04F_0F55] {
            let block = translate_invalid(raw);
            assert!(!block.instructions.iter().any(|inst| {
                matches!(inst.opcode, Opcode::A32GetCFlag | Opcode::A32GetRegister)
            }));
            assert!(block
                .instructions
                .iter()
                .any(|inst| inst.opcode == Opcode::A32ExceptionRaised));
        }
    }

    fn translate_invalid(raw: u32) -> Block {
        let location = location();
        let inst = decode_thumb32((raw >> 16) as u16, raw as u16);
        let mut block = Block::new(location.to_location());
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(!super::super::thumb32::translate_thumb32(
                &mut ir,
                &inst,
                super::super::TranslationOptions::default(),
            ));
        }
        assert!(block.instructions.iter().any(|inst| {
            inst.opcode == Opcode::A32ExceptionRaised
                && inst.args[1]
                    == Value::ImmU64(Exception::UnpredictableInstruction.as_u32() as u64)
        }));
        block
    }

    #[test]
    fn logical_immediate_preserves_carry_read_register_and_flag_order() {
        let block = translate(0xF011_0255);
        let opcodes = block
            .instructions
            .iter()
            .map(|inst| inst.opcode)
            .collect::<Vec<_>>();
        assert_eq!(
            opcodes,
            vec![
                Opcode::A32GetCFlag,
                Opcode::A32GetRegister,
                Opcode::And32,
                Opcode::A32SetRegister,
                Opcode::GetNZFromOp,
                Opcode::A32SetCpsrNZC,
            ]
        );
    }

    #[test]
    fn bic_mvn_and_orn_use_upstream_immediate_operations() {
        let bic = translate(0xF031_0255);
        assert!(bic
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::AndNot32));

        for raw in [0xF07F_0255, 0xF071_0255] {
            let block = translate(raw);
            assert!(!block
                .instructions
                .iter()
                .any(|inst| inst.opcode == Opcode::Not32));
        }
    }

    #[test]
    fn arithmetic_writes_destination_before_extracting_and_setting_flags() {
        let block = translate(0xF111_0255);
        let set_register = block
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::A32SetRegister)
            .expect("destination write");
        let extract_flags = block
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::GetNZCVFromOp)
            .expect("flag extraction");
        let set_flags = block
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::A32SetCpsrNZCV)
            .expect("flag write");
        assert!(set_register < extract_flags && extract_flags < set_flags);
    }

    #[test]
    fn tests_do_not_write_a_destination_register() {
        for raw in [0xF011_0F55, 0xF091_0F55, 0xF111_0F55, 0xF1B1_0F55] {
            let block = translate(raw);
            assert!(!block
                .instructions
                .iter()
                .any(|inst| inst.opcode == Opcode::A32SetRegister));
        }
    }
}
