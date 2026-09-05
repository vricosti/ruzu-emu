// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `frontend/A32/translate/impl/asimd_two_regs_misc.cpp`.

use crate::frontend::a32::decoder::DecodedArm;
use crate::ir::a32_emitter::A32IREmitter;

use super::asimd::to_vector_reg;

/// Port of upstream `TranslatorVisitor::asimd_VMOVN`.
pub fn arm_asimd_vmovn(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let d = ((inst.raw >> 22) & 1) != 0;
    let sz = (inst.raw >> 18) & 0x3;
    let vd = (inst.raw >> 12) & 0xF;
    let m = ((inst.raw >> 5) & 1) != 0;
    let vm = inst.raw & 0xF;

    if sz == 0b11 || (vm & 1) != 0 {
        return super::undefined_instruction(ir);
    }
    let esize = 8usize << sz;
    let Some(d_reg) = to_vector_reg(false, d, vd) else {
        return super::undefined_instruction(ir);
    };
    let Some(m_reg) = to_vector_reg(true, m, vm) else {
        return super::undefined_instruction(ir);
    };

    let reg_m = ir.get_vector(m_reg);
    let result = ir.ir().vector_narrow(2 * esize, reg_m);
    ir.set_vector(d_reg, result);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder::ArmInstId;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;

    #[test]
    fn observed_vmovn_i64_emits_narrow() {
        let loc = A32LocationDescriptor::at(0x2000);
        let mut block = Block::new(loc.to_location());
        let decoded = DecodedArm {
            raw: 0xF3FA_2220,
            id: ArmInstId::AsimdVmovn,
        };
        let ok = {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            arm_asimd_vmovn(&mut ir, &decoded)
        };
        assert!(ok);
        let opcodes: Vec<_> = block.instructions.iter().map(|inst| inst.opcode).collect();
        assert!(opcodes.contains(&Opcode::VectorNarrow64));
        assert_eq!(opcodes.last(), Some(&Opcode::A32SetVector));
    }
}
