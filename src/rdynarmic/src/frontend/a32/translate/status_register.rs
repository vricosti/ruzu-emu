use crate::frontend::a32::decoder::{arm_expand_imm, DecodedArm};
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

/// ARM MRS - move status register to register.
pub fn arm_mrs(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = inst.rd();
    let cpsr = ir.get_cpsr();
    ir.set_register(rd, cpsr);
    true
}

/// ARM MSR (immediate) - move immediate to status register.
pub fn arm_msr_imm(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let mask = (inst.raw >> 16) & 0xF;
    let rotate = inst.rotate();
    let imm8 = inst.imm8();
    let imm = arm_expand_imm(rotate, imm8);

    apply_msr(ir, mask, Value::ImmU32(imm))
}

/// ARM MSR (register) - move register to status register.
pub fn arm_msr_reg(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let mask = (inst.raw >> 16) & 0xF;
    let rm = inst.rm();
    let value = ir.get_register(rm);

    apply_msr(ir, mask, value)
}

fn apply_msr(ir: &mut A32IREmitter, mask: u32, value: Value) -> bool {
    let write_nzcvq = (mask & 0x8) != 0;
    let write_g = (mask & 0x4) != 0;
    let write_e = (mask & 0x2) != 0;

    if write_nzcvq {
        let masked = ir.ir().and_32(value, Value::ImmU32(0xF800_0000));
        ir.set_cpsr_nzcvq(masked);
    }

    if write_g {
        let masked = ir.ir().and_32(value, Value::ImmU32(0x000F_0000));
        ir.set_ge_flags_compressed(masked);
    }

    if write_e {
        let cpsr_mask = (if write_nzcvq { 0xF800_0000 } else { 0 })
            | (if write_g { 0x000F_0000 } else { 0 })
            | 0x0000_0200;
        let cpsr = ir.get_cpsr();
        let old_cpsr = ir.ir().and_32(cpsr, Value::ImmU32(!cpsr_mask));
        let new_cpsr = ir.ir().and_32(value, Value::ImmU32(cpsr_mask));
        let merged_cpsr = ir.ir().or_32(old_cpsr, new_cpsr);
        ir.set_cpsr(merged_cpsr);

        let loc = ir.current_location.expect("current_location not set");
        let next_loc = loc.advance_pc(4);
        ir.base.push_rsb(next_loc.into());
        ir.branch_write_pc(Value::ImmU32(next_loc.pc()));
        ir.set_term(Terminal::CheckHalt {
            else_: Box::new(Terminal::PopRSBHint),
        });
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder::ArmInstId;
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;

    #[test]
    fn arm_msr_reg_write_e_matches_upstream_control_flow() {
        let loc = A32LocationDescriptor::new(0x4000, PSR::default(), FPSCR::default(), false);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);
        let inst = DecodedArm {
            raw: (0xE << 28) | (0b0110 << 16),
            id: ArmInstId::MsrReg,
        };

        assert!(!arm_msr_reg(&mut ir, &inst));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::PushRSB));
        assert_eq!(
            block.instructions.last().map(|inst| inst.opcode),
            Some(Opcode::A32SetRegister)
        );
        assert!(matches!(
            &block.terminal,
            Terminal::CheckHalt { else_ } if matches!(else_.as_ref(), Terminal::PopRSBHint)
        ));
    }
}
