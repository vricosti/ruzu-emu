// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/shader_recompiler/frontend/maxwell/translate/impl/floating_point_set_predicate.cpp

use super::common_funcs::{floating_point_compare_32, predicate_combine};
use super::{bit, field, TranslatorVisitor};
use crate::frontend::maxwell_opcodes::MaxwellOpcode;
use crate::ir::types::{FmzMode, FpControl};
use crate::ir::value::Pred;

pub fn fsetp(tv: &mut TranslatorVisitor, insn: u64, opcode: MaxwellOpcode) {
    let src_a = tv.f(tv.src_a_reg(insn));
    let src_b = tv.decode_src_b_f32(insn, opcode);

    let dest_pred_b = Pred(field(insn, 0, 3) as u8);
    let dest_pred_a = Pred(field(insn, 3, 3) as u8);
    let abs_a = bit(insn, 7);
    let abs_b = bit(insn, 44);
    let neg_a = bit(insn, 43);
    let neg_b = bit(insn, 6);

    let a = tv.ir.fp_abs_neg_32(src_a, abs_a, neg_a);
    let b = tv.ir.fp_abs_neg_32(src_b, abs_b, neg_b);

    let ftz = bit(insn, 47);
    let control = FpControl {
        fmz_mode: if ftz { FmzMode::FTZ } else { FmzMode::None },
        ..FpControl::default()
    };
    let cmp_op = field(insn, 48, 4);
    let bool_op = field(insn, 45, 2);
    let pred_idx = Pred(field(insn, 39, 3) as u8);
    let neg_bop_pred = bit(insn, 42);
    let pred39 = tv.ir.get_pred(pred_idx, neg_bop_pred);

    let cmp_result = floating_point_compare_32(tv, a, b, cmp_op, control);
    let result_a = predicate_combine(tv, cmp_result.clone(), pred39.clone(), bool_op);
    let not_cmp = tv.ir.logical_not(cmp_result);
    let result_b = predicate_combine(tv, not_cmp, pred39, bool_op);

    tv.ir.set_pred(dest_pred_a, result_a);
    tv.ir.set_pred(dest_pred_b, result_b);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::basic_block::Block;
    use crate::ir::opcodes::Opcode;
    use crate::ir::program::Program;
    use crate::ir::types::{FmzMode, FpControl, ShaderStage};
    use crate::ir::value::Value;

    #[test]
    fn fsetp_writes_upstream_predicate_destinations_and_negates_bop_pred() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        let insn = (1u64) // dest_pred_b
            | (6u64 << 3) // dest_pred_a
            | (1u64 << 8) // src_a_reg
            | (3u64 << 39) // bop_pred
            | (1u64 << 42) // neg_bop_pred
            | (0u64 << 45) // AND
            | (2u64 << 48); // Equal

        fsetp(&mut tv, insn, MaxwellOpcode::FSETP_reg);

        let block = &tv.ir.program.blocks[0];
        let set_preds: Vec<_> = block
            .iter()
            .filter(|inst| inst.opcode == Opcode::SetPred)
            .collect();
        assert_eq!(set_preds.len(), 2);
        assert_eq!(set_preds[0].args[0], Value::Pred(Pred(6)));
        assert_eq!(set_preds[1].args[0], Value::Pred(Pred(1)));
        assert!(block.iter().any(|inst| inst.opcode == Opcode::LogicalNot));
    }

    #[test]
    fn fsetp_uses_unordered_less_greater_equal_opcodes() {
        for (cmp, expected) in [
            (11u64, Opcode::FPUnordLessThanEqual32),
            (14u64, Opcode::FPUnordGreaterThanEqual32),
        ] {
            let mut program = Program::new(ShaderStage::VertexB);
            program.blocks.push(Block::new());
            let mut tv = TranslatorVisitor::new(&mut program, 0);
            let insn = (1u64 << 8) | (1u64 << 39) | (0u64 << 45) | (cmp << 48);

            fsetp(&mut tv, insn, MaxwellOpcode::FSETP_reg);

            let block = &tv.ir.program.blocks[0];
            assert!(
                block.iter().any(|inst| inst.opcode == expected),
                "missing {:?} for cmp {}",
                expected,
                cmp
            );
        }
    }

    #[test]
    fn fsetp_threads_ftz_bit_into_compare_fp_control_like_upstream() {
        for (ftz, expected) in [(false, FmzMode::None), (true, FmzMode::FTZ)] {
            let mut program = Program::new(ShaderStage::VertexB);
            program.blocks.push(Block::new());
            let mut tv = TranslatorVisitor::new(&mut program, 0);
            let insn =
                (1u64 << 8) | (1u64 << 39) | (0u64 << 45) | ((ftz as u64) << 47) | (2u64 << 48);

            fsetp(&mut tv, insn, MaxwellOpcode::FSETP_reg);

            let cmp = tv.ir.program.blocks[0]
                .iter()
                .find(|inst| inst.opcode == Opcode::FPOrdEqual32)
                .expect("missing FSETP comparison");
            assert_eq!(FpControl::from_u32(cmp.flags).fmz_mode, expected);
        }
    }

    #[test]
    fn fsetp_rejects_invalid_boolean_op_like_upstream() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        let mut tv = TranslatorVisitor::new(&mut program, 0);
        let insn = (1u64 << 8) | (1u64 << 39) | (3u64 << 45) | (2u64 << 48);

        let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fsetp(&mut tv, insn, MaxwellOpcode::FSETP_reg);
        })).unwrap_err();
        let error = error.downcast_ref::<crate::exception::NotImplementedException>().unwrap();
        assert_eq!(error.to_string(), "Invalid bop 3 is not implemented");
    }
}
