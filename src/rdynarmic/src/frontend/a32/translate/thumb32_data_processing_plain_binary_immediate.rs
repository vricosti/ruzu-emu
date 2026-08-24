//! Thumb32 data-processing translation for plain binary immediates.
//!
//! Upstream owner:
//! `frontend/A32/translate/impl/thumb32_data_processing_plain_binary_immediate.cpp`.

use super::helpers::{emit_imm_shift, most_significant_half, pack_2x16_to_1x32};
use crate::frontend::a32::decoder_thumb32::DecodedThumb32;
use crate::frontend::a32::types::{Reg, ShiftType};
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::emitter::ResultAndOverflow;
use crate::ir::value::Value;

#[derive(Clone, Copy)]
enum SaturationFunction {
    Signed,
    Unsigned,
}

fn apply_saturation(
    ir: &mut A32IREmitter<'_>,
    operand: Value,
    saturate_to: usize,
    sat_fn: SaturationFunction,
) -> ResultAndOverflow {
    match sat_fn {
        SaturationFunction::Signed => ir.ir().signed_saturation(operand, saturate_to),
        SaturationFunction::Unsigned => ir.ir().unsigned_saturation(operand, saturate_to),
    }
}

fn saturation(
    ir: &mut A32IREmitter<'_>,
    sh: bool,
    n: Reg,
    d: Reg,
    shift_amount: u32,
    saturate_to: usize,
    sat_fn: SaturationFunction,
) -> bool {
    assert!(!(sh && shift_amount == 0), "Invalid decode");

    if d == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let shift = if sh { ShiftType::ASR } else { ShiftType::LSL };
    let reg_n = ir.get_register(n);
    let carry = ir.get_c_flag();
    let (operand, _) = emit_imm_shift(ir, reg_n, shift, shift_amount, carry);
    let result = apply_saturation(ir, operand, saturate_to, sat_fn);

    ir.set_register(d, result.result);
    ir.or_q_flag(result.overflow);
    true
}

fn saturation16(
    ir: &mut A32IREmitter<'_>,
    n: Reg,
    d: Reg,
    saturate_to: usize,
    sat_fn: SaturationFunction,
) -> bool {
    if d == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_n = ir.get_register(n);
    let lo_half = ir.ir().least_significant_half(reg_n);
    let lo_operand = ir.ir().sign_extend_half_to_word(lo_half);
    let hi_half = most_significant_half(ir, reg_n);
    let hi_operand = ir.ir().sign_extend_half_to_word(hi_half);
    let lo_result = apply_saturation(ir, lo_operand, saturate_to, sat_fn);
    let hi_result = apply_saturation(ir, hi_operand, saturate_to, sat_fn);

    let result = pack_2x16_to_1x32(ir, lo_result.result, hi_result.result);
    ir.set_register(d, result);
    ir.or_q_flag(lo_result.overflow);
    ir.or_q_flag(hi_result.overflow);
    true
}

pub fn thumb32_adr_t2(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let d = inst.rd();
    if d == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let result = ir.align_pc(4).wrapping_sub(inst.imm12());
    ir.set_register(d, Value::ImmU32(result));
    true
}

pub fn thumb32_adr_t3(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let d = inst.rd();
    if d == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let result = ir.align_pc(4).wrapping_add(inst.imm12());
    ir.set_register(d, Value::ImmU32(result));
    true
}

pub fn thumb32_add_imm_2(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    if d == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_n = ir.get_register(n);
    let result = ir
        .ir()
        .add_32(reg_n, Value::ImmU32(inst.imm12()), Value::ImmU1(false));
    ir.set_register(d, result);
    true
}

pub fn thumb32_bfc(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let d = inst.rd();
    if d == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (lsbit, msbit) = inst.bfc_lsb_msb();
    if msbit < lsbit {
        return super::unpredictable_instruction(ir);
    }

    let width = msbit - lsbit + 1;
    let mask = !((u32::MAX >> (32 - width)) << lsbit);
    let reg_d = ir.get_register(d);
    let result = ir.ir().and_32(reg_d, Value::ImmU32(mask));
    ir.set_register(d, result);
    true
}

pub fn thumb32_bfi(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    if d == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (lsbit, msbit) = inst.bfc_lsb_msb();
    if msbit < lsbit {
        return super::unpredictable_instruction(ir);
    }

    let width = msbit - lsbit + 1;
    let inclusion_mask = (u32::MAX >> (32 - width)) << lsbit;
    let exclusion_mask = !inclusion_mask;
    let reg_d = ir.get_register(d);
    let operand1 = ir.ir().and_32(reg_d, Value::ImmU32(exclusion_mask));
    let reg_n = ir.get_register(n);
    let shifted =
        ir.ir()
            .logical_shift_left_32(reg_n, Value::ImmU8(lsbit as u8), Value::ImmU1(false));
    let operand2 = ir.ir().and_32(shifted, Value::ImmU32(inclusion_mask));
    let result = ir.ir().or_32(operand1, operand2);
    ir.set_register(d, result);
    true
}

pub fn thumb32_movt(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let d = inst.rd();
    if d == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let imm16 = Value::ImmU32(inst.imm16() << 16);
    let operand = ir.get_register(d);
    let low_half = ir.ir().and_32(operand, Value::ImmU32(0x0000_ffff));
    let result = ir.ir().or_32(low_half, imm16);
    ir.set_register(d, result);
    true
}

pub fn thumb32_movw_imm(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let d = inst.rd();
    if d == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    ir.set_register(d, Value::ImmU32(inst.imm16()));
    true
}

pub fn thumb32_sbfx(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    if d == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (lsbit, width) = inst.bfx_lsb_width();
    if lsbit + width - 1 >= 32 {
        return super::unpredictable_instruction(ir);
    }

    let left_shift_amount = 32 - width - lsbit;
    let right_shift_amount = 32 - width;
    let operand = ir.get_register(n);
    let tmp = ir.ir().logical_shift_left_32(
        operand,
        Value::ImmU8(left_shift_amount as u8),
        Value::ImmU1(false),
    );
    let result = ir.ir().arithmetic_shift_right_32(
        tmp,
        Value::ImmU8(right_shift_amount as u8),
        Value::ImmU1(false),
    );
    ir.set_register(d, result);
    true
}

pub fn thumb32_ssat(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let shift_amount = (((inst.raw >> 12) & 7) << 2) | ((inst.raw >> 6) & 3);
    saturation(
        ir,
        inst.raw & (1 << 21) != 0,
        inst.rn(),
        inst.rd(),
        shift_amount,
        ((inst.raw & 0x1f) + 1) as usize,
        SaturationFunction::Signed,
    )
}

pub fn thumb32_ssat16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    saturation16(
        ir,
        inst.rn(),
        inst.rd(),
        ((inst.raw & 0xf) + 1) as usize,
        SaturationFunction::Signed,
    )
}

pub fn thumb32_sub_imm_2(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    if d == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_n = ir.get_register(n);
    let result = ir
        .ir()
        .sub_32(reg_n, Value::ImmU32(inst.imm12()), Value::ImmU1(true));
    ir.set_register(d, result);
    true
}

pub fn thumb32_ubfx(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    if d == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let (lsbit, width) = inst.bfx_lsb_width();
    if lsbit + width - 1 >= 32 {
        return super::unpredictable_instruction(ir);
    }

    let operand = ir.get_register(n);
    let shifted =
        ir.ir()
            .logical_shift_right_32(operand, Value::ImmU8(lsbit as u8), Value::ImmU1(false));
    let mask = Value::ImmU32(u32::MAX >> (32 - width));
    let result = ir.ir().and_32(shifted, mask);
    ir.set_register(d, result);
    true
}

pub fn thumb32_usat(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let shift_amount = (((inst.raw >> 12) & 7) << 2) | ((inst.raw >> 6) & 3);
    saturation(
        ir,
        inst.raw & (1 << 21) != 0,
        inst.rn(),
        inst.rd(),
        shift_amount,
        (inst.raw & 0x1f) as usize,
        SaturationFunction::Unsigned,
    )
}

pub fn thumb32_usat16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    saturation16(
        ir,
        inst.rn(),
        inst.rd(),
        (inst.raw & 0xf) as usize,
        SaturationFunction::Unsigned,
    )
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
        (0xfbff_8000, 0xf20f_0000, Thumb32InstId::ADR_t3),
        (0xfbf0_8000, 0xf200_0000, Thumb32InstId::ADD_imm_2),
        (0xfbf0_8000, 0xf240_0000, Thumb32InstId::MOVW_imm),
        (0xfbff_8000, 0xf2af_0000, Thumb32InstId::ADR_t2),
        (0xfbf0_8000, 0xf2a0_0000, Thumb32InstId::SUB_imm_2),
        (0xfbf0_8000, 0xf2c0_0000, Thumb32InstId::MOVT),
        (0xff70_f0f0, 0xf320_0010, Thumb32InstId::UDF),
        (0xfff0_f0f0, 0xf320_0000, Thumb32InstId::SSAT16),
        (0xfff0_f0f0, 0xf3a0_0000, Thumb32InstId::USAT16),
        (0xffd0_8020, 0xf300_0000, Thumb32InstId::SSAT),
        (0xffd0_8020, 0xf380_0000, Thumb32InstId::USAT),
        (0xfff0_8020, 0xf340_0000, Thumb32InstId::SBFX),
        (0xffff_8020, 0xf36f_0000, Thumb32InstId::BFC),
        (0xfff0_8020, 0xf360_0000, Thumb32InstId::BFI),
        (0xfff0_8020, 0xf3c0_0000, Thumb32InstId::UBFX),
    ];

    fn location() -> A32LocationDescriptor {
        let mut psr = PSR::default();
        psr.set_t(true);
        A32LocationDescriptor::new(0x1002, psr, FPSCR::default(), false)
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
    fn all_plain_binary_immediate_patterns_decode_and_translate() {
        let variable_bits = 0x0401_2255;
        for &(mask, expected, id) in PATTERNS {
            let raw = expected | (variable_bits & !mask);
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                id,
                "raw={raw:08X}"
            );
            if id != Thumb32InstId::UDF {
                let _ = translate(raw);
            }
        }
    }

    #[test]
    fn invalid_fields_raise_before_reading_registers() {
        for raw in [0xf36f_1203, 0xf341_72c1, 0xf3c1_72c1, 0xf240_0f00] {
            let (result, block) = translate(raw);
            assert!(!result, "raw={raw:08X}");
            assert!(!block
                .instructions
                .iter()
                .any(|inst| inst.opcode == Opcode::A32GetRegister));
            assert!(block.instructions.iter().any(|inst| {
                inst.opcode == Opcode::A32ExceptionRaised
                    && inst.args[1]
                        == Value::ImmU64(Exception::UnpredictableInstruction.as_u32() as u64)
            }));
        }
    }

    #[test]
    fn bfi_reads_destination_then_source_and_keeps_zero_shift() {
        let (result, block) = translate(0xf361_0207);
        assert!(result);
        let reads = block
            .instructions
            .iter()
            .filter(|inst| inst.opcode == Opcode::A32GetRegister)
            .map(|inst| inst.args[0])
            .collect::<Vec<_>>();
        assert_eq!(
            reads,
            vec![Value::ImmA32Reg(Reg::R2), Value::ImmA32Reg(Reg::R1)]
        );
        assert!(block.instructions.iter().any(
            |inst| inst.opcode == Opcode::LogicalShiftLeft32 && inst.args[1] == Value::ImmU8(0)
        ));
    }

    #[test]
    fn saturation16_reads_source_once_and_sets_both_q_results_after_destination() {
        let (result, block) = translate(0xf321_0207);
        assert!(result);
        assert_eq!(
            block
                .instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::A32GetRegister)
                .count(),
            1
        );
        assert_eq!(
            block
                .instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::SignedSaturation)
                .count(),
            2
        );
        let set_register = block
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::A32SetRegister)
            .expect("destination write");
        let q_positions = block
            .instructions
            .iter()
            .enumerate()
            .filter(|(_, inst)| inst.opcode == Opcode::A32OrQFlag)
            .map(|(position, _)| position)
            .collect::<Vec<_>>();
        assert_eq!(q_positions.len(), 2);
        assert!(q_positions.iter().all(|&position| position > set_register));
    }

    #[test]
    fn bitfield_extracts_emit_upstream_shifts_even_for_zero_lsb() {
        let (_, sbfx) = translate(0xf341_0207);
        assert!(sbfx
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::LogicalShiftLeft32));
        assert!(sbfx
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::ArithmeticShiftRight32));

        let (_, ubfx) = translate(0xf3c1_0207);
        assert!(ubfx.instructions.iter().any(|inst| {
            inst.opcode == Opcode::LogicalShiftRight32 && inst.args[1] == Value::ImmU8(0)
        }));
    }

    #[test]
    fn adr_uses_aligned_architectural_pc() {
        let (_, block) = translate(0xf20f_0201);
        let write = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::A32SetRegister)
            .expect("destination write");
        assert_eq!(write.args[1], Value::ImmU32(0x1005));
    }
}
