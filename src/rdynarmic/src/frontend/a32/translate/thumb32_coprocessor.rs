use crate::frontend::a32::decoder_thumb32::DecodedThumb32;
use crate::frontend::a32::types::Reg;
use crate::interface::a32::coprocessor_util::CoprocReg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::value::Value;

pub fn thumb32_mcrr(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let t2 = inst.rn();
    let t = inst.rt();
    let word1 = ir.get_register(t);
    let word2 = ir.get_register(t2);
    ir.coproc_send_two_words(
        inst.coproc_no() as usize,
        inst.coproc_two(),
        inst.coproc_transfer_opc() as usize,
        CoprocReg::from_u8(inst.coproc_crm() as u8),
        word1,
        word2,
    );
    true
}

pub fn thumb32_mrrc(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let t2 = inst.rn();
    let t = inst.rt();
    let two_words = ir.coproc_get_two_words(
        inst.coproc_no() as usize,
        inst.coproc_two(),
        inst.coproc_transfer_opc() as usize,
        CoprocReg::from_u8(inst.coproc_crm() as u8),
    );
    let low = ir.ir().least_significant_word(two_words);
    let high = ir.ir().most_significant_word(two_words).result;
    ir.set_register(t, low);
    ir.set_register(t2, high);
    true
}

pub fn thumb32_stc(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let p = inst.raw & (1 << 24) != 0;
    let u = inst.raw & (1 << 23) != 0;
    let d = inst.raw & (1 << 22) != 0;
    let w = inst.raw & (1 << 21) != 0;
    let n = inst.rn();
    let imm8 = inst.imm8();
    let imm32 = imm8 << 2;
    let reg_n = ir.get_register(n);
    let offset_address = if u {
        ir.ir()
            .add_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(false))
    } else {
        ir.ir()
            .sub_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(true))
    };
    let address = if p { offset_address } else { reg_n };
    ir.coproc_store_words(
        inst.coproc_no() as usize,
        inst.coproc_two(),
        d,
        CoprocReg::from_u8(inst.coproc_crd() as u8),
        address,
        !p && !w && u,
        imm8 as u8,
    );
    if w {
        ir.set_register(n, offset_address);
    }
    true
}

pub fn thumb32_ldc(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let p = inst.raw & (1 << 24) != 0;
    let u = inst.raw & (1 << 23) != 0;
    let d = inst.raw & (1 << 22) != 0;
    let w = inst.raw & (1 << 21) != 0;
    let n = inst.rn();
    let imm8 = inst.imm8();
    let imm32 = imm8 << 2;
    let reg_n = ir.get_register(n);
    let offset_address = if u {
        ir.ir()
            .add_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(false))
    } else {
        ir.ir()
            .sub_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(true))
    };
    let address = if p { offset_address } else { reg_n };
    ir.coproc_load_words(
        inst.coproc_no() as usize,
        inst.coproc_two(),
        d,
        CoprocReg::from_u8(inst.coproc_crd() as u8),
        address,
        !p && !w && u,
        imm8 as u8,
    );
    if w {
        ir.set_register(n, offset_address);
    }
    true
}

pub fn thumb32_cdp(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    ir.coproc_internal_operation(
        inst.coproc_no() as usize,
        inst.coproc_two(),
        inst.coproc_dp_opc1() as usize,
        CoprocReg::from_u8(inst.coproc_crd() as u8),
        CoprocReg::from_u8(inst.coproc_crn() as u8),
        CoprocReg::from_u8(inst.coproc_crm() as u8),
        inst.coproc_opc2() as usize,
    );
    true
}

pub fn thumb32_mcr(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let t = inst.rt();
    let word = ir.get_register(t);
    ir.coproc_send_one_word(
        inst.coproc_no() as usize,
        inst.coproc_two(),
        inst.coproc_opc1() as usize,
        CoprocReg::from_u8(inst.coproc_crn() as u8),
        CoprocReg::from_u8(inst.coproc_crm() as u8),
        inst.coproc_opc2() as usize,
        word,
    );
    true
}

pub fn thumb32_mrc(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let t = inst.rt();
    let word = ir.coproc_get_one_word(
        inst.coproc_no() as usize,
        inst.coproc_two(),
        inst.coproc_opc1() as usize,
        CoprocReg::from_u8(inst.coproc_crn() as u8),
        CoprocReg::from_u8(inst.coproc_crm() as u8),
        inst.coproc_opc2() as usize,
    );
    if t != Reg::R15 {
        ir.set_register(t, word);
    } else {
        let new_cpsr_nzcv = ir.ir().and_32(word, Value::ImmU32(0xF000_0000));
        ir.set_cpsr_nzcv_raw(new_cpsr_nzcv);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder_thumb32::{decode_thumb32, Thumb32InstId};
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;

    #[test]
    fn mcr_preserves_all_upstream_metadata_fields() {
        let raw: u32 = 0xFE00_0010 | (5 << 21) | (4 << 16) | (3 << 12) | (15 << 8) | (2 << 5) | 1;
        let decoded = decode_thumb32((raw >> 16) as u16, raw as u16);
        assert_eq!(decoded.id, Thumb32InstId::MCR);

        let location = A32LocationDescriptor::at(0x1000).set_t_flag(true);
        let mut block = Block::new(location.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, location);
        assert!(thumb32_mcr(&mut ir, &decoded));

        let coproc = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::A32CoprocSendOneWord)
            .expect("MCR must emit A32CoprocSendOneWord");
        assert_eq!(
            coproc.args[0].get_coproc_info().to_le_bytes(),
            [15, 1, 5, 4, 1, 2, 0, 0]
        );
    }

    #[test]
    fn ldc_option_form_uses_base_address_without_writeback() {
        let raw: u32 =
            0xEC10_0000 | (1 << 23) | (1 << 22) | (4 << 16) | (7 << 12) | (15 << 8) | 0x22;
        let decoded = decode_thumb32((raw >> 16) as u16, raw as u16);
        assert_eq!(decoded.id, Thumb32InstId::LDC);

        let location = A32LocationDescriptor::at(0x2000).set_t_flag(true);
        let mut block = Block::new(location.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, location);
        assert!(thumb32_ldc(&mut ir, &decoded));

        let coproc = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::A32CoprocLoadWords)
            .expect("LDC must emit A32CoprocLoadWords");
        assert_eq!(
            coproc.args[0].get_coproc_info().to_le_bytes(),
            [15, 0, 1, 7, 1, 0x22, 0, 0]
        );
        assert!(!block.instructions.iter().any(|inst| {
            inst.opcode == Opcode::A32SetRegister && inst.args[0].get_a32_reg() == Reg::R4
        }));
    }
}
