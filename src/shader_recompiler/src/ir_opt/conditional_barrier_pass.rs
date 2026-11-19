// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `ir_opt/conditional_barrier_pass.cpp`
//!
//! Removes barrier instructions that appear inside conditional control flow,
//! since barriers in divergent control flow cause undefined behavior on some
//! GPUs.

use crate::ir::opcodes::Opcode;
use crate::ir::program::{Program, SyntaxNode};

/// Remove barriers inside conditional control flow by replacing them with Identity.
pub fn conditional_barrier_pass(program: &mut Program) {
    let mut conditional_control_flow_count: i32 = 0;
    let mut conditional_return_count: i32 = 0;

    for node in &program.syntax_list {
        match node {
            SyntaxNode::If { .. } | SyntaxNode::Loop { .. } => {
                conditional_control_flow_count += 1;
            }
            SyntaxNode::EndIf { .. } | SyntaxNode::Repeat { .. } => {
                conditional_control_flow_count -= 1;
            }
            SyntaxNode::Unreachable | SyntaxNode::Return => {
                if conditional_control_flow_count > 0 {
                    conditional_return_count += 1;
                }
            }
            SyntaxNode::Block(block_idx) => {
                let block_idx = *block_idx as usize;
                if block_idx < program.blocks.len() {
                    for inst in program.blocks[block_idx].iter_mut() {
                        if (conditional_control_flow_count > 0 || conditional_return_count > 0)
                            && inst.opcode == Opcode::Barrier
                        {
                            log::warn!("Barrier within conditional control flow");
                            inst.opcode = Opcode::Identity;
                            program.info.removed_conditional_barrier = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    debug_assert_eq!(conditional_control_flow_count, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{emitter::Emitter, types::ShaderStage, Value};

    #[test]
    fn removed_barrier_metadata_survives_info_collection() {
        for early_return in [false, true] {
            let mut program = Program::new(ShaderStage::Compute);
            let block = program.add_block();
            Emitter::new(&mut program, block).barrier();
            let condition = SyntaxNode::If { cond: Value::ImmU1(true), body: block, merge: block };
            program.syntax_list = if early_return {
                vec![condition, SyntaxNode::Return, SyntaxNode::EndIf { merge: block }, SyntaxNode::Block(block)]
            } else {
                vec![condition, SyntaxNode::Block(block), SyntaxNode::EndIf { merge: block }]
            };
            conditional_barrier_pass(&mut program);
            assert!(program.info.removed_conditional_barrier);
            assert_eq!(program.block(block).iter().next().unwrap().opcode, Opcode::Identity);
            crate::ir_opt::collect_shader_info_pass::collect_shader_info_pass(&mut program);
            assert!(program.info.removed_conditional_barrier);
            // Reprocessing an already lowered shader must not forget the requirement.
            conditional_barrier_pass(&mut program);
            assert!(program.info.removed_conditional_barrier);
        }
    }

    #[test]
    fn unconditional_barrier_keeps_opcode_without_warp_requirement() {
        let mut program = Program::new(ShaderStage::Compute);
        let block = program.add_block();
        Emitter::new(&mut program, block).barrier();
        program.syntax_list = vec![SyntaxNode::Block(block), SyntaxNode::Return];
        conditional_barrier_pass(&mut program);
        assert!(!program.info.removed_conditional_barrier);
        assert_eq!(program.block(block).iter().next().unwrap().opcode, Opcode::Barrier);
    }
}
