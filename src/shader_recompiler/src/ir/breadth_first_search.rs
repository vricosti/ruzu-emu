// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `frontend/ir/breadth_first_search.h`
//!
//! Generic breadth-first search over the SSA instruction graph.
//! Used by optimization passes to find specific instruction patterns
//! by traversing arguments.

use std::collections::{HashSet, VecDeque};

use super::instruction::Inst;
use super::opcodes::Opcode;
use super::program::Program;
use super::value::{InstRef, Value};

fn inst_recursive(program: &Program, mut value: Value) -> Option<InstRef> {
    loop {
        let Value::Inst(inst_ref) = value else {
            return None;
        };
        let inst = program.block(inst_ref.block).inst(inst_ref.inst);
        if inst.opcode != Opcode::Identity {
            return Some(inst_ref);
        }
        value = *inst.arg(0);
    }
}

/// Perform a breadth-first search over the SSA graph starting from `value`.
///
/// The predicate `pred` is called for each instruction encountered.
/// If it returns `Some(result)`, the search terminates and returns that result.
/// If the entire SSA tree is traversed without finding a match, returns `None`.
///
/// Visits the rightmost arguments first, matching upstream behavior.
pub fn breadth_first_search<T, F>(value: Value, program: &Program, mut pred: F) -> Option<T>
where
    F: FnMut(&Inst) -> Option<T>,
{
    if value.is_immediate() {
        return None;
    }

    // Upstream keeps `visited` in a small_vector and searches it linearly.
    // A search that fails walks every reachable instruction, and Mario Kart
    // World's 28,000-instruction compute shaders run one such search per
    // global memory access: the linear lookup held the GPU thread for
    // seconds per shader. A hash set gives the same membership answers, so
    // the visiting order is unchanged.
    let mut visited = HashSet::with_capacity(2);
    let mut queue = VecDeque::from([inst_recursive(program, value)?]);

    while let Some(inst_ref) = queue.pop_front() {
        let inst = program.block(inst_ref.block).inst(inst_ref.inst);

        if let Some(result) = pred(inst) {
            return Some(result);
        }

        for index in (0..inst.num_args()).rev() {
            let arg = *inst.arg(index);
            if arg.is_immediate() {
                continue;
            }
            let Some(arg_ref) = inst_recursive(program, arg) else {
                continue;
            };
            if visited.insert(arg_ref) {
                queue.push_back(arg_ref);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::basic_block::Block;

    #[test]
    fn visits_phi_operands_rightmost_first_like_upstream() {
        let mut program = Program::new(crate::ir::types::ShaderStage::Fragment);
        program.blocks.push(Block::new());
        let left = program.blocks[0].append_inst(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(1), Value::ImmU32(0x20)],
        ));
        let right = program.blocks[0].append_inst(Inst::new(
            Opcode::GetCbufU32,
            vec![Value::ImmU32(2), Value::ImmU32(0x40)],
        ));
        let mut phi = Inst::phi();
        phi.add_phi_operand(
            0,
            Value::Inst(InstRef {
                block: 0,
                inst: left,
            }),
        );
        phi.add_phi_operand(
            0,
            Value::Inst(InstRef {
                block: 0,
                inst: right,
            }),
        );
        let phi = program.blocks[0].append_inst(phi);

        let found = breadth_first_search(
            Value::Inst(InstRef {
                block: 0,
                inst: phi,
            }),
            &program,
            |inst| (inst.opcode == Opcode::GetCbufU32).then(|| inst.arg(0).imm_u32()),
        );

        assert_eq!(found, Some(2));
    }
}
