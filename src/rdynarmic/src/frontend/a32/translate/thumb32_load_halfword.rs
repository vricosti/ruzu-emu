//! Thumb32 halfword-load translation.
//!
//! Upstream owner: `frontend/A32/translate/impl/thumb32_load_halfword.cpp`.

use crate::frontend::a32::decoder_thumb32::DecodedThumb32;
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::acc_type::AccType;
use crate::ir::value::Value;

type ExtensionFunctionU16 = for<'a> fn(&mut A32IREmitter<'a>, Value) -> Value;

fn zero_extend_half_to_word(ir: &mut A32IREmitter<'_>, value: Value) -> Value {
    ir.ir().zero_extend_half_to_word(value)
}

fn sign_extend_half_to_word(ir: &mut A32IREmitter<'_>, value: Value) -> Value {
    ir.ir().sign_extend_half_to_word(value)
}

fn load_half_literal(
    ir: &mut A32IREmitter<'_>,
    inst: &DecodedThumb32,
    ext_fn: ExtensionFunctionU16,
) -> bool {
    let imm32 = inst.raw & 0xfff;
    let base = ir.align_pc(4);
    let address = if (inst.raw >> 23) & 1 != 0 {
        base.wrapping_add(imm32)
    } else {
        base.wrapping_sub(imm32)
    };
    let data = ir.read_memory_16(Value::ImmU32(address), AccType::Normal);
    let data = ext_fn(ir, data);

    ir.set_register(inst.rt(), data);
    true
}

fn load_half_register(
    ir: &mut A32IREmitter<'_>,
    inst: &DecodedThumb32,
    ext_fn: ExtensionFunctionU16,
) -> bool {
    if inst.rm() == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(inst.rm());
    let reg_n = ir.get_register(inst.rn());
    let imm2 = ((inst.raw >> 4) & 0b11) as u8;
    let offset = ir
        .ir()
        .logical_shift_left_32(reg_m, Value::ImmU8(imm2), Value::ImmU1(false));
    let address = ir.ir().add_32(reg_n, offset, Value::ImmU1(false));
    let data = ir.read_memory_16(address, AccType::Normal);
    let data = ext_fn(ir, data);

    ir.set_register(inst.rt(), data);
    true
}

fn load_half_immediate(
    ir: &mut A32IREmitter<'_>,
    inst: &DecodedThumb32,
    p: bool,
    u: bool,
    w: bool,
    imm32: u32,
    ext_fn: ExtensionFunctionU16,
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
    let data = ir.read_memory_16(address, AccType::Normal);
    let data = ext_fn(ir, data);

    if w {
        ir.set_register(inst.rn(), offset_address);
    }
    ir.set_register(inst.rt(), data);
    true
}

pub fn thumb32_ldrh_lit(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    load_half_literal(ir, inst, zero_extend_half_to_word)
}

pub fn thumb32_ldrh_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    load_half_register(ir, inst, zero_extend_half_to_word)
}

pub fn thumb32_ldrh_imm8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();
    if !p && !w {
        return super::undefined_instruction(ir);
    }
    if inst.rt() == Reg::PC && w {
        return super::unpredictable_instruction(ir);
    }
    if w && inst.rn() == inst.rt() {
        return super::unpredictable_instruction(ir);
    }

    load_half_immediate(ir, inst, p, u, w, inst.imm8(), zero_extend_half_to_word)
}

pub fn thumb32_ldrh_imm12(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    load_half_immediate(
        ir,
        inst,
        true,
        true,
        false,
        inst.raw & 0xfff,
        zero_extend_half_to_word,
    )
}

pub fn thumb32_ldrht(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if inst.rt() == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    thumb32_ldrh_imm8(ir, inst)
}

pub fn thumb32_ldrsh_lit(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    load_half_literal(ir, inst, sign_extend_half_to_word)
}

pub fn thumb32_ldrsh_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    load_half_register(ir, inst, sign_extend_half_to_word)
}

pub fn thumb32_ldrsh_imm8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();
    if !p && !w {
        return super::undefined_instruction(ir);
    }
    if inst.rt() == Reg::PC && w {
        return super::unpredictable_instruction(ir);
    }
    if w && inst.rn() == inst.rt() {
        return super::unpredictable_instruction(ir);
    }

    load_half_immediate(ir, inst, p, u, w, inst.imm8(), sign_extend_half_to_word)
}

pub fn thumb32_ldrsh_imm12(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    load_half_immediate(
        ir,
        inst,
        true,
        true,
        false,
        inst.raw & 0xfff,
        sign_extend_half_to_word,
    )
}

pub fn thumb32_ldrsht(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if inst.rt() == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    thumb32_ldrsh_imm8(ir, inst)
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
        let positive = decode_thumb32(0xF8BF, 0x1004);
        assert_eq!(positive.id, Thumb32InstId::LdrhLit);
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(thumb32_ldrh_lit(&mut ir, &positive));
        }
        let read = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::A32ReadMemory16)
            .expect("literal halfword read");
        assert_eq!(read.args[1], Value::ImmU32(0x1008));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::ZeroExtendHalfToWord));

        let location = thumb_location(0x1002);
        let mut block = Block::new(location.to_location());
        let negative = decode_thumb32(0xF93F, 0x2204);
        assert_eq!(negative.id, Thumb32InstId::LdrshLit);
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(thumb32_ldrsh_lit(&mut ir, &negative));
        }
        let read = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::A32ReadMemory16)
            .expect("signed literal halfword read");
        assert_eq!(read.args[1], Value::ImmU32(0x0e00));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::SignExtendHalfToWord));
    }

    #[test]
    fn register_load_preserves_rm_then_rn_order_and_zero_shift() {
        let location = thumb_location(0x1000);
        let mut block = Block::new(location.to_location());
        let inst = decoded(0xF831_2003, Thumb32InstId::LdrhReg);
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(thumb32_ldrh_reg(&mut ir, &inst));
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
                Opcode::A32GetRegister,
                Opcode::LogicalShiftLeft32,
                Opcode::Add32,
                Opcode::A32ReadMemory16,
                Opcode::ZeroExtendHalfToWord,
                Opcode::A32SetRegister,
            ]
        );
        assert_eq!(block.instructions[0].args[0], Value::ImmA32Reg(Reg::R3));
        assert_eq!(block.instructions[1].args[0], Value::ImmA32Reg(Reg::R1));
    }

    #[test]
    fn immediate_writeback_precedes_destination_write() {
        let location = thumb_location(0x1000);
        let mut block = Block::new(location.to_location());
        let inst = decoded(0xF831_2B04, Thumb32InstId::LdrhImmT3);
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(thumb32_ldrh_imm8(&mut ir, &inst));
        }

        let sets = block
            .instructions
            .iter()
            .filter(|inst| inst.opcode == Opcode::A32SetRegister)
            .collect::<Vec<_>>();
        assert_eq!(sets.len(), 2);
        assert_eq!(sets[0].args[0], Value::ImmA32Reg(Reg::R1));
        assert_eq!(sets[1].args[0], Value::ImmA32Reg(Reg::R2));
    }

    #[test]
    fn immediate_validation_precedes_ir_side_effects() {
        for (raw, expected) in [
            (0xF831_2A04u32, Exception::UndefinedInstruction),
            (0xF831_FB04, Exception::UnpredictableInstruction),
            (0xF832_2B04, Exception::UnpredictableInstruction),
        ] {
            let location = thumb_location(0x1000);
            let mut block = Block::new(location.to_location());
            let inst = decoded(raw, Thumb32InstId::LdrhImmT3);
            {
                let mut ir = A32IREmitter::with_location(&mut block, location);
                assert!(!thumb32_ldrh_imm8(&mut ir, &inst));
            }
            assert!(!block.instructions.iter().any(|inst| matches!(
                inst.opcode,
                Opcode::A32GetRegister | Opcode::A32ReadMemory16
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
    fn unprivileged_load_has_no_writeback() {
        let location = thumb_location(0x1000);
        let mut block = Block::new(location.to_location());
        let inst = decoded(0xF831_2E04, Thumb32InstId::LDRHT);
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(thumb32_ldrht(&mut ir, &inst));
        }

        let sets = block
            .instructions
            .iter()
            .filter(|inst| inst.opcode == Opcode::A32SetRegister)
            .collect::<Vec<_>>();
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].args[0], Value::ImmA32Reg(Reg::R2));
    }
}
