//! Thumb32 byte-load and memory-hint translation.
//!
//! Upstream owner: `frontend/A32/translate/impl/thumb32_load_byte.cpp`.

use crate::frontend::a32::decoder_thumb32::DecodedThumb32;
use crate::frontend::a32::types::{Exception, Reg};
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::acc_type::AccType;
use crate::ir::value::Value;

use super::TranslationOptions;

type ExtensionFunctionU8 = for<'a> fn(&mut A32IREmitter<'a>, Value) -> Value;

fn zero_extend_byte_to_word(ir: &mut A32IREmitter<'_>, value: Value) -> Value {
    ir.ir().zero_extend_byte_to_word(value)
}

fn sign_extend_byte_to_word(ir: &mut A32IREmitter<'_>, value: Value) -> Value {
    ir.ir().sign_extend_byte_to_word(value)
}

fn pld_handler(ir: &mut A32IREmitter<'_>, write_intent: bool, options: TranslationOptions) -> bool {
    if !options.hook_hint_instructions {
        return true;
    }

    let exception = if write_intent {
        Exception::PreloadDataWithIntentToWrite
    } else {
        Exception::PreloadData
    };
    super::raise_exception(ir, exception)
}

fn pli_handler(ir: &mut A32IREmitter<'_>, options: TranslationOptions) -> bool {
    if !options.hook_hint_instructions {
        return true;
    }

    super::raise_exception(ir, Exception::PreloadInstruction)
}

fn load_byte_literal(
    ir: &mut A32IREmitter<'_>,
    inst: &DecodedThumb32,
    ext_fn: ExtensionFunctionU8,
) -> bool {
    let imm32 = inst.raw & 0xfff;
    let base = ir.align_pc(4);
    let address = if (inst.raw >> 23) & 1 != 0 {
        base.wrapping_add(imm32)
    } else {
        base.wrapping_sub(imm32)
    };
    let data = ir.read_memory_8(Value::ImmU32(address), AccType::Normal);
    let data = ext_fn(ir, data);

    ir.set_register(inst.rt(), data);
    true
}

fn load_byte_register(
    ir: &mut A32IREmitter<'_>,
    inst: &DecodedThumb32,
    ext_fn: ExtensionFunctionU8,
) -> bool {
    if inst.rm() == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_n = ir.get_register(inst.rn());
    let reg_m = ir.get_register(inst.rm());
    let imm2 = ((inst.raw >> 4) & 0b11) as u8;
    let offset = ir
        .ir()
        .logical_shift_left_32(reg_m, Value::ImmU8(imm2), Value::ImmU1(false));
    let address = ir.ir().add_32(reg_n, offset, Value::ImmU1(false));
    let data = ir.read_memory_8(address, AccType::Normal);
    let data = ext_fn(ir, data);

    ir.set_register(inst.rt(), data);
    true
}

fn load_byte_immediate(
    ir: &mut A32IREmitter<'_>,
    inst: &DecodedThumb32,
    p: bool,
    u: bool,
    w: bool,
    imm32: u32,
    ext_fn: ExtensionFunctionU8,
) -> bool {
    let reg_n = ir.get_register(inst.rn());
    let offset_address = if u {
        ir.ir()
            .add_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(false))
    } else {
        ir.ir()
            .sub_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(true))
    };
    let address = if p { offset_address } else { reg_n };
    let data = ir.read_memory_8(address, AccType::Normal);
    let data = ext_fn(ir, data);

    ir.set_register(inst.rt(), data);
    if w {
        ir.set_register(inst.rn(), offset_address);
    }
    true
}

pub fn thumb32_pld_lit(
    ir: &mut A32IREmitter<'_>,
    _inst: &DecodedThumb32,
    options: TranslationOptions,
) -> bool {
    pld_handler(ir, false, options)
}

pub fn thumb32_pld_imm8(
    ir: &mut A32IREmitter<'_>,
    inst: &DecodedThumb32,
    options: TranslationOptions,
) -> bool {
    pld_handler(ir, (inst.raw >> 21) & 1 != 0, options)
}

pub fn thumb32_pld_imm12(
    ir: &mut A32IREmitter<'_>,
    inst: &DecodedThumb32,
    options: TranslationOptions,
) -> bool {
    pld_handler(ir, (inst.raw >> 21) & 1 != 0, options)
}

pub fn thumb32_pld_reg(
    ir: &mut A32IREmitter<'_>,
    inst: &DecodedThumb32,
    options: TranslationOptions,
) -> bool {
    if inst.rm() == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    pld_handler(ir, (inst.raw >> 21) & 1 != 0, options)
}

pub fn thumb32_pli_lit(
    ir: &mut A32IREmitter<'_>,
    _inst: &DecodedThumb32,
    options: TranslationOptions,
) -> bool {
    pli_handler(ir, options)
}

pub fn thumb32_pli_imm8(
    ir: &mut A32IREmitter<'_>,
    _inst: &DecodedThumb32,
    options: TranslationOptions,
) -> bool {
    pli_handler(ir, options)
}

pub fn thumb32_pli_imm12(
    ir: &mut A32IREmitter<'_>,
    _inst: &DecodedThumb32,
    options: TranslationOptions,
) -> bool {
    pli_handler(ir, options)
}

pub fn thumb32_pli_reg(
    ir: &mut A32IREmitter<'_>,
    inst: &DecodedThumb32,
    options: TranslationOptions,
) -> bool {
    if inst.rm() == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    pli_handler(ir, options)
}

pub fn thumb32_ldrb_lit(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    load_byte_literal(ir, inst, zero_extend_byte_to_word)
}

pub fn thumb32_ldrb_imm8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();
    if inst.rt() == Reg::PC && w {
        return super::unpredictable_instruction(ir);
    }
    if w && inst.rn() == inst.rt() {
        return super::unpredictable_instruction(ir);
    }
    if !p && !w {
        return super::undefined_instruction(ir);
    }

    load_byte_immediate(ir, inst, p, u, w, inst.imm8(), zero_extend_byte_to_word)
}

pub fn thumb32_ldrb_imm12(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    load_byte_immediate(
        ir,
        inst,
        true,
        true,
        false,
        inst.raw & 0xfff,
        zero_extend_byte_to_word,
    )
}

pub fn thumb32_ldrb_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    load_byte_register(ir, inst, zero_extend_byte_to_word)
}

pub fn thumb32_ldrbt(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if inst.rt() == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    thumb32_ldrb_imm8(ir, inst)
}

pub fn thumb32_ldrsb_lit(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    load_byte_literal(ir, inst, sign_extend_byte_to_word)
}

pub fn thumb32_ldrsb_imm8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();
    if inst.rt() == Reg::PC && w {
        return super::unpredictable_instruction(ir);
    }
    if w && inst.rn() == inst.rt() {
        return super::unpredictable_instruction(ir);
    }
    if !p && !w {
        return super::undefined_instruction(ir);
    }

    load_byte_immediate(ir, inst, p, u, w, inst.imm8(), sign_extend_byte_to_word)
}

pub fn thumb32_ldrsb_imm12(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    load_byte_immediate(
        ir,
        inst,
        true,
        true,
        false,
        inst.raw & 0xfff,
        sign_extend_byte_to_word,
    )
}

pub fn thumb32_ldrsb_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    load_byte_register(ir, inst, sign_extend_byte_to_word)
}

pub fn thumb32_ldrsbt(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if inst.rt() == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    thumb32_ldrsb_imm8(ir, inst)
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

    fn thumb_location(pc: u32) -> A32LocationDescriptor {
        let mut psr = PSR::default();
        psr.set_t(true);
        A32LocationDescriptor::new(pc, psr, FPSCR::default(), false)
    }

    fn decoded(raw: u32, id: Thumb32InstId) -> DecodedThumb32 {
        DecodedThumb32 { raw, id }
    }

    #[test]
    fn literal_load_uses_hw1_u_bit_and_matching_extension() {
        let location = thumb_location(0x1002);
        let mut block = Block::new(location.to_location());
        let positive = decode_thumb32(0xF89F, 0x1004);
        assert_eq!(positive.id, Thumb32InstId::LdrbLit);
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(thumb32_ldrb_lit(&mut ir, &positive));
        }

        let read = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::A32ReadMemory8)
            .expect("literal byte read");
        assert_eq!(read.args[1], Value::ImmU32(0x1008));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::ZeroExtendByteToWord));

        let location = thumb_location(0x1002);
        let mut block = Block::new(location.to_location());
        let negative = decode_thumb32(0xF91F, 0x2204);
        assert_eq!(negative.id, Thumb32InstId::LdrsbLit);
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(thumb32_ldrsb_lit(&mut ir, &negative));
        }
        let read = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::A32ReadMemory8)
            .expect("signed literal byte read");
        assert_eq!(read.args[1], Value::ImmU32(0x0e00));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::SignExtendByteToWord));
    }

    #[test]
    fn immediate_writeback_occurs_after_destination_write() {
        let location = thumb_location(0x1000);
        let mut block = Block::new(location.to_location());
        let inst = decoded(0xF811_2B04, Thumb32InstId::LdrbImmT3);
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(thumb32_ldrb_imm8(&mut ir, &inst));
        }

        let opcodes = block
            .instructions
            .iter()
            .map(|inst| inst.opcode)
            .collect::<Vec<_>>();
        assert_eq!(
            opcodes,
            vec![
                Opcode::A32GetRegister,
                Opcode::Add32,
                Opcode::A32ReadMemory8,
                Opcode::ZeroExtendByteToWord,
                Opcode::A32SetRegister,
                Opcode::A32SetRegister,
            ]
        );
        assert_eq!(block.instructions[4].args[0], Value::ImmA32Reg(Reg::R2));
        assert_eq!(block.instructions[5].args[0], Value::ImmA32Reg(Reg::R1));
    }

    #[test]
    fn immediate_validation_precedes_ir_side_effects() {
        for (raw, expected) in [
            (0xF811_2A04u32, Exception::UndefinedInstruction),
            (0xF811_FB04, Exception::UnpredictableInstruction),
            (0xF812_2B04, Exception::UnpredictableInstruction),
        ] {
            let location = thumb_location(0x1000);
            let mut block = Block::new(location.to_location());
            let inst = decoded(raw, Thumb32InstId::LdrbImmT3);
            {
                let mut ir = A32IREmitter::with_location(&mut block, location);
                assert!(!thumb32_ldrb_imm8(&mut ir, &inst));
            }

            assert!(!block.instructions.iter().any(|inst| matches!(
                inst.opcode,
                Opcode::A32GetRegister | Opcode::A32ReadMemory8
            )));
            let exception = block
                .instructions
                .iter()
                .find(|inst| inst.opcode == Opcode::A32ExceptionRaised)
                .expect("validation exception");
            assert_eq!(exception.args[1], Value::ImmU64(expected.as_u32() as u64));
        }
    }

    #[test]
    fn register_and_preload_pc_checks_run_before_hook_or_load() {
        let location = thumb_location(0x1000);
        for (inst, preload) in [
            (decoded(0xF811_200F, Thumb32InstId::LdrbReg), false),
            (decoded(0xF811_F00F, Thumb32InstId::PldReg), true),
        ] {
            let mut block = Block::new(location.to_location());
            {
                let mut ir = A32IREmitter::with_location(&mut block, location);
                let result = if preload {
                    thumb32_pld_reg(
                        &mut ir,
                        &inst,
                        TranslationOptions {
                            hook_hint_instructions: false,
                            ..TranslationOptions::default()
                        },
                    )
                } else {
                    thumb32_ldrb_reg(&mut ir, &inst)
                };
                assert!(!result);
            }
            assert!(!block.instructions.iter().any(|inst| matches!(
                inst.opcode,
                Opcode::A32GetRegister | Opcode::A32ReadMemory8
            )));
            assert!(block
                .instructions
                .iter()
                .any(|inst| inst.opcode == Opcode::A32ExceptionRaised));
        }
    }

    #[test]
    fn unprivileged_load_uses_normal_positive_offset_without_writeback() {
        let location = thumb_location(0x1000);
        let mut block = Block::new(location.to_location());
        let inst = decoded(0xF811_2E04, Thumb32InstId::LDRBT);
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(thumb32_ldrbt(&mut ir, &inst));
        }

        let sets = block
            .instructions
            .iter()
            .filter(|inst| inst.opcode == Opcode::A32SetRegister)
            .collect::<Vec<_>>();
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].args[0], Value::ImmA32Reg(Reg::R2));
        let read = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::A32ReadMemory8)
            .expect("unprivileged byte read");
        assert_eq!(read.args[2], Value::ImmAccType(AccType::Normal));
    }
}
