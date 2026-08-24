//! Thumb32 data-processing translation for register operands.
//!
//! Upstream owner:
//! `frontend/A32/translate/impl/thumb32_data_processing_register.cpp`.

use super::helpers::{emit_reg_shift, rotate};
use crate::frontend::a32::decoder_thumb32::DecodedThumb32;
use crate::frontend::a32::types::{Reg, ShiftType};
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::value::Value;

fn shift_instruction(
    ir: &mut A32IREmitter<'_>,
    inst: &DecodedThumb32,
    shift_type: ShiftType,
) -> bool {
    let m = inst.rn();
    let d = inst.rd();
    let s = inst.rm();
    if d == Reg::PC || m == Reg::PC || s == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_s = ir.get_register(s);
    let shift_s = ir.ir().least_significant_byte(reg_s);
    let apsr_c = ir.get_c_flag();
    let reg_m = ir.get_register(m);
    let (result, carry) = emit_reg_shift(ir, reg_m, shift_type, shift_s, apsr_c);

    if inst.s_flag() {
        let nz = ir.nz_from(result);
        ir.set_cpsr_nzc(nz, carry);
    }
    ir.set_register(d, result);
    true
}

pub fn thumb32_asr_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    shift_instruction(ir, inst, ShiftType::ASR)
}

pub fn thumb32_lsl_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    shift_instruction(ir, inst, ShiftType::LSL)
}

pub fn thumb32_lsr_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    shift_instruction(ir, inst, ShiftType::LSR)
}

pub fn thumb32_ror_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    shift_instruction(ir, inst, ShiftType::ROR)
}

fn rotation(inst: &DecodedThumb32) -> u32 {
    (inst.raw >> 4) & 3
}

pub fn thumb32_sxtb(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let rotated = rotate(ir, m, rotation(inst));
    let byte = ir.ir().least_significant_byte(rotated);
    let result = ir.ir().sign_extend_byte_to_word(byte);
    ir.set_register(d, result);
    true
}

pub fn thumb32_sxtb16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let rotated = rotate(ir, m, rotation(inst));
    let low_byte = ir.ir().and_32(rotated, Value::ImmU32(0x00ff_00ff));
    let sign_bit = ir.ir().and_32(rotated, Value::ImmU32(0x0080_0080));
    let sign_extension = ir.ir().mul_32(sign_bit, Value::ImmU32(0x1fe));
    let result = ir.ir().or_32(low_byte, sign_extension);
    ir.set_register(d, result);
    true
}

pub fn thumb32_sxtab(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let rotated = rotate(ir, m, rotation(inst));
    let reg_n = ir.get_register(n);
    let byte = ir.ir().least_significant_byte(rotated);
    let extended = ir.ir().sign_extend_byte_to_word(byte);
    let result = ir.ir().add_32(reg_n, extended, Value::ImmU1(false));
    ir.set_register(d, result);
    true
}

pub fn thumb32_sxtab16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let rotated = rotate(ir, m, rotation(inst));
    let low_byte = ir.ir().and_32(rotated, Value::ImmU32(0x00ff_00ff));
    let sign_bit = ir.ir().and_32(rotated, Value::ImmU32(0x0080_0080));
    let sign_extension = ir.ir().mul_32(sign_bit, Value::ImmU32(0x1fe));
    let addend = ir.ir().or_32(low_byte, sign_extension);
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_add_u16(addend, reg_n).result;
    ir.set_register(d, result);
    true
}

pub fn thumb32_sxth(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let rotated = rotate(ir, m, rotation(inst));
    let half = ir.ir().least_significant_half(rotated);
    let result = ir.ir().sign_extend_half_to_word(half);
    ir.set_register(d, result);
    true
}

pub fn thumb32_sxtah(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let rotated = rotate(ir, m, rotation(inst));
    let reg_n = ir.get_register(n);
    let half = ir.ir().least_significant_half(rotated);
    let extended = ir.ir().sign_extend_half_to_word(half);
    let result = ir.ir().add_32(reg_n, extended, Value::ImmU1(false));
    ir.set_register(d, result);
    true
}

pub fn thumb32_uxtb(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let rotated = rotate(ir, m, rotation(inst));
    let byte = ir.ir().least_significant_byte(rotated);
    let result = ir.ir().zero_extend_byte_to_word(byte);
    ir.set_register(d, result);
    true
}

pub fn thumb32_uxtb16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let rotated = rotate(ir, m, rotation(inst));
    let result = ir.ir().and_32(rotated, Value::ImmU32(0x00ff_00ff));
    ir.set_register(d, result);
    true
}

pub fn thumb32_uxtab(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let rotated = rotate(ir, m, rotation(inst));
    let reg_n = ir.get_register(n);
    let byte = ir.ir().least_significant_byte(rotated);
    let extended = ir.ir().zero_extend_byte_to_word(byte);
    let result = ir.ir().add_32(reg_n, extended, Value::ImmU1(false));
    ir.set_register(d, result);
    true
}

pub fn thumb32_uxtab16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let rotated = rotate(ir, m, rotation(inst));
    let masked = ir.ir().and_32(rotated, Value::ImmU32(0x00ff_00ff));
    let reg_n = ir.get_register(n);
    let result = ir.ir().packed_add_u16(reg_n, masked).result;
    ir.set_register(d, result);
    true
}

pub fn thumb32_uxth(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let rotated = rotate(ir, m, rotation(inst));
    let half = ir.ir().least_significant_half(rotated);
    let result = ir.ir().zero_extend_half_to_word(half);
    ir.set_register(d, result);
    true
}

pub fn thumb32_uxtah(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let rotated = rotate(ir, m, rotation(inst));
    let reg_n = ir.get_register(n);
    let half = ir.ir().least_significant_half(rotated);
    let extended = ir.ir().zero_extend_half_to_word(half);
    let result = ir.ir().add_32(reg_n, extended, Value::ImmU1(false));
    ir.set_register(d, result);
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
        (0xffe0_f0f0, 0xfa00_f000, Thumb32InstId::LSL_reg),
        (0xffe0_f0f0, 0xfa20_f000, Thumb32InstId::LSR_reg),
        (0xffe0_f0f0, 0xfa40_f000, Thumb32InstId::ASR_reg),
        (0xffe0_f0f0, 0xfa60_f000, Thumb32InstId::ROR_reg),
        (0xffff_f0c0, 0xfa0f_f080, Thumb32InstId::SXTH),
        (0xfff0_f0c0, 0xfa00_f080, Thumb32InstId::SXTAH),
        (0xffff_f0c0, 0xfa1f_f080, Thumb32InstId::UXTH),
        (0xfff0_f0c0, 0xfa10_f080, Thumb32InstId::UXTAH),
        (0xffff_f0c0, 0xfa2f_f080, Thumb32InstId::SXTB16),
        (0xfff0_f0c0, 0xfa20_f080, Thumb32InstId::SXTAB16),
        (0xffff_f0c0, 0xfa3f_f080, Thumb32InstId::UXTB16),
        (0xfff0_f0c0, 0xfa30_f080, Thumb32InstId::UXTAB16),
        (0xffff_f0c0, 0xfa4f_f080, Thumb32InstId::SXTB),
        (0xfff0_f0c0, 0xfa40_f080, Thumb32InstId::SXTAB),
        (0xffff_f0c0, 0xfa5f_f080, Thumb32InstId::UXTB),
        (0xfff0_f0c0, 0xfa50_f080, Thumb32InstId::UXTAB),
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
    fn all_register_patterns_decode_and_translate() {
        let variable_bits = 0x0011_02a3;
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
    fn invalid_registers_raise_before_input_reads() {
        for raw in [
            0xfa01_ff03,
            0xfa01_f20f,
            0xfa0f_f203,
            0xfa0f_ff83,
            0xfa0f_f08f,
        ] {
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
    fn shift_instruction_preserves_source_carry_flags_and_destination_order() {
        let (_, block) = translate(0xfa11_f203);
        let opcodes = block
            .instructions
            .iter()
            .map(|inst| inst.opcode)
            .collect::<Vec<_>>();
        assert_eq!(
            opcodes,
            vec![
                Opcode::A32GetRegister,
                Opcode::LeastSignificantByte,
                Opcode::A32GetCFlag,
                Opcode::A32GetRegister,
                Opcode::LogicalShiftLeft32,
                Opcode::GetCarryFromOp,
                Opcode::GetNZFromOp,
                Opcode::A32SetCpsrNZC,
                Opcode::A32SetRegister,
            ]
        );
        assert_eq!(block.instructions[0].args[0], Value::ImmA32Reg(Reg::R3));
        assert_eq!(block.instructions[3].args[0], Value::ImmA32Reg(Reg::R1));
    }

    #[test]
    fn extension_rotate_zero_is_not_optimized_away() {
        let (_, block) = translate(0xfa0f_f283);
        assert!(block.instructions.iter().any(|inst| {
            inst.opcode == Opcode::BitRotateRight32 && inst.args[1] == Value::ImmU8(0)
        }));
    }

    #[test]
    fn signed_and_unsigned_accumulate_halfwords_preserve_operand_order_and_ge() {
        for (raw, first, second, reverse_args) in [
            (0xfa21_f283, Opcode::Or32, Opcode::A32GetRegister, false),
            (0xfa31_f283, Opcode::And32, Opcode::A32GetRegister, true),
        ] {
            let (_, block) = translate(raw);
            let packed = block
                .instructions
                .iter()
                .position(|inst| inst.opcode == Opcode::PackedAddU16)
                .expect("packed add");
            assert_eq!(block.instructions[packed - 2].opcode, first);
            assert_eq!(block.instructions[packed - 1].opcode, second);
            let expected_args = if reverse_args {
                [
                    Value::Inst(crate::ir::value::InstRef((packed - 1) as u32)),
                    Value::Inst(crate::ir::value::InstRef((packed - 2) as u32)),
                ]
            } else {
                [
                    Value::Inst(crate::ir::value::InstRef((packed - 2) as u32)),
                    Value::Inst(crate::ir::value::InstRef((packed - 1) as u32)),
                ]
            };
            assert_eq!(block.instructions[packed].args[..2], expected_args);
            assert_eq!(block.instructions[packed + 1].opcode, Opcode::GetGEFromOp);
            assert_eq!(
                block.instructions[packed + 2].opcode,
                Opcode::A32SetRegister
            );
        }
    }
}
