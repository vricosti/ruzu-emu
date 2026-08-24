//! Thumb32 store-single-data-item translation.
//!
//! Upstream owner: `frontend/A32/translate/impl/thumb32_store_single_data_item.cpp`.

use crate::frontend::a32::decoder_thumb32::DecodedThumb32;
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::acc_type::AccType;
use crate::ir::value::Value;

type StoreRegFn = for<'a> fn(&mut A32IREmitter<'a>, Value, Value);
type StoreImmFn = for<'a> fn(&mut A32IREmitter<'a>, Value, Value);

fn store_register(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32, store_fn: StoreRegFn) -> bool {
    let n = inst.rn();
    let t = inst.rt();
    let m = inst.rm();

    if n == Reg::PC {
        return super::undefined_instruction(ir);
    }
    if t == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let reg_t = ir.get_register(t);

    let shift_amount = ((inst.raw >> 4) & 0b11) as u8;
    let offset =
        ir.ir()
            .logical_shift_left_32(reg_m, Value::ImmU8(shift_amount), Value::ImmU1(false));
    let offset_address = ir.ir().add_32(reg_n, offset, Value::ImmU1(false));

    store_fn(ir, offset_address, reg_t);
    true
}

fn store_reg_byte_fn(ir: &mut A32IREmitter<'_>, address: Value, data: Value) {
    let data = ir.ir().least_significant_byte(data);
    ir.write_memory_8(address, data, AccType::Normal);
}

fn store_reg_half_fn(ir: &mut A32IREmitter<'_>, address: Value, data: Value) {
    let data = ir.ir().least_significant_half(data);
    ir.write_memory_16(address, data, AccType::Normal);
}

fn store_reg_word_fn(ir: &mut A32IREmitter<'_>, address: Value, data: Value) {
    ir.write_memory_32(address, data, AccType::Normal);
}

fn store_imm_byte_fn(ir: &mut A32IREmitter<'_>, address: Value, data: Value) {
    let data = ir.ir().least_significant_byte(data);
    ir.write_memory_8(address, data, AccType::Normal);
}

fn store_imm_half_fn(ir: &mut A32IREmitter<'_>, address: Value, data: Value) {
    let data = ir.ir().least_significant_half(data);
    ir.write_memory_16(address, data, AccType::Normal);
}

fn store_imm_word_fn(ir: &mut A32IREmitter<'_>, address: Value, data: Value) {
    ir.write_memory_32(address, data, AccType::Normal);
}

fn store_immediate(
    ir: &mut A32IREmitter<'_>,
    inst: &DecodedThumb32,
    p: bool,
    u: bool,
    w: bool,
    imm32: u32,
    store_fn: StoreImmFn,
) -> bool {
    let reg_n = ir.get_register(inst.rn());
    let reg_t = ir.get_register(inst.rt());

    let offset_address = if u {
        ir.ir()
            .add_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(false))
    } else {
        ir.ir()
            .sub_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(true))
    };
    let address = if p { offset_address } else { reg_n };

    store_fn(ir, address, reg_t);
    if w {
        ir.set_register(inst.rn(), offset_address);
    }

    true
}

pub fn thumb32_strb_imm_1(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if inst.rn() == Reg::PC {
        return super::undefined_instruction(ir);
    }
    if inst.rt() == Reg::PC || inst.rn() == inst.rt() {
        return super::unpredictable_instruction(ir);
    }
    store_immediate(
        ir,
        inst,
        inst.p_flag(),
        inst.u_flag(),
        true,
        inst.imm8(),
        store_imm_byte_fn,
    )
}

pub fn thumb32_strb_imm_2(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if inst.rn() == Reg::PC {
        return super::undefined_instruction(ir);
    }
    if inst.rt() == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    store_immediate(ir, inst, true, false, false, inst.imm8(), store_imm_byte_fn)
}

pub fn thumb32_strb_imm_3(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if inst.rn() == Reg::PC {
        return super::undefined_instruction(ir);
    }
    if inst.rt() == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    store_immediate(
        ir,
        inst,
        true,
        true,
        false,
        inst.raw & 0xfff,
        store_imm_byte_fn,
    )
}

pub fn thumb32_strbt(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if inst.rn() == Reg::PC {
        return super::undefined_instruction(ir);
    }
    if inst.rt() == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    store_immediate(ir, inst, true, true, false, inst.imm8(), store_imm_byte_fn)
}

pub fn thumb32_strb(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    store_register(ir, inst, store_reg_byte_fn)
}

pub fn thumb32_strh_imm_1(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if inst.rn() == Reg::PC {
        return super::undefined_instruction(ir);
    }
    if inst.rt() == Reg::PC || inst.rn() == inst.rt() {
        return super::unpredictable_instruction(ir);
    }
    store_immediate(
        ir,
        inst,
        inst.p_flag(),
        inst.u_flag(),
        true,
        inst.imm8(),
        store_imm_half_fn,
    )
}

pub fn thumb32_strh_imm_2(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if inst.rn() == Reg::PC {
        return super::undefined_instruction(ir);
    }
    if inst.rt() == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    store_immediate(ir, inst, true, false, false, inst.imm8(), store_imm_half_fn)
}

pub fn thumb32_strh_imm_3(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if inst.rn() == Reg::PC {
        return super::undefined_instruction(ir);
    }
    if inst.rt() == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    store_immediate(
        ir,
        inst,
        true,
        true,
        false,
        inst.raw & 0xfff,
        store_imm_half_fn,
    )
}

pub fn thumb32_strht(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if inst.rn() == Reg::PC {
        return super::undefined_instruction(ir);
    }
    if inst.rt() == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    store_immediate(ir, inst, true, true, false, inst.imm8(), store_imm_half_fn)
}

pub fn thumb32_strh(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    store_register(ir, inst, store_reg_half_fn)
}

pub fn thumb32_str_imm_1(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if inst.rn() == Reg::PC {
        return super::undefined_instruction(ir);
    }
    if inst.rt() == Reg::PC || inst.rn() == inst.rt() {
        return super::unpredictable_instruction(ir);
    }
    store_immediate(
        ir,
        inst,
        inst.p_flag(),
        inst.u_flag(),
        true,
        inst.imm8(),
        store_imm_word_fn,
    )
}

pub fn thumb32_str_imm_2(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if inst.rn() == Reg::PC {
        return super::undefined_instruction(ir);
    }
    if inst.rt() == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    store_immediate(ir, inst, true, false, false, inst.imm8(), store_imm_word_fn)
}

pub fn thumb32_str_imm_3(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if inst.rn() == Reg::PC {
        return super::undefined_instruction(ir);
    }
    if inst.rt() == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    store_immediate(
        ir,
        inst,
        true,
        true,
        false,
        inst.raw & 0xfff,
        store_imm_word_fn,
    )
}

pub fn thumb32_strt(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if inst.rn() == Reg::PC {
        return super::undefined_instruction(ir);
    }
    if inst.rt() == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    store_immediate(ir, inst, true, true, false, inst.imm8(), store_imm_word_fn)
}

pub fn thumb32_str_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    store_register(ir, inst, store_reg_word_fn)
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

    fn thumb_location() -> A32LocationDescriptor {
        let mut psr = PSR::default();
        psr.set_t(true);
        A32LocationDescriptor::new(0x1000, psr, FPSCR::default(), false)
    }

    fn decoded(raw: u32, id: Thumb32InstId) -> DecodedThumb32 {
        DecodedThumb32 { raw, id }
    }

    fn assert_exception_without_operands(block: &Block, expected: Exception) {
        assert!(!block.instructions.iter().any(|inst| matches!(
            inst.opcode,
            Opcode::A32GetRegister
                | Opcode::A32WriteMemory8
                | Opcode::A32WriteMemory16
                | Opcode::A32WriteMemory32
        )));
        let exception = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::A32ExceptionRaised)
            .expect("validation exception");
        assert_eq!(exception.args[1], Value::ImmU64(expected.as_u32() as u64));
    }

    #[test]
    fn register_store_preserves_operand_shift_and_width_order() {
        for (raw, id, translate, truncate, write) in [
            (
                0xF801_2034,
                Thumb32InstId::STRB_reg,
                thumb32_strb as fn(&mut A32IREmitter<'_>, &DecodedThumb32) -> bool,
                Some(Opcode::LeastSignificantByte),
                Opcode::A32WriteMemory8,
            ),
            (
                0xF821_2034,
                Thumb32InstId::STRH_reg,
                thumb32_strh,
                Some(Opcode::LeastSignificantHalf),
                Opcode::A32WriteMemory16,
            ),
            (
                0xF841_2034,
                Thumb32InstId::STR_reg,
                thumb32_str_reg,
                None,
                Opcode::A32WriteMemory32,
            ),
        ] {
            let location = thumb_location();
            let mut block = Block::new(location.to_location());
            let inst = decoded(raw, id);
            {
                let mut ir = A32IREmitter::with_location(&mut block, location);
                assert!(translate(&mut ir, &inst));
            }

            assert_eq!(block.instructions[0].args[0], Value::ImmA32Reg(Reg::R4));
            assert_eq!(block.instructions[1].args[0], Value::ImmA32Reg(Reg::R1));
            assert_eq!(block.instructions[2].args[0], Value::ImmA32Reg(Reg::R2));
            assert_eq!(block.instructions[3].opcode, Opcode::LogicalShiftLeft32);
            assert_eq!(block.instructions[4].opcode, Opcode::Add32);
            if let Some(truncate) = truncate {
                assert_eq!(block.instructions[5].opcode, truncate);
                assert_eq!(block.instructions[6].opcode, write);
            } else {
                assert_eq!(block.instructions[5].opcode, write);
            }
        }
    }

    #[test]
    fn immediate_store_writes_memory_before_base_writeback() {
        let location = thumb_location();
        let mut block = Block::new(location.to_location());
        let inst = decoded(0xF841_2B34, Thumb32InstId::STR_imm_1);
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(thumb32_str_imm_1(&mut ir, &inst));
        }

        assert_eq!(block.instructions[0].args[0], Value::ImmA32Reg(Reg::R1));
        assert_eq!(block.instructions[1].args[0], Value::ImmA32Reg(Reg::R2));
        let write = block
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::A32WriteMemory32)
            .expect("word store");
        let writeback = block
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::A32SetRegister)
            .expect("base writeback");
        assert!(write < writeback);
        assert_eq!(
            block.instructions[writeback].args[0],
            Value::ImmA32Reg(Reg::R1)
        );
    }

    #[test]
    fn immediate_families_preserve_fixed_addressing_modes() {
        for (raw, id, translate, address_op, has_writeback) in [
            (
                0xF801_2C34u32,
                Thumb32InstId::STRB_imm_2,
                thumb32_strb_imm_2 as fn(&mut A32IREmitter<'_>, &DecodedThumb32) -> bool,
                Opcode::Sub32,
                false,
            ),
            (
                0xF8A1_2234,
                Thumb32InstId::STRH_imm_3,
                thumb32_strh_imm_3,
                Opcode::Add32,
                false,
            ),
            (
                0xF841_2E34,
                Thumb32InstId::STRT,
                thumb32_strt,
                Opcode::Add32,
                false,
            ),
        ] {
            let location = thumb_location();
            let mut block = Block::new(location.to_location());
            let inst = decode_thumb32((raw >> 16) as u16, raw as u16);
            assert_eq!(inst.id, id);
            {
                let mut ir = A32IREmitter::with_location(&mut block, location);
                assert!(translate(&mut ir, &inst));
            }
            assert!(block
                .instructions
                .iter()
                .any(|inst| inst.opcode == address_op));
            assert_eq!(
                block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == Opcode::A32SetRegister),
                has_writeback
            );
        }
    }

    #[test]
    fn visitor_validation_precedes_all_operand_reads() {
        for (raw, id, translate, expected) in [
            (
                0xF84F_2B34,
                Thumb32InstId::STR_imm_1,
                thumb32_str_imm_1 as fn(&mut A32IREmitter<'_>, &DecodedThumb32) -> bool,
                Exception::UndefinedInstruction,
            ),
            (
                0xF841_FB34,
                Thumb32InstId::STR_imm_1,
                thumb32_str_imm_1,
                Exception::UnpredictableInstruction,
            ),
            (
                0xF842_2B34,
                Thumb32InstId::STR_imm_1,
                thumb32_str_imm_1,
                Exception::UnpredictableInstruction,
            ),
            (
                0xF841_200F,
                Thumb32InstId::STR_reg,
                thumb32_str_reg,
                Exception::UnpredictableInstruction,
            ),
        ] {
            let location = thumb_location();
            let mut block = Block::new(location.to_location());
            let inst = decoded(raw, id);
            {
                let mut ir = A32IREmitter::with_location(&mut block, location);
                assert!(!translate(&mut ir, &inst));
            }
            assert_exception_without_operands(&block, expected);
        }
    }
}
