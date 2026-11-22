// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/frontend/maxwell/translate/impl/floating_point_multi_function.cpp

use super::{bit, field, TranslatorVisitor};

/// MUFU — Multi-Function Unit (transcendentals).
pub fn mufu(tv: &mut TranslatorVisitor, insn: u64) {
    let dst = tv.dst_reg(insn);
    let src = tv.f(tv.src_a_reg(insn));
    let abs_src = bit(insn, 46);
    let neg_src = bit(insn, 48);
    let sat = bit(insn, 50);
    let op = field(insn, 20, 4);

    let a = tv.ir.fp_abs_neg_32(src, abs_src, neg_src);

    let mut result = match op {
        0 => tv.ir.fp_cos(a),           // COS
        1 => tv.ir.fp_sin(a),           // SIN
        2 => tv.ir.fp_exp2(a),          // EX2
        3 => tv.ir.fp_log2(a),          // LG2
        4 => tv.ir.fp_recip_32(a),      // RCP
        5 => tv.ir.fp_recip_sqrt_32(a), // RSQ
        6 => panic!("MUFU.RCP64H not implemented (upstream NotImplementedException)"),
        7 => panic!("MUFU.RSQ64H not implemented (upstream NotImplementedException)"),
        8 => tv.ir.fp_sqrt_32(a), // SQRT
        // Not a Maxwell operation: only lowered native (SM86) programs emit it
        // (Lichii floating_point_multi_function.cpp, eaeb09b098).
        9 => tv.ir.fp_tanh(a), // TANH
        _ => panic!("Unknown MUFU op: {}", op),
    };

    if sat {
        result = tv.ir.fp_saturate_32(result);
    }

    tv.set_f(dst, result);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{basic_block::Block, opcodes::Opcode, program::Program, types::ShaderStage};

    // MUFU operation 9 only comes from lowered native programs (MUFU.TANH, Lichii eaeb09b098).
    #[test]
    fn mufu_operation_nine_is_fp_tanh() {
        let mut program = Program::new(ShaderStage::Compute);
        program.blocks.push(Block::new());
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        mufu(&mut tv, 0x5080_0000_0097_0413);
        let opcodes: Vec<_> = tv.ir.program.blocks[0].iter().map(|inst| inst.opcode).collect();
        assert!(opcodes.contains(&Opcode::FPTanh), "{opcodes:?}");
        assert!(!opcodes.contains(&Opcode::FPExp2));
    }
}
