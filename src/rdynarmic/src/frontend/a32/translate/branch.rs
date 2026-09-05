use crate::frontend::a32::decoder::{sign_extend, DecodedArm};
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

/// ARM B (branch).
pub fn arm_b(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let pc = ir.pc(); // current PC + 8
    let imm24 = inst.imm24();
    let offset = sign_extend(imm24 << 2, 26);
    let target = pc.wrapping_add(offset);

    let loc = ir.current_location.expect("location not set");
    let next = loc.set_pc(target);
    ir.set_term(Terminal::link_block(next.to_location()));
    false
}

/// ARM BL (branch with link).
pub fn arm_bl(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let pc = ir.pc(); // current PC + 8
    let imm24 = inst.imm24();
    let offset = sign_extend(imm24 << 2, 26);
    let target = pc.wrapping_add(offset);

    // LR = address of next instruction
    let loc = ir.current_location.expect("location not set");
    let return_addr = loc.pc().wrapping_add(4);
    ir.base.push_rsb(loc.advance_pc(4).to_location());
    ir.set_register(
        crate::frontend::a32::types::Reg::R14,
        Value::ImmU32(return_addr),
    );

    let next = loc.set_pc(target);
    ir.set_term(Terminal::link_block(next.to_location()));
    false
}

/// ARM BX (branch and exchange).
pub fn arm_bx(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rm = inst.rm();
    let target = ir.get_register(rm);
    ir.bx_write_pc(target);
    let inner = if rm == crate::frontend::a32::types::Reg::R14 {
        Terminal::PopRSBHint
    } else {
        Terminal::FastDispatchHint
    };
    ir.set_term(Terminal::CheckHalt {
        else_: Box::new(inner),
    });
    false
}

/// ARM BLX (register).
pub fn arm_blx_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rm = inst.rm();
    let target = ir.get_register(rm);

    // Matches upstream ordering:
    //   PushRSB
    //   BXWritePC(GetRegister(m))
    //   SetRegister(LR, return_addr)
    //
    // This matters when rm == LR: the branch target must observe the old LR,
    // not the freshly written return address.
    let loc = ir.current_location.expect("location not set");
    let return_addr = loc.pc().wrapping_add(4);
    ir.base.push_rsb(loc.advance_pc(4).to_location());
    ir.bx_write_pc(target);
    ir.set_register(
        crate::frontend::a32::types::Reg::R14,
        Value::ImmU32(return_addr),
    );
    ir.set_term(Terminal::CheckHalt {
        else_: Box::new(Terminal::FastDispatchHint),
    });
    false
}

/// ARM BLX (immediate) - switch to Thumb.
pub fn arm_blx_imm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let pc = ir.pc(); // current PC + 8
    let imm24 = inst.imm24();
    let h = if inst.h_flag() { 2u32 } else { 0u32 };
    let offset = sign_extend(imm24 << 2, 26).wrapping_add(h);
    let target = pc.wrapping_add(offset);

    // LR = address of next instruction
    let loc = ir.current_location.expect("location not set");
    let return_addr = loc.pc().wrapping_add(4);
    ir.base.push_rsb(loc.advance_pc(4).to_location());
    ir.set_register(
        crate::frontend::a32::types::Reg::R14,
        Value::ImmU32(return_addr),
    );

    // Switch to Thumb mode - target bit 0 determines T flag
    let next = loc.set_pc(target).set_t_flag(true);
    ir.set_term(Terminal::link_block(next.to_location()));
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder::ArmInstId;
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::frontend::a32::types::Reg;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::terminal::Terminal;

    #[test]
    fn arm_blx_reg_writes_pc_before_link_register() {
        let loc = A32LocationDescriptor::new(0x4000, PSR::default(), FPSCR::default(), false);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);
        let inst = DecodedArm {
            raw: (0xE << 28) | 0x012F_FF30 | 14,
            id: ArmInstId::BlxReg,
        };

        assert!(!arm_blx_reg(&mut ir, &inst));
        assert_eq!(block.instructions.len(), 4);
        let bx_pos = block
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::A32BXWritePC)
            .expect("missing A32BXWritePC");
        let lr_pos = block
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::A32SetRegister)
            .expect("missing A32SetRegister");
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::PushRSB));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32GetRegister));
        assert!(bx_pos < lr_pos);
        assert_eq!(block.instructions[lr_pos].args[0].get_a32_reg(), Reg::R14);
        assert!(matches!(block.terminal, Terminal::CheckHalt { .. }));
    }
}
