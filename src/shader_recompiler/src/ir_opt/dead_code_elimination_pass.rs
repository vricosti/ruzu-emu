// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Dead code elimination pass — remove unused instructions with no side effects.
//!
//! Matches upstream `dead_code_elimination_pass.cpp`.
//!
//! Iterates instructions in reverse order within each block. An instruction is
//! removed if it has no uses (`use_count == 0`) and may not have side effects.

use crate::ir::program::Program;

/// Remove dead (unused, side-effect-free) instructions.
pub fn dead_code_elimination_pass(program: &mut Program) {
    // Indexed IR rewrites do not maintain use counts eagerly. Materialize
    // upstream's use-def invariant once; erase_inst updates it during traversal.
    program.recompute_use_counts();
    for position in 0..program.post_order_blocks.len() {
        let block = program.post_order_blocks[position];
        // Snapshot stable slots to release the block borrow before cross-block
        // operand use counts are updated by Program::erase_inst.
        let slots: Vec<u32> = program.block(block)
            .indexed_rev_iter().map(|(slot, _)| slot).collect();
        for inst in slots {
            let candidate = program.block(block).inst(inst);
            if candidate.use_count == 0 && !candidate.may_have_side_effects() {
                program.erase_inst(crate::ir::value::InstRef { block, inst });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::dead_code_elimination_pass;
    use crate::ir::basic_block::Block;
    use crate::ir::instruction::Inst;
    use crate::ir::opcodes::Opcode;
    use crate::ir::program::{Program, SyntaxNode};
    use crate::ir::types::ShaderStage;
    use crate::ir::value::{InstRef, Value};

    #[test]
    fn dce_preserves_instref_indices_with_tombstones() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        program.post_order_blocks.push(0);
        program.blocks[0].append_inst(Inst::new(
            Opcode::IAdd32,
            vec![Value::ImmU32(1), Value::ImmU32(2)],
        ));
        program.blocks[0].append_inst(Inst::new(
            Opcode::IAdd32,
            vec![Value::ImmU32(3), Value::ImmU32(4)],
        ));
        program.blocks[0].append_inst(Inst::new(
            Opcode::SetAttribute,
            vec![
                Value::Attribute(crate::ir::value::Attribute::POSITION_X),
                Value::Inst(InstRef { block: 0, inst: 1 }),
                Value::ImmU32(0),
            ],
        ));

        dead_code_elimination_pass(&mut program);

        assert_eq!(program.blocks[0].instructions.len(), 3);
        assert!(program.blocks[0].instructions[0].is_none());
        assert_eq!(program.blocks[0].inst(1).opcode, Opcode::IAdd32);
        assert_eq!(
            program.blocks[0].inst(2).args[1],
            Value::Inst(InstRef { block: 0, inst: 1 })
        );
    }

    #[test]
    fn dce_counts_phi_args_as_uses() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        program.post_order_blocks.push(0);
        program.blocks[0].append_inst(Inst::new(
            Opcode::IAdd32,
            vec![Value::ImmU32(1), Value::ImmU32(2)],
        ));
        let mut phi = Inst::new(Opcode::Phi, Vec::new());
        phi.add_phi_operand(0, Value::Inst(InstRef { block: 0, inst: 0 }));
        let phi_ref = InstRef {
            block: 0,
            inst: program.blocks[0].append_inst(phi),
        };
        program.blocks[0].append_inst(Inst::new(
            Opcode::SetAttribute,
            vec![
                Value::Attribute(crate::ir::value::Attribute::POSITION_X),
                Value::Inst(phi_ref),
                Value::ImmU32(0),
            ],
        ));

        dead_code_elimination_pass(&mut program);

        assert_eq!(program.blocks[0].inst(0).opcode, Opcode::IAdd32);
        assert_eq!(program.blocks[0].inst(1).opcode, Opcode::Phi);
    }

    #[test]
    fn dce_counts_syntax_conditions_as_uses() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        program.post_order_blocks.push(0);
        let cond = program.blocks[0]
            .append_inst(Inst::new(Opcode::ConditionRef, vec![Value::ImmU1(true)]));
        program.syntax_list.push(SyntaxNode::If {
            cond: Value::Inst(InstRef {
                block: 0,
                inst: cond,
            }),
            body: 0,
            merge: 0,
        });

        dead_code_elimination_pass(&mut program);

        assert_eq!(program.blocks[0].inst(cond).opcode, Opcode::ConditionRef);
    }

    #[test]
    fn dce_unlinks_dead_pseudo_flags_from_their_parent() {
        let mut program = Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        program.post_order_blocks.push(0);
        let parent = program.blocks[0].append_inst(Inst::new(
            Opcode::IAdd32,
            vec![Value::ImmU32(1), Value::ImmU32(2)],
        ));
        for pseudo_op in [
            Opcode::GetZeroFromOp,
            Opcode::GetSignFromOp,
            Opcode::GetCarryFromOp,
            Opcode::GetOverflowFromOp,
        ] {
            let pseudo = program.blocks[0].append_inst(Inst::new(
                pseudo_op,
                vec![Value::Inst(InstRef {
                    block: 0,
                    inst: parent,
                })],
            ));
            program.blocks[0].inst_mut(parent).set_associated_pseudo(
                pseudo_op,
                InstRef {
                    block: 0,
                    inst: pseudo,
                },
            );
        }
        program.blocks[0].append_inst(Inst::new(
            Opcode::SetAttribute,
            vec![
                Value::Attribute(crate::ir::value::Attribute::POSITION_X),
                Value::Inst(InstRef {
                    block: 0,
                    inst: parent,
                }),
                Value::ImmU32(0),
            ],
        ));

        dead_code_elimination_pass(&mut program);

        let parent = program.blocks[0].inst(parent);
        for pseudo_op in [
            Opcode::GetZeroFromOp,
            Opcode::GetSignFromOp,
            Opcode::GetCarryFromOp,
            Opcode::GetOverflowFromOp,
        ] {
            assert_eq!(parent.get_associated_pseudo(pseudo_op), None);
        }
    }
    #[test]
    fn dce_uses_post_order_and_leaves_unvisited_blocks_untouched() {
        let mut program = Program::new(ShaderStage::VertexB);
        for _ in 0..4 { program.blocks.push(Block::new()); }
        program.post_order_blocks = vec![2, 1, 0];
        let root = program.blocks[0].append_inst(Inst::new(Opcode::IAdd32,
            vec![Value::ImmU32(1), Value::ImmU32(2)]));
        let middle = program.blocks[1].append_inst(Inst::new(Opcode::Identity,
            vec![Value::Inst(InstRef { block: 0, inst: root })]));
        program.blocks[2].append_inst(Inst::new(Opcode::Identity,
            vec![Value::Inst(InstRef { block: 1, inst: middle })]));
        // Storage can contain a block absent from Program::post_order_blocks.
        // Eden only visits the supplied list, not every allocated block.
        program.blocks[3].append_inst(Inst::new(Opcode::IAdd32,
            vec![Value::ImmU32(3), Value::ImmU32(4)]));
        dead_code_elimination_pass(&mut program);
        assert!(program.blocks[..3].iter().all(|block| block.is_empty()));
        assert_eq!(program.blocks[3].live_len(), 1);
    }

}
