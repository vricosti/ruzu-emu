//! Thumb32 branch translation.
//!
//! Upstream owner: `frontend/A32/translate/impl/thumb32_branch.cpp`.

use super::helpers::it_block_check;
use crate::frontend::a32::decoder_thumb32::DecodedThumb32;
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

pub fn thumb32_bl_imm(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if it_block_check(ir) {
        return super::unpredictable_instruction(ir);
    }

    let location = ir.current_location.expect("current_location not set");
    ir.base
        .push_rsb(location.advance_pc(4).advance_it().to_location());
    ir.set_register(Reg::LR, Value::ImmU32(location.pc().wrapping_add(4) | 1));

    let imm32 = inst.branch_offset_t4().wrapping_add(4);
    let new_location = location.advance_pc(imm32).advance_it();
    ir.set_term(Terminal::link_block(new_location.to_location()));
    false
}

pub fn thumb32_blx_imm(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if it_block_check(ir) {
        return super::unpredictable_instruction(ir);
    }
    if inst.raw & 1 != 0 {
        return super::unpredictable_instruction(ir);
    }

    let location = ir.current_location.expect("current_location not set");
    ir.base
        .push_rsb(location.advance_pc(4).advance_it().to_location());
    ir.set_register(Reg::LR, Value::ImmU32(location.pc().wrapping_add(4) | 1));

    let imm32 = inst.branch_offset_t4();
    let new_location = location
        .set_pc((ir.align_pc(4) as i32).wrapping_add(imm32) as u32)
        .set_t_flag(false)
        .advance_it();
    ir.set_term(Terminal::link_block(new_location.to_location()));
    false
}

pub fn thumb32_b(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    if it_block_check(ir) {
        return super::unpredictable_instruction(ir);
    }

    let location = ir.current_location.expect("current_location not set");
    let imm32 = inst.branch_offset_t4().wrapping_add(4);
    let new_location = location.advance_pc(imm32).advance_it();
    ir.set_term(Terminal::link_block(new_location.to_location()));
    false
}

pub fn thumb32_b_cond(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let location = ir.current_location.expect("current_location not set");
    if location.it().is_in_it_block() {
        return super::unpredictable_instruction(ir);
    }

    let imm32 = inst.branch_offset_t3().wrapping_add(4);
    let then_location = location.advance_pc(imm32).advance_it();
    let else_location = location.advance_pc(4).advance_it();
    ir.set_term(Terminal::if_then_else(
        inst.cond(),
        Terminal::link_block(then_location.to_location()),
        Terminal::link_block(else_location.to_location()),
    ));
    false
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

    fn location(pc: u32, it: u8) -> A32LocationDescriptor {
        let mut psr = PSR::default();
        psr.set_t(true);
        A32LocationDescriptor::new(pc, psr, FPSCR::default(), false).set_it(ITState::new(it))
    }

    fn translate(raw: u32, location: A32LocationDescriptor) -> Block {
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
        block
    }

    fn exception(block: &Block) -> Option<u64> {
        block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::A32ExceptionRaised)
            .and_then(|inst| match inst.args[1] {
                Value::ImmU64(value) => Some(value),
                _ => None,
            })
    }

    #[test]
    fn all_four_upstream_branch_patterns_translate() {
        for (raw, expected) in [
            (0xF000_D000, Thumb32InstId::BL_imm),
            (0xF000_C000, Thumb32InstId::BLX_imm),
            (0xF000_9000, Thumb32InstId::B),
            (0xF000_8000, Thumb32InstId::B_cond),
        ] {
            let inst = decode_thumb32((raw >> 16) as u16, raw as u16);
            assert_eq!(inst.id, expected);
            let _ = translate(raw, location(0x1000, 0));
        }
    }

    #[test]
    fn branch_it_validation_precedes_branch_side_effects() {
        for (raw, it) in [
            (0xF000_F800, 0x0c),
            (0xF000_E800, 0x0c),
            (0xF000_B800, 0x0c),
            (0xF000_8000, 0x08),
        ] {
            let block = translate(raw, location(0x1000, it));
            assert_eq!(
                exception(&block),
                Some(Exception::UnpredictableInstruction.as_u32() as u64)
            );
            assert!(!block
                .instructions
                .iter()
                .any(|inst| inst.opcode == Opcode::PushRSB));
            assert!(!block.instructions.iter().any(|inst| {
                inst.opcode == Opcode::A32SetRegister && inst.args[0] == Value::ImmA32Reg(Reg::LR)
            }));
        }
    }

    #[test]
    fn blx_rejects_an_odd_low_field_before_link_side_effects() {
        let block = translate(0xF000_E801, location(0x1000, 0));
        assert_eq!(
            exception(&block),
            Some(Exception::UnpredictableInstruction.as_u32() as u64)
        );
        assert!(!block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::PushRSB));
        assert!(!block.instructions.iter().any(|inst| {
            inst.opcode == Opcode::A32SetRegister && inst.args[0] == Value::ImmA32Reg(Reg::LR)
        }));
    }

    #[test]
    fn bl_preserves_rsb_lr_and_target_order() {
        let block = translate(0xF000_F800, location(0x1000, 0));
        let opcodes = block
            .instructions
            .iter()
            .map(|inst| inst.opcode)
            .collect::<Vec<_>>();
        assert_eq!(opcodes, vec![Opcode::PushRSB, Opcode::A32SetRegister]);
        assert_eq!(block.instructions[1].args[0], Value::ImmA32Reg(Reg::LR));
        assert_eq!(block.instructions[1].args[1], Value::ImmU32(0x1005));
        let Terminal::LinkBlock { next } = block.terminal else {
            panic!("expected LinkBlock terminal");
        };
        assert_eq!(A32LocationDescriptor::from_location(next).pc(), 0x1004);
    }

    #[test]
    fn blx_uses_aligned_thumb_pc_base_and_switches_to_arm() {
        let block = translate(0xF000_E800, location(0x1002, 0));
        let Terminal::LinkBlock { next } = block.terminal else {
            panic!("expected LinkBlock terminal");
        };
        let next = A32LocationDescriptor::from_location(next);
        assert_eq!(next.pc(), 0x1004);
        assert!(!next.t_flag());
    }

    #[test]
    fn conditional_branch_preserves_then_and_else_locations() {
        let block = translate(0xF000_8000, location(0x1000, 0));
        let Terminal::If { then_, else_, .. } = block.terminal else {
            panic!("expected conditional terminal");
        };
        let Terminal::LinkBlock { next: then_next } = *then_ else {
            panic!("expected linked then path");
        };
        let Terminal::LinkBlock { next: else_next } = *else_ else {
            panic!("expected linked else path");
        };
        assert_eq!(A32LocationDescriptor::from_location(then_next).pc(), 0x1004);
        assert_eq!(A32LocationDescriptor::from_location(else_next).pc(), 0x1004);
    }
}
