//! Thumb32 data-processing translation for shifted registers.
//!
//! Upstream owner:
//! `frontend/A32/translate/impl/thumb32_data_processing_shifted_register.cpp`.

use super::helpers::emit_imm_shift;
use crate::frontend::a32::decoder_thumb32::DecodedThumb32;
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::value::Value;

fn shifted_register(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> (Value, Value) {
    let (shift_type, shift_amount) = inst.shift_type_amount();
    let reg_m = ir.get_register(inst.rm());
    let carry = ir.get_c_flag();
    emit_imm_shift(ir, reg_m, shift_type, shift_amount, carry)
}

pub fn thumb32_tst_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let m = inst.rm();
    if n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (shifted, carry) = shifted_register(ir, inst);
    let reg_n = ir.get_register(n);
    let result = ir.ir().and_32(reg_n, shifted);
    let nz = ir.nz_from(result);
    ir.set_cpsr_nzc(nz, carry);
    true
}

pub fn thumb32_and_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    assert!(!(d == Reg::PC && s), "Decode error");
    if (d == Reg::PC && !s) || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (shifted, carry) = shifted_register(ir, inst);
    let reg_n = ir.get_register(n);
    let result = ir.ir().and_32(reg_n, shifted);
    ir.set_register(d, result);
    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nzc(nz, carry);
    }
    true
}

pub fn thumb32_bic_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (shifted, carry) = shifted_register(ir, inst);
    let reg_n = ir.get_register(n);
    let result = ir.ir().and_not_32(reg_n, shifted);
    ir.set_register(d, result);
    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nzc(nz, carry);
    }
    true
}

pub fn thumb32_mov_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (result, carry) = shifted_register(ir, inst);
    ir.set_register(d, result);
    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nzc(nz, carry);
    }
    true
}

pub fn thumb32_orr_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    assert!(n != Reg::PC, "Decode error");
    if d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (shifted, carry) = shifted_register(ir, inst);
    let reg_n = ir.get_register(n);
    let result = ir.ir().or_32(reg_n, shifted);
    ir.set_register(d, result);
    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nzc(nz, carry);
    }
    true
}

pub fn thumb32_mvn_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (shifted, carry) = shifted_register(ir, inst);
    let result = ir.ir().not_32(shifted);
    ir.set_register(d, result);
    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nzc(nz, carry);
    }
    true
}

pub fn thumb32_orn_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    assert!(n != Reg::PC, "Decode error");
    if d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (shifted, carry) = shifted_register(ir, inst);
    let reg_n = ir.get_register(n);
    let not_shifted = ir.ir().not_32(shifted);
    let result = ir.ir().or_32(reg_n, not_shifted);
    ir.set_register(d, result);
    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nzc(nz, carry);
    }
    true
}

pub fn thumb32_teq_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let m = inst.rm();
    if n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (shifted, carry) = shifted_register(ir, inst);
    let reg_n = ir.get_register(n);
    let result = ir.ir().eor_32(reg_n, shifted);
    let nz = ir.nz_from(result);
    ir.set_cpsr_nzc(nz, carry);
    true
}

pub fn thumb32_eor_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    assert!(!(d == Reg::PC && s), "Decode error");
    if (d == Reg::PC && !s) || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (shifted, carry) = shifted_register(ir, inst);
    let reg_n = ir.get_register(n);
    let result = ir.ir().eor_32(reg_n, shifted);
    ir.set_register(d, result);
    if s {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nzc(nz, carry);
    }
    true
}

pub fn thumb32_pkh(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let tb = inst.raw & (1 << 5) != 0;
    let (operand2, _) = shifted_register(ir, inst);
    let lower_source = if tb { operand2 } else { ir.get_register(n) };
    let lower = ir.ir().and_32(lower_source, Value::ImmU32(0x0000_ffff));
    let upper_source = if tb { ir.get_register(n) } else { operand2 };
    let upper = ir.ir().and_32(upper_source, Value::ImmU32(0xffff_0000));
    let result = ir.ir().or_32(upper, lower);
    ir.set_register(d, result);
    true
}

pub fn thumb32_cmn_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let m = inst.rm();
    if n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (shifted, _) = shifted_register(ir, inst);
    let reg_n = ir.get_register(n);
    let result = ir.ir().add_32(reg_n, shifted, Value::ImmU1(false));
    let nzcv = ir.nzcv_from(result);
    ir.set_cpsr_nzcv(nzcv);
    true
}

pub fn thumb32_add_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    assert!(!(d == Reg::PC && s), "Decode error");
    if (d == Reg::PC && !s) || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (shifted, _) = shifted_register(ir, inst);
    let reg_n = ir.get_register(n);
    let result = ir.ir().add_32(reg_n, shifted, Value::ImmU1(false));
    ir.set_register(d, result);
    if s {
        let nzcv = ir.nzcv_from(result);
        ir.set_cpsr_nzcv(nzcv);
    }
    true
}

pub fn thumb32_adc_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (shifted, _) = shifted_register(ir, inst);
    let reg_n = ir.get_register(n);
    let carry = ir.get_c_flag();
    let result = ir.ir().add_32(reg_n, shifted, carry);
    ir.set_register(d, result);
    if s {
        let nzcv = ir.nzcv_from(result);
        ir.set_cpsr_nzcv(nzcv);
    }
    true
}

pub fn thumb32_sbc_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (shifted, _) = shifted_register(ir, inst);
    let reg_n = ir.get_register(n);
    let carry = ir.get_c_flag();
    let result = ir.ir().sub_32(reg_n, shifted, carry);
    ir.set_register(d, result);
    if s {
        let nzcv = ir.nzcv_from(result);
        ir.set_cpsr_nzcv(nzcv);
    }
    true
}

pub fn thumb32_cmp_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let m = inst.rm();
    if n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (shifted, _) = shifted_register(ir, inst);
    let reg_n = ir.get_register(n);
    let result = ir.ir().sub_32(reg_n, shifted, Value::ImmU1(true));
    let nzcv = ir.nzcv_from(result);
    ir.set_cpsr_nzcv(nzcv);
    true
}

pub fn thumb32_sub_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    assert!(!(d == Reg::PC && s), "Decode error");
    if (d == Reg::PC && !s) || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (shifted, _) = shifted_register(ir, inst);
    let reg_n = ir.get_register(n);
    let result = ir.ir().sub_32(reg_n, shifted, Value::ImmU1(true));
    ir.set_register(d, result);
    if s {
        let nzcv = ir.nzcv_from(result);
        ir.set_cpsr_nzcv(nzcv);
    }
    true
}

pub fn thumb32_rsb_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let s = inst.s_flag();
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (shifted, _) = shifted_register(ir, inst);
    let reg_n = ir.get_register(n);
    let result = ir.ir().sub_32(shifted, reg_n, Value::ImmU1(true));
    ir.set_register(d, result);
    if s {
        let nzcv = ir.nzcv_from(result);
        ir.set_cpsr_nzcv(nzcv);
    }
    true
}

#[cfg(test)]
mod tests {
    use crate::frontend::a32::decoder_thumb32::{decode_thumb32, Thumb32InstId};
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::frontend::a32::types::{Exception, Reg};
    use crate::ir::a32_emitter::A32IREmitter;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::value::Value;

    const PATTERNS: &[(u32, u32, Thumb32InstId)] = &[
        (0xfff0_8f00, 0xea10_0f00, Thumb32InstId::TstReg),
        (0xffe0_8000, 0xea00_0000, Thumb32InstId::AndReg),
        (0xffe0_8000, 0xea20_0000, Thumb32InstId::BicReg),
        (0xffef_8000, 0xea4f_0000, Thumb32InstId::MovReg),
        (0xffe0_8000, 0xea40_0000, Thumb32InstId::OrrReg),
        (0xffef_8000, 0xea6f_0000, Thumb32InstId::MvnReg),
        (0xffe0_8000, 0xea60_0000, Thumb32InstId::OrnReg),
        (0xfff0_8f00, 0xea90_0f00, Thumb32InstId::TeqReg),
        (0xffe0_8000, 0xea80_0000, Thumb32InstId::EorReg),
        (0xfff0_8010, 0xeac0_0000, Thumb32InstId::PKH),
        (0xfff0_8f00, 0xeb10_0f00, Thumb32InstId::CmnReg),
        (0xffe0_8000, 0xeb00_0000, Thumb32InstId::AddReg),
        (0xffe0_8000, 0xeb40_0000, Thumb32InstId::AdcReg),
        (0xffe0_8000, 0xeb60_0000, Thumb32InstId::SbcReg),
        (0xfff0_8f00, 0xebb0_0f00, Thumb32InstId::CmpReg),
        (0xffe0_8000, 0xeba0_0000, Thumb32InstId::SubReg),
        (0xffe0_8000, 0xebc0_0000, Thumb32InstId::RsbReg),
    ];

    fn location() -> A32LocationDescriptor {
        let mut psr = PSR::default();
        psr.set_t(true);
        A32LocationDescriptor::new(0x1000, psr, FPSCR::default(), false)
    }

    fn translate(raw: u32) -> (bool, Block) {
        let location = location();
        let inst = decode_thumb32((raw >> 16) as u16, raw as u16);
        let mut block = Block::new(location.to_location());
        let result = {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            super::super::thumb32::translate_thumb32(
                &mut ir,
                &inst,
                super::super::TranslationOptions::default(),
            )
        };
        (result, block)
    }

    #[test]
    fn all_shifted_register_patterns_decode_and_translate() {
        let variable_bits = 0x0011_2243;
        for &(mask, expected, id) in PATTERNS {
            let raw = expected | (variable_bits & !mask);
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                id,
                "raw={raw:08X}"
            );
            assert!(translate(raw).0, "raw={raw:08X}");
        }
    }

    #[test]
    fn invalid_registers_raise_before_shift_inputs() {
        for raw in [0xea1f_0f02, 0xea01_0f02, 0xea21_0f02] {
            let (result, block) = translate(raw);
            assert!(!result, "raw={raw:08X}");
            assert!(!block.instructions.iter().any(|inst| {
                matches!(inst.opcode, Opcode::A32GetRegister | Opcode::A32GetCFlag)
            }));
            assert!(block.instructions.iter().any(|inst| {
                inst.opcode == Opcode::A32ExceptionRaised
                    && inst.args[1]
                        == Value::ImmU64(Exception::UnpredictableInstruction.as_u32() as u64)
            }));
        }
    }

    #[test]
    fn mov_reads_only_shift_source_and_writes_destination_before_flags() {
        let (_, block) = translate(0xea5f_0201);
        let opcodes = block
            .instructions
            .iter()
            .map(|inst| inst.opcode)
            .collect::<Vec<_>>();
        assert_eq!(
            opcodes,
            vec![
                Opcode::A32GetRegister,
                Opcode::A32GetCFlag,
                Opcode::A32SetRegister,
                Opcode::GetNZFromOp,
                Opcode::A32SetCpsrNZC,
            ]
        );
        assert_eq!(block.instructions[0].args[0], Value::ImmA32Reg(Reg::R1));
    }

    #[test]
    fn bic_uses_and_not_and_destination_precedes_flags() {
        let (_, block) = translate(0xea31_0203);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::AndNot32));
        assert!(!block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::Not32));
        let set_register = block
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::A32SetRegister)
            .unwrap();
        let set_flags = block
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::A32SetCpsrNZC)
            .unwrap();
        assert!(set_register < set_flags);
    }

    #[test]
    fn adc_and_sbc_read_carry_for_shift_and_arithmetic() {
        for raw in [0xeb51_0203, 0xeb71_0203] {
            let (_, block) = translate(raw);
            assert_eq!(
                block
                    .instructions
                    .iter()
                    .filter(|inst| inst.opcode == Opcode::A32GetCFlag)
                    .count(),
                2
            );
        }
    }

    #[test]
    fn pkh_preserves_tb_dependent_source_read_order() {
        let (_, bottom) = translate(0xeac1_0203);
        let bottom_n = bottom
            .instructions
            .iter()
            .position(|inst| {
                inst.opcode == Opcode::A32GetRegister && inst.args[0] == Value::ImmA32Reg(Reg::R1)
            })
            .unwrap();
        let bottom_first_and = bottom
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::And32)
            .unwrap();
        assert!(bottom_n < bottom_first_and);

        let (_, top) = translate(0xeac1_0223);
        let top_n = top
            .instructions
            .iter()
            .position(|inst| {
                inst.opcode == Opcode::A32GetRegister && inst.args[0] == Value::ImmA32Reg(Reg::R1)
            })
            .unwrap();
        let top_first_and = top
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::And32)
            .unwrap();
        assert!(top_n > top_first_and);
    }
}
