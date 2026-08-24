//! Thumb32 word-load translation.
//!
//! Upstream owner: `frontend/A32/translate/impl/thumb32_load_word.cpp`.

use super::helpers::it_block_check;
use crate::frontend::a32::decoder_thumb32::DecodedThumb32;
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::acc_type::AccType;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

pub fn thumb32_ldr_lit(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let t = inst.rt();
    if t == Reg::PC && it_block_check(ir) {
        return super::unpredictable_instruction(ir);
    }

    let imm32 = inst.raw & 0xfff;
    let base = ir.align_pc(4);
    let address = if (inst.raw >> 23) & 1 != 0 {
        base.wrapping_add(imm32)
    } else {
        base.wrapping_sub(imm32)
    };
    let data = ir.read_memory_32(Value::ImmU32(address), AccType::Normal);

    if t == Reg::PC {
        ir.update_upper_location_descriptor();
        ir.load_write_pc(data);
        ir.set_term(Terminal::FastDispatchHint);
        return false;
    }

    ir.set_register(t, data);
    true
}

pub fn thumb32_ldr_imm8(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let t = inst.rt();
    let p = inst.p_flag();
    let u = inst.u_flag();
    let w = inst.w_flag();

    if !p && !w {
        return super::undefined_instruction(ir);
    }
    if w && n == t {
        return super::unpredictable_instruction(ir);
    }
    if t == Reg::PC && it_block_check(ir) {
        return super::unpredictable_instruction(ir);
    }

    let imm32 = inst.imm8();
    let reg_n = ir.get_register(n);
    let offset_address = if u {
        ir.ir()
            .add_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(false))
    } else {
        ir.ir()
            .sub_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(true))
    };
    let address = if p { offset_address } else { reg_n };
    let data = ir.read_memory_32(address, AccType::Normal);

    if w {
        ir.set_register(n, offset_address);
    }

    if t == Reg::PC {
        ir.update_upper_location_descriptor();
        ir.load_write_pc(data);

        if !p && w && n == Reg::R13 {
            ir.set_term(Terminal::PopRSBHint);
        } else {
            ir.set_term(Terminal::FastDispatchHint);
        }

        return false;
    }

    ir.set_register(t, data);
    true
}

pub fn thumb32_ldr_imm12(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let t = inst.rt();
    if t == Reg::PC && it_block_check(ir) {
        return super::unpredictable_instruction(ir);
    }

    let imm32 = inst.raw & 0xfff;
    let reg_n = ir.get_register(inst.rn());
    let address = ir
        .ir()
        .add_32(reg_n, Value::ImmU32(imm32), Value::ImmU1(false));
    let data = ir.read_memory_32(address, AccType::Normal);

    if t == Reg::PC {
        ir.update_upper_location_descriptor();
        ir.load_write_pc(data);
        ir.set_term(Terminal::FastDispatchHint);
        return false;
    }

    ir.set_register(t, data);
    true
}

pub fn thumb32_ldr_reg(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let t = inst.rt();
    let m = inst.rm();

    if m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    if t == Reg::PC && it_block_check(ir) {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let imm2 = ((inst.raw >> 4) & 0b11) as u8;
    let offset = ir
        .ir()
        .logical_shift_left_32(reg_m, Value::ImmU8(imm2), Value::ImmU1(false));
    let address = ir.ir().add_32(reg_n, offset, Value::ImmU1(false));
    let data = ir.read_memory_32(address, AccType::Normal);

    if t == Reg::PC {
        ir.update_upper_location_descriptor();
        ir.load_write_pc(data);
        ir.set_term(Terminal::FastDispatchHint);
        return false;
    }

    ir.set_register(t, data);
    true
}

pub fn thumb32_ldrt(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if inst.rt() == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    thumb32_ldr_imm8(ir, inst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder_thumb32::{decode_thumb32, Thumb32InstId};
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::it_state::ITState;
    use crate::frontend::a32::psr::PSR;
    use crate::frontend::a32::types::Exception;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;

    fn thumb_location(pc: u32, it: u8) -> A32LocationDescriptor {
        let mut psr = PSR::default();
        psr.set_t(true);
        A32LocationDescriptor::new(pc, psr, FPSCR::default(), false).set_it(ITState::new(it))
    }

    fn decoded(raw: u32, id: Thumb32InstId) -> DecodedThumb32 {
        DecodedThumb32 { raw, id }
    }

    fn assert_exception_without_memory(block: &Block, expected: Exception) {
        assert!(!block.instructions.iter().any(|inst| matches!(
            inst.opcode,
            Opcode::A32GetRegister | Opcode::A32ReadMemory32
        )));
        let exception = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::A32ExceptionRaised)
            .expect("validation exception");
        assert_eq!(exception.args[1], Value::ImmU64(expected.as_u32() as u64));
    }

    #[test]
    fn literal_load_uses_u_bit_and_pc_path() {
        let location = thumb_location(0x1002, 0);
        let mut block = Block::new(location.to_location());
        let inst = decode_thumb32(0xF8DF, 0x2004);
        assert_eq!(inst.id, Thumb32InstId::LDR_lit);
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(thumb32_ldr_lit(&mut ir, &inst));
        }
        let read = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::A32ReadMemory32)
            .expect("literal word read");
        assert_eq!(read.args[1], Value::ImmU32(0x1008));

        let location = thumb_location(0x1002, 0);
        let mut block = Block::new(location.to_location());
        let inst = decode_thumb32(0xF85F, 0xF004);
        assert_eq!(inst.id, Thumb32InstId::LDR_lit);
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(!thumb32_ldr_lit(&mut ir, &inst));
        }
        assert!(matches!(block.terminal, Terminal::FastDispatchHint));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32BXWritePC));
    }

    #[test]
    fn pc_load_is_rejected_before_ir_in_nonfinal_it_position() {
        for (raw, id, translate) in [
            (
                0xF8DF_F004,
                Thumb32InstId::LDR_lit,
                thumb32_ldr_lit as fn(&mut A32IREmitter<'_>, &DecodedThumb32) -> bool,
            ),
            (0xF8D1_F004, Thumb32InstId::LDR_imm_t3, thumb32_ldr_imm12),
            (0xF851_F003, Thumb32InstId::LDR_reg, thumb32_ldr_reg),
        ] {
            let location = thumb_location(0x1000, 0x0c);
            let mut block = Block::new(location.to_location());
            let inst = decoded(raw, id);
            {
                let mut ir = A32IREmitter::with_location(&mut block, location);
                assert!(!translate(&mut ir, &inst));
            }
            assert_exception_without_memory(&block, Exception::UnpredictableInstruction);
        }
    }

    #[test]
    fn register_load_preserves_rm_then_rn_order_and_zero_shift() {
        let location = thumb_location(0x1000, 0);
        let mut block = Block::new(location.to_location());
        let inst = decoded(0xF851_2003, Thumb32InstId::LDR_reg);
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(thumb32_ldr_reg(&mut ir, &inst));
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
                Opcode::A32ReadMemory32,
                Opcode::A32SetRegister,
            ]
        );
        assert_eq!(block.instructions[0].args[0], Value::ImmA32Reg(Reg::R3));
        assert_eq!(block.instructions[1].args[0], Value::ImmA32Reg(Reg::R1));
    }

    #[test]
    fn imm8_validation_order_precedes_ir_side_effects() {
        for (raw, expected) in [
            (0xF851_2804u32, Exception::UndefinedInstruction),
            (0xF852_2B04, Exception::UnpredictableInstruction),
            (0xF851_FB04, Exception::UnpredictableInstruction),
        ] {
            let location = thumb_location(0x1000, if raw >> 12 & 0xf == 0xf { 0x0c } else { 0 });
            let mut block = Block::new(location.to_location());
            let inst = decoded(raw, Thumb32InstId::LDR_imm_t4);
            {
                let mut ir = A32IREmitter::with_location(&mut block, location);
                assert!(!thumb32_ldr_imm8(&mut ir, &inst));
            }
            assert_exception_without_memory(&block, expected);
        }
    }

    #[test]
    fn pop_load_writes_back_before_pc_and_sets_rsb_hint() {
        let location = thumb_location(0x1000, 0);
        let mut block = Block::new(location.to_location());
        let inst = decoded(0xF85D_FB04, Thumb32InstId::LDR_imm_t4);
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(!thumb32_ldr_imm8(&mut ir, &inst));
        }

        let writeback = block
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::A32SetRegister)
            .expect("SP writeback");
        let load_pc = block
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::A32BXWritePC)
            .expect("PC load");
        assert!(writeback < load_pc);
        assert_eq!(
            block.instructions[writeback].args[0],
            Value::ImmA32Reg(Reg::R13)
        );
        assert!(matches!(block.terminal, Terminal::PopRSBHint));
    }

    #[test]
    fn ldrt_reuses_positive_preindexed_nonwriteback_path() {
        let location = thumb_location(0x1000, 0);
        let mut block = Block::new(location.to_location());
        let inst = decode_thumb32(0xF851, 0x2E04);
        assert_eq!(inst.id, Thumb32InstId::LDRT);
        {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            assert!(thumb32_ldrt(&mut ir, &inst));
        }
        assert_eq!(
            block
                .instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::A32SetRegister)
                .count(),
            1
        );
    }
}
