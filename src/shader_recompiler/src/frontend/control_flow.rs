// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Control flow graph builder for Maxwell shader binaries.
//!
//! Analyzes branch instructions (BRA, BRK, SYNC, EXIT, etc.) and convergence
//! stack operations (SSY, PBK, PCNT) to build a flat CFG of basic blocks.
//!
//! Ref: zuyu `frontend/maxwell/control_flow.cpp`

use super::indirect_branch_table_track::track_indirect_branch_table;
use super::instruction::{Instruction, Predicate};
use super::location::Location;
use super::maxwell_opcodes::{self, MaxwellOpcode};
use crate::environment::Environment;
pub use crate::ir::condition::Condition;
use crate::ir::condition::IrPred;
use crate::ir::flow_test::FlowTest;

/// How a basic block ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndClass {
    /// Conditional or unconditional branch.
    Branch,
    /// Indirect branch (BRX/JMX).
    IndirectBranch,
    /// Subroutine call.
    Call,
    /// Return from subroutine.
    Return,
    /// Shader exit.
    Exit,
    /// Fragment kill/discard.
    Kill,
}

/// A basic block in the flat CFG.
#[derive(Debug, Clone)]
pub struct CfgBlock {
    /// Start instruction offset (instruction index, not byte offset).
    pub begin: u32,
    /// End instruction offset (exclusive).
    pub end: u32,
    /// How this block ends.
    pub end_class: EndClass,
    /// Branch target (true path) block index.
    pub branch_true: Option<usize>,
    /// Fall-through (false path) block index.
    pub branch_false: Option<usize>,
    /// Branch condition (None = unconditional).
    pub cond: Condition,
    /// Stack depth at entry (for SSY/PBK/PCNT tracking).
    pub stack_depth: u32,
    /// Register and offset used to materialize an indirect branch address.
    pub branch_reg: u32,
    pub branch_offset: i32,
    /// Resolved indirect branch destinations.
    pub indirect_branches: Vec<IndirectBranch>,
}

/// Convergence stack entry — tracks SSY/PBK/PCNT push points.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StackEntry {
    /// Type of stack push (SSY, PBK, PCNT).
    kind: StackKind,
    /// Target address (instruction index) pushed onto the stack.
    target: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StackKind {
    Ssy,
    Pbk,
    Pcnt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Ssy,
    Pbk,
    Pexit,
    Pret,
    Pcnt,
    Plongjmp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlowStackEntry {
    pub token: Token,
    pub target: Location,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlowStack {
    entries: Vec<FlowStackEntry>,
}

impl FlowStack {
    pub fn push(&mut self, token: Token, target: Location) {
        self.entries.push(FlowStackEntry { token, target });
    }

    pub fn pop(&self, token: Token) -> (Location, Self) {
        let pc = self.peek(token).unwrap_or_else(|| {
            std::panic::panic_any(crate::exception::LogicError::new(
                "Token could not be found",
            ))
        });
        (pc, self.remove(token))
    }

    pub fn peek(&self, token: Token) -> Option<Location> {
        self.entries
            .iter()
            .rfind(|entry| entry.token == token)
            .map(|entry| entry.target)
    }

    pub fn remove(&self, token: Token) -> Self {
        let Some(pos) = self.entries.iter().rposition(|entry| entry.token == token) else {
            return self.clone();
        };
        Self {
            entries: self.entries[..pos].to_vec(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct IndirectBranch {
    pub block: usize,
    pub address: u32,
}

#[derive(Debug, Clone)]
pub struct FlowBlock {
    pub begin: Location,
    pub end: Location,
    pub end_class: EndClass,
    pub cond: Condition,
    pub stack: FlowStack,
    pub branch_true: Option<usize>,
    pub branch_false: Option<usize>,
    pub function_call: usize,
    pub return_block: Option<usize>,
    pub branch_reg: u32,
    pub branch_offset: i32,
    pub indirect_branches: Vec<IndirectBranch>,
}

impl FlowBlock {
    fn new(begin: Location) -> Self {
        Self {
            begin,
            end: begin,
            end_class: EndClass::Branch,
            cond: Condition::always(),
            stack: FlowStack::default(),
            branch_true: None,
            branch_false: None,
            function_call: 0,
            return_block: None,
            branch_reg: 0,
            branch_offset: 0,
            indirect_branches: Vec::new(),
        }
    }

    pub fn contains(&self, pc: Location) -> bool {
        pc >= self.begin && pc < self.end
    }
}

#[derive(Debug, Clone)]
pub struct FlowLabel {
    pub address: Location,
    pub block: usize,
    pub stack: FlowStack,
}

#[derive(Debug, Clone)]
pub struct FlowFunction {
    pub entrypoint: Location,
    pub labels: Vec<FlowLabel>,
    pub blocks: Vec<usize>,
}

impl FlowFunction {
    fn new(blocks: &mut Vec<FlowBlock>, start_address: Location) -> Self {
        let block = blocks.len();
        blocks.push(FlowBlock::new(start_address));
        Self {
            entrypoint: start_address,
            labels: vec![FlowLabel {
                address: start_address,
                block,
                stack: FlowStack::default(),
            }],
            blocks: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnalysisState {
    Branch,
    Continue,
}

/// Port of upstream `Shader::Maxwell::Flow::CFG`.
pub struct FlowCfg {
    pub functions: Vec<FlowFunction>,
    pub blocks: Vec<FlowBlock>,
    program_start: Location,
    exits_to_dispatcher: bool,
    dispatch_block: Option<usize>,
}

impl FlowCfg {
    pub fn new(
        env: &mut dyn Environment,
        start_address: Location,
        exits_to_dispatcher: bool,
    ) -> Self {
        let mut cfg = Self {
            functions: Vec::new(),
            blocks: Vec::new(),
            program_start: start_address,
            exits_to_dispatcher,
            dispatch_block: None,
        };
        if exits_to_dispatcher {
            let dispatch = cfg.blocks.len();
            let mut block = FlowBlock::new(Location::default());
            block.end_class = EndClass::Exit;
            cfg.blocks.push(block);
            cfg.dispatch_block = Some(dispatch);
        }
        cfg.functions
            .push(FlowFunction::new(&mut cfg.blocks, start_address));
        let mut function_id = 0;
        while function_id < cfg.functions.len() {
            while let Some(mut label) = cfg.functions[function_id].labels.pop() {
                cfg.analyze_label(env, function_id, &mut label);
            }
            function_id += 1;
        }
        if let Some(dispatch) = cfg.dispatch_block {
            if let Some(&last) = cfg.functions[0].blocks.last() {
                let end = cfg.blocks[last].end.next();
                cfg.blocks[dispatch].begin = end;
                cfg.blocks[dispatch].end = end;
                cfg.insert_block_sorted(0, dispatch);
            }
        }
        cfg
    }

    fn analyze_label(
        &mut self,
        env: &mut dyn Environment,
        function_id: usize,
        label: &mut FlowLabel,
    ) {
        if self.inspect_visited_blocks(function_id, label) {
            return;
        }
        let mut pc = label.address;
        let next = self.next_block_after(function_id, pc);
        let block = label.block;
        self.blocks[block].stack = label.stack.clone();
        let mut is_branch = false;
        while next.is_none_or(|next_block| pc < self.blocks[next_block].begin) {
            is_branch = self.analyze_inst(env, block, function_id, pc) == AnalysisState::Branch;
            if is_branch {
                break;
            }
            pc.step();
        }
        if !is_branch {
            self.blocks[block].end = pc;
            self.blocks[block].cond = Condition::always();
            self.blocks[block].branch_true = next;
            self.blocks[block].branch_false = None;
        }
        self.insert_block_sorted(function_id, block);
    }

    fn inspect_visited_blocks(&mut self, function_id: usize, label: &FlowLabel) -> bool {
        let pc = label.address;
        let containing = self.functions[function_id]
            .blocks
            .iter()
            .copied()
            .find(|&block| self.blocks[block].contains(pc));
        let Some(visited) = containing else {
            return false;
        };
        if self.blocks[visited].begin == pc {
            std::panic::panic_any(crate::exception::LogicError::new("Dangling block"));
        }
        self.split_block(function_id, visited, label.block, pc);
        true
    }

    fn analyze_inst(
        &mut self,
        env: &mut dyn Environment,
        block: usize,
        function_id: usize,
        pc: Location,
    ) -> AnalysisState {
        let inst = Instruction::new(env.read_instruction(pc.offset()));
        let Some(opcode) = maxwell_opcodes::decode_opcode(inst.raw) else {
            return AnalysisState::Continue;
        };
        match opcode {
            MaxwellOpcode::BRA | MaxwellOpcode::JMP | MaxwellOpcode::RET => {
                if !self.analyze_branch(block, function_id, pc, inst, opcode) {
                    return AnalysisState::Continue;
                }
                match opcode {
                    MaxwellOpcode::BRA | MaxwellOpcode::JMP => {
                        self.analyze_bra(block, function_id, pc, inst, is_absolute_jump(opcode));
                    }
                    MaxwellOpcode::RET => {
                        self.blocks[block].end_class = EndClass::Return;
                    }
                    _ => {}
                }
                self.blocks[block].end = pc;
                AnalysisState::Branch
            }
            MaxwellOpcode::BRK
            | MaxwellOpcode::CONT
            | MaxwellOpcode::LONGJMP
            | MaxwellOpcode::SYNC => {
                if !self.analyze_branch(block, function_id, pc, inst, opcode) {
                    return AnalysisState::Continue;
                }
                let (stack_pc, new_stack) = self.blocks[block].stack.pop(opcode_token(opcode));
                self.blocks[block].branch_true =
                    Some(self.add_label(block, new_stack, stack_pc, function_id));
                self.blocks[block].end = pc;
                AnalysisState::Branch
            }
            MaxwellOpcode::KIL => {
                let pred = inst.pred();
                let cond = condition_from_flow(inst.branch_flow_test(), pred);
                self.analyze_cond_inst(block, function_id, pc, EndClass::Kill, cond);
                AnalysisState::Branch
            }
            MaxwellOpcode::PBK
            | MaxwellOpcode::PCNT
            | MaxwellOpcode::PEXIT
            | MaxwellOpcode::PLONGJMP
            | MaxwellOpcode::SSY => {
                let target = branch_offset(pc, inst);
                self.blocks[block].stack.push(opcode_token(opcode), target);
                AnalysisState::Continue
            }
            MaxwellOpcode::BRX | MaxwellOpcode::JMX => {
                self.analyze_indirect_branch(env, block, pc, inst, opcode, function_id)
            }
            MaxwellOpcode::EXIT => self.analyze_exit(block, function_id, pc, inst),
            MaxwellOpcode::PRET => {
                std::panic::panic_any(crate::exception::NotImplementedException::new(
                    "PRET flow analysis",
                ));
            }
            MaxwellOpcode::CAL | MaxwellOpcode::JCAL => {
                let cal_pc = if is_absolute_jump(opcode) {
                    Location::new(inst.branch_absolute())
                } else {
                    branch_offset(pc, inst)
                };
                let call_id = self
                    .functions
                    .iter()
                    .position(|function| function.entrypoint == cal_pc)
                    .unwrap_or_else(|| {
                        let id = self.functions.len();
                        self.functions
                            .push(FlowFunction::new(&mut self.blocks, cal_pc));
                        id
                    });
                self.blocks[block].end_class = EndClass::Call;
                self.blocks[block].function_call = call_id;
                self.blocks[block].return_block = Some(self.add_label(
                    block,
                    self.blocks[block].stack.clone(),
                    pc.next(),
                    function_id,
                ));
                self.blocks[block].end = pc;
                AnalysisState::Branch
            }
            _ => {
                let pred = inst.pred();
                if pred == Predicate::from_bool(true) || pred == Predicate::from_bool(false) {
                    return AnalysisState::Continue;
                }
                let cond = condition_from_predicate(pred);
                self.analyze_cond_inst(block, function_id, pc, EndClass::Branch, cond);
                AnalysisState::Branch
            }
        }
    }

    fn analyze_cond_inst(
        &mut self,
        block: usize,
        function_id: usize,
        pc: Location,
        end_class: EndClass,
        cond: Condition,
    ) {
        if self.blocks[block].begin != pc {
            self.blocks[block].end = pc;
            self.blocks[block].cond = Condition::always();
            self.blocks[block].branch_true =
                Some(self.add_label(block, self.blocks[block].stack.clone(), pc, function_id));
            self.blocks[block].branch_false = None;
            return;
        }

        let conditional_block = self.blocks.len();
        let original = self.blocks[block].clone();
        self.blocks.push(original);
        self.blocks[block] = FlowBlock::new(pc.virtual_loc());
        self.blocks[block].end = pc.virtual_loc();
        self.blocks[block].stack = self.blocks[conditional_block].stack.clone();
        self.blocks[block].cond = cond;
        self.blocks[block].branch_true = Some(conditional_block);

        self.blocks[conditional_block].end = pc.next();
        self.blocks[conditional_block].end_class = end_class;
        let endif = self.add_label(
            conditional_block,
            self.blocks[block].stack.clone(),
            pc.next(),
            function_id,
        );
        self.blocks[block].branch_false = Some(endif);
        if matches!(end_class, EndClass::Branch | EndClass::Kill) {
            self.blocks[conditional_block].cond = Condition::always();
            self.blocks[conditional_block].branch_true = Some(endif);
            self.blocks[conditional_block].branch_false = None;
        }
        self.insert_block_sorted(function_id, conditional_block);
    }

    fn analyze_branch(
        &mut self,
        block: usize,
        function_id: usize,
        pc: Location,
        inst: Instruction,
        opcode: MaxwellOpcode,
    ) -> bool {
        if inst.branch_is_cbuf() {
            std::panic::panic_any(crate::exception::NotImplementedException::new(
                "Branch with constant buffer offset",
            ));
        }
        let pred = inst.pred();
        if pred == Predicate::from_bool(false) {
            return false;
        }
        let flow_test = if has_flow_test(opcode) {
            inst.branch_flow_test().unwrap_or(FlowTest::T)
        } else {
            FlowTest::T
        };
        if pred != Predicate::from_bool(true) || flow_test != FlowTest::T {
            self.blocks[block].cond = condition_from_flow(Some(flow_test), pred);
            self.blocks[block].branch_false = Some(self.add_label(
                block,
                self.blocks[block].stack.clone(),
                pc.next(),
                function_id,
            ));
        } else {
            self.blocks[block].cond = Condition::always();
        }
        true
    }

    fn analyze_bra(
        &mut self,
        block: usize,
        function_id: usize,
        pc: Location,
        inst: Instruction,
        is_absolute: bool,
    ) {
        let target = if is_absolute {
            Location::new(inst.branch_absolute())
        } else {
            branch_offset(pc, inst)
        };
        self.blocks[block].branch_true =
            Some(self.add_label(block, self.blocks[block].stack.clone(), target, function_id));
    }

    fn analyze_indirect_branch(
        &mut self,
        env: &mut dyn Environment,
        block: usize,
        pc: Location,
        inst: Instruction,
        opcode: MaxwellOpcode,
        function_id: usize,
    ) -> AnalysisState {
        let table = track_indirect_branch_table(env, pc, self.program_start).unwrap_or_else(|| {
            track_indirect_branch_table(env, pc, self.program_start);
            std::panic::panic_any(crate::exception::NotImplementedException::new(
                "Failed to track indirect branch",
            ));
        });
        let flow_test = inst.branch_flow_test().unwrap_or(FlowTest::T);
        let pred = inst.pred();
        if flow_test != FlowTest::T || pred != Predicate::from_bool(true) {
            std::panic::panic_any(crate::exception::NotImplementedException::new(
                "Conditional indirect branch",
            ));
        }

        let mut targets = Vec::with_capacity(table.num_entries as usize);
        for index in 0..table.num_entries {
            let mut target = env.read_cbuf_value(
                table.cbuf_index,
                table.cbuf_offset.wrapping_add(index.wrapping_mul(4)),
            );
            if !is_absolute_jump(opcode) {
                target = target.wrapping_add(pc.offset());
            }
            target = target.wrapping_add(table.branch_offset as u32);
            target = target.wrapping_add(8);
            targets.push(target);
        }
        targets.sort_unstable();
        targets.dedup();

        self.blocks[block].indirect_branches.reserve(targets.len());
        for target in targets {
            let branch = self.add_label(
                block,
                self.blocks[block].stack.clone(),
                Location::new(target),
                function_id,
            );
            self.blocks[block].indirect_branches.push(IndirectBranch {
                block: branch,
                address: target,
            });
        }
        self.blocks[block].cond = Condition::always();
        self.blocks[block].end = pc.next();
        self.blocks[block].end_class = EndClass::IndirectBranch;
        self.blocks[block].branch_reg = table.branch_reg;
        self.blocks[block].branch_offset = table.branch_offset.wrapping_add(8);
        if !is_absolute_jump(opcode) {
            self.blocks[block].branch_offset = self.blocks[block]
                .branch_offset
                .wrapping_add(pc.offset() as i32);
        }
        AnalysisState::Branch
    }

    fn analyze_exit(
        &mut self,
        block: usize,
        function_id: usize,
        pc: Location,
        inst: Instruction,
    ) -> AnalysisState {
        let flow_test = inst.branch_flow_test().unwrap_or(FlowTest::T);
        let pred = inst.pred();
        if pred == Predicate::from_bool(false) || flow_test == FlowTest::F {
            return AnalysisState::Continue;
        }
        if self.exits_to_dispatcher && function_id != 0 {
            std::panic::panic_any(crate::exception::NotImplementedException::new(
                "Dispatch EXIT on external function",
            ));
        }
        if pred != Predicate::from_bool(true) || flow_test != FlowTest::T {
            if self.blocks[block].stack.peek(Token::Pexit).is_some() {
                std::panic::panic_any(crate::exception::NotImplementedException::new(
                    "Conditional EXIT with PEXIT token",
                ));
            }
            if self.exits_to_dispatcher {
                self.blocks[block].end = pc;
                self.blocks[block].end_class = EndClass::Branch;
                self.blocks[block].cond = condition_from_flow(Some(flow_test), pred);
                self.blocks[block].branch_true = self.dispatch_block;
                self.blocks[block].branch_false = Some(self.add_label(
                    block,
                    self.blocks[block].stack.clone(),
                    pc.next(),
                    function_id,
                ));
                return AnalysisState::Branch;
            }
            self.analyze_cond_inst(
                block,
                function_id,
                pc,
                EndClass::Exit,
                condition_from_flow(Some(flow_test), pred),
            );
            return AnalysisState::Branch;
        }
        if let Some(exit_pc) = self.blocks[block].stack.peek(Token::Pexit) {
            let stack = self.blocks[block].stack.remove(Token::Pexit);
            self.blocks[block].cond = Condition::always();
            self.blocks[block].branch_true =
                Some(self.add_label(block, stack, exit_pc, function_id));
            self.blocks[block].branch_false = None;
            return AnalysisState::Branch;
        }
        if self.exits_to_dispatcher {
            self.blocks[block].cond = Condition::always();
            self.blocks[block].end = pc;
            self.blocks[block].end_class = EndClass::Branch;
            self.blocks[block].branch_true = self.dispatch_block;
            self.blocks[block].branch_false = None;
            return AnalysisState::Branch;
        }
        self.blocks[block].end = pc.next();
        self.blocks[block].end_class = EndClass::Exit;
        AnalysisState::Branch
    }

    fn add_label(
        &mut self,
        block: usize,
        stack: FlowStack,
        pc: Location,
        function_id: usize,
    ) -> usize {
        if self.blocks[block].begin == pc {
            return block;
        }
        if let Some(existing) = self.find_block_by_begin(function_id, pc) {
            if let Some(prev) = self.previous_block(function_id, existing) {
                if self.blocks[existing].begin.virtual_loc() == self.blocks[prev].begin {
                    return prev;
                }
            }
            return existing;
        }
        if let Some(label) = self.functions[function_id]
            .labels
            .iter()
            .find(|label| label.address == pc)
        {
            return label.block;
        }
        let new_block = self.blocks.len();
        let mut flow_block = FlowBlock::new(pc);
        flow_block.stack = stack.clone();
        self.blocks.push(flow_block);
        self.functions[function_id].labels.push(FlowLabel {
            address: pc,
            block: new_block,
            stack,
        });
        new_block
    }

    fn next_block_after(&self, function_id: usize, pc: Location) -> Option<usize> {
        self.functions[function_id]
            .blocks
            .iter()
            .copied()
            .filter(|&block| self.blocks[block].begin > pc)
            .min_by_key(|&block| self.blocks[block].begin)
    }

    fn find_block_by_begin(&self, function_id: usize, pc: Location) -> Option<usize> {
        self.functions[function_id]
            .blocks
            .iter()
            .copied()
            .find(|&block| self.blocks[block].begin == pc)
    }

    fn previous_block(&self, function_id: usize, block: usize) -> Option<usize> {
        let blocks = &self.functions[function_id].blocks;
        let pos = blocks.iter().position(|&candidate| candidate == block)?;
        pos.checked_sub(1).map(|prev| blocks[prev])
    }

    fn insert_block_sorted(&mut self, function_id: usize, block: usize) {
        let blocks = &mut self.functions[function_id].blocks;
        if blocks.contains(&block) {
            return;
        }
        let pos = blocks
            .binary_search_by_key(&self.blocks[block].begin, |&id| self.blocks[id].begin)
            .unwrap_or_else(|pos| pos);
        blocks.insert(pos, block);
    }

    fn split_block(
        &mut self,
        function_id: usize,
        old_block: usize,
        new_block: usize,
        pc: Location,
    ) {
        if pc <= self.blocks[old_block].begin || pc >= self.blocks[old_block].end {
            std::panic::panic_any(crate::exception::InvalidArgument::new(format!(
                "Invalid address to split={pc}"
            )));
        }
        let mut split = self.blocks[old_block].clone();
        split.begin = pc;
        self.blocks[new_block] = split;
        self.blocks[old_block].end = pc;
        self.blocks[old_block].end_class = EndClass::Branch;
        self.blocks[old_block].cond = Condition::always();
        self.blocks[old_block].branch_true = Some(new_block);
        self.blocks[old_block].branch_false = None;
        self.insert_block_sorted(function_id, new_block);
    }

    pub fn to_cfg_blocks(&self, base_offset: u32) -> Vec<CfgBlock> {
        let Some(function) = self.functions.first() else {
            return Vec::new();
        };
        let mut index_by_block = std::collections::HashMap::new();
        for (index, &block) in function.blocks.iter().enumerate() {
            index_by_block.insert(block, index);
        }
        function
            .blocks
            .iter()
            .map(|&block| {
                let flow = &self.blocks[block];
                CfgBlock {
                    begin: location_to_word_index(flow.begin, base_offset),
                    end: location_to_word_index(flow.end, base_offset),
                    end_class: flow.end_class,
                    branch_true: flow
                        .branch_true
                        .and_then(|target| index_by_block.get(&target).copied()),
                    branch_false: flow
                        .branch_false
                        .and_then(|target| index_by_block.get(&target).copied()),
                    cond: flow.cond,
                    stack_depth: flow.stack.entries.len() as u32,
                    branch_reg: flow.branch_reg,
                    branch_offset: flow.branch_offset,
                    indirect_branches: flow
                        .indirect_branches
                        .iter()
                        .filter_map(|indirect| {
                            Some(IndirectBranch {
                                block: *index_by_block.get(&indirect.block)?,
                                address: indirect.address,
                            })
                        })
                        .collect(),
                }
            })
            .collect()
    }
}

pub fn build_cfg_from_env(
    env: &mut dyn Environment,
    base_offset: u32,
    _code_words: usize,
) -> Vec<CfgBlock> {
    let cfg = FlowCfg::new(env, Location::new(base_offset), false);
    cfg.to_cfg_blocks(base_offset)
}

fn opcode_token(opcode: MaxwellOpcode) -> Token {
    match opcode {
        MaxwellOpcode::PBK | MaxwellOpcode::BRK => Token::Pbk,
        MaxwellOpcode::PCNT | MaxwellOpcode::CONT => Token::Pcnt,
        MaxwellOpcode::PEXIT | MaxwellOpcode::EXIT => Token::Pexit,
        MaxwellOpcode::PLONGJMP | MaxwellOpcode::LONGJMP => Token::Plongjmp,
        MaxwellOpcode::PRET | MaxwellOpcode::RET | MaxwellOpcode::CAL => Token::Pret,
        MaxwellOpcode::SSY | MaxwellOpcode::SYNC => Token::Ssy,
        _ => std::panic::panic_any(crate::exception::InvalidArgument::new(format!(
            "{opcode:?}"
        ))),
    }
}

fn is_absolute_jump(opcode: MaxwellOpcode) -> bool {
    matches!(
        opcode,
        MaxwellOpcode::JCAL | MaxwellOpcode::JMP | MaxwellOpcode::JMX
    )
}

fn has_flow_test(opcode: MaxwellOpcode) -> bool {
    match opcode {
        MaxwellOpcode::BRA
        | MaxwellOpcode::BRX
        | MaxwellOpcode::EXIT
        | MaxwellOpcode::JMP
        | MaxwellOpcode::JMX
        | MaxwellOpcode::KIL
        | MaxwellOpcode::BRK
        | MaxwellOpcode::CONT
        | MaxwellOpcode::LONGJMP
        | MaxwellOpcode::RET
        | MaxwellOpcode::SYNC => true,
        MaxwellOpcode::CAL | MaxwellOpcode::JCAL => false,
        _ => std::panic::panic_any(crate::exception::InvalidArgument::new(format!(
            "Invalid branch {opcode:?}"
        ))),
    }
}

fn branch_offset(pc: Location, inst: Instruction) -> Location {
    Location::new(
        pc.offset()
            .wrapping_add(inst.branch_offset() as u32)
            .wrapping_add(8),
    )
}

fn condition_from_predicate(pred: Predicate) -> Condition {
    Condition::from_pred(IrPred::from_u8(pred.index as u8), pred.negated)
}

fn condition_from_flow(flow_test: Option<FlowTest>, pred: Predicate) -> Condition {
    Condition::new(
        flow_test.unwrap_or(FlowTest::T),
        IrPred::from_u8(pred.index as u8),
        pred.negated,
    )
}

fn location_to_word_index(location: Location, base_offset: u32) -> u32 {
    let offset = if location.is_virtual() {
        location.offset().wrapping_add(4)
    } else {
        location.offset()
    };
    offset.saturating_sub(base_offset) / 8
}

/// Build a control flow graph from Maxwell instruction words.
///
/// Returns a list of `CfgBlock` representing the flat CFG.
pub fn build_cfg(instructions: &[u64]) -> Vec<CfgBlock> {
    if instructions.is_empty() {
        return Vec::new();
    }

    // Phase 1: Find all block boundaries (branch targets and next-after-branch).
    let mut block_starts = std::collections::BTreeSet::new();
    block_starts.insert(0u32);

    let mut stack: Vec<StackEntry> = Vec::new();

    for (pc, &insn) in instructions.iter().enumerate() {
        let pc = pc as u32;
        let opcode = maxwell_opcodes::decode_opcode(insn);

        match opcode {
            Some(MaxwellOpcode::BRA) => {
                let offset = decode_branch_offset(insn);
                let target = (pc as i32 + offset + 1) as u32;
                block_starts.insert(target);
                block_starts.insert(pc + 1);
            }
            Some(MaxwellOpcode::SSY) => {
                let offset = decode_branch_offset(insn);
                let target = (pc as i32 + offset + 1) as u32;
                stack.push(StackEntry {
                    kind: StackKind::Ssy,
                    target,
                });
                block_starts.insert(target);
            }
            Some(MaxwellOpcode::PBK) => {
                let offset = decode_branch_offset(insn);
                let target = (pc as i32 + offset + 1) as u32;
                stack.push(StackEntry {
                    kind: StackKind::Pbk,
                    target,
                });
                block_starts.insert(target);
            }
            Some(MaxwellOpcode::PCNT) => {
                let offset = decode_branch_offset(insn);
                let target = (pc as i32 + offset + 1) as u32;
                stack.push(StackEntry {
                    kind: StackKind::Pcnt,
                    target,
                });
                block_starts.insert(target);
            }
            Some(MaxwellOpcode::SYNC) | Some(MaxwellOpcode::BRK) | Some(MaxwellOpcode::CONT) => {
                let kind = match opcode {
                    Some(MaxwellOpcode::SYNC) => StackKind::Ssy,
                    Some(MaxwellOpcode::BRK) => StackKind::Pbk,
                    Some(MaxwellOpcode::CONT) => StackKind::Pcnt,
                    _ => unreachable!(),
                };
                if let Some(pos) = stack.iter().rposition(|entry| entry.kind == kind) {
                    let target = stack[pos].target;
                    block_starts.insert(target);
                    let cond = decode_condition(insn, opcode.expect("matched flow opcode"));
                    if cond.is_always() {
                        stack.truncate(pos);
                    }
                }
                block_starts.insert(pc + 1);
            }
            Some(MaxwellOpcode::EXIT) | Some(MaxwellOpcode::KIL) => {
                block_starts.insert(pc + 1);
            }
            Some(MaxwellOpcode::CAL) | Some(MaxwellOpcode::JCAL) => {
                block_starts.insert(pc + 1);
            }
            Some(MaxwellOpcode::RET) => {
                block_starts.insert(pc + 1);
            }
            _ => {}
        }
    }

    // Phase 2: Create blocks from boundaries.
    let starts: Vec<u32> = block_starts.into_iter().collect();
    let mut blocks = Vec::new();
    let num_insns = instructions.len() as u32;

    for i in 0..starts.len() {
        let begin = starts[i];
        if begin >= num_insns {
            break;
        }
        let end = if i + 1 < starts.len() {
            starts[i + 1].min(num_insns)
        } else {
            num_insns
        };

        // Determine end class from last instruction in block.
        let last_pc = end - 1;
        let last_insn = instructions[last_pc as usize];
        let last_opcode = maxwell_opcodes::decode_opcode(last_insn);

        let (end_class, cond) = match last_opcode {
            Some(MaxwellOpcode::BRA) => {
                let c = decode_condition(last_insn, MaxwellOpcode::BRA);
                (EndClass::Branch, c)
            }
            Some(MaxwellOpcode::EXIT) => {
                let c = decode_condition(last_insn, MaxwellOpcode::EXIT);
                (EndClass::Exit, c)
            }
            Some(MaxwellOpcode::KIL) => {
                let c = decode_condition(last_insn, MaxwellOpcode::KIL);
                (EndClass::Kill, c)
            }
            Some(MaxwellOpcode::SYNC) => {
                let c = decode_condition(last_insn, MaxwellOpcode::SYNC);
                (EndClass::Branch, c)
            }
            Some(MaxwellOpcode::BRK) => {
                let c = decode_condition(last_insn, MaxwellOpcode::BRK);
                (EndClass::Branch, c)
            }
            Some(MaxwellOpcode::CONT) => {
                let c = decode_condition(last_insn, MaxwellOpcode::CONT);
                (EndClass::Branch, c)
            }
            Some(MaxwellOpcode::CAL) | Some(MaxwellOpcode::JCAL) => {
                (EndClass::Call, Condition::always())
            }
            Some(MaxwellOpcode::RET) => (EndClass::Return, Condition::always()),
            Some(MaxwellOpcode::BRX) | Some(MaxwellOpcode::JMX) => {
                (EndClass::IndirectBranch, Condition::always())
            }
            _ => (EndClass::Branch, Condition::always()),
        };

        blocks.push(CfgBlock {
            begin,
            end,
            end_class,
            branch_true: None,
            branch_false: None,
            cond,
            stack_depth: 0,
            branch_reg: 0,
            branch_offset: 0,
            indirect_branches: Vec::new(),
        });
    }

    // Phase 3: Link blocks (resolve branch targets to block indices).
    //
    // Upstream stores a convergence stack on each pending label. A SYNC/BRK/CONT
    // pop only applies to the taken branch target; a conditional fall-through
    // keeps the pre-pop stack. The first boundary pass above only discovers
    // possible block starts, so this phase propagates stacks through the flat
    // block graph and resolves stack-token targets from each block's entry
    // stack instead of a single global linear stack.
    let block_start_to_index: std::collections::HashMap<u32, usize> = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.begin, i))
        .collect();

    let mut entry_stacks: Vec<Option<Vec<StackEntry>>> = vec![None; blocks.len()];
    let mut worklist = std::collections::VecDeque::new();
    entry_stacks[0] = Some(Vec::new());
    worklist.push_back(0usize);

    while let Some(i) = worklist.pop_front() {
        let Some(entry_stack) = entry_stacks[i].clone() else {
            continue;
        };
        blocks[i].stack_depth = entry_stack.len() as u32;
        blocks[i].branch_true = None;
        blocks[i].branch_false = None;

        let mut block_stack = entry_stack;
        for pc in blocks[i].begin..blocks[i].end {
            let insn = instructions[pc as usize];
            match maxwell_opcodes::decode_opcode(insn) {
                Some(MaxwellOpcode::SSY) => {
                    let offset = decode_branch_offset(insn);
                    let target = (pc as i32 + offset + 1) as u32;
                    block_stack.push(StackEntry {
                        kind: StackKind::Ssy,
                        target,
                    });
                }
                Some(MaxwellOpcode::PBK) => {
                    let offset = decode_branch_offset(insn);
                    let target = (pc as i32 + offset + 1) as u32;
                    block_stack.push(StackEntry {
                        kind: StackKind::Pbk,
                        target,
                    });
                }
                Some(MaxwellOpcode::PCNT) => {
                    let offset = decode_branch_offset(insn);
                    let target = (pc as i32 + offset + 1) as u32;
                    block_stack.push(StackEntry {
                        kind: StackKind::Pcnt,
                        target,
                    });
                }
                _ => {}
            }
        }

        let last_pc = blocks[i].end - 1;
        let last_insn = instructions[last_pc as usize];
        let last_opcode = maxwell_opcodes::decode_opcode(last_insn);

        match last_opcode {
            Some(MaxwellOpcode::BRA) => {
                let offset = decode_branch_offset(last_insn);
                let target = (last_pc as i32 + offset + 1) as u32;
                blocks[i].branch_true = block_start_to_index.get(&target).copied();
                if let Some(target_index) = blocks[i].branch_true {
                    propagate_stack(&mut entry_stacks, &mut worklist, target_index, &block_stack);
                }
                // Fall-through for conditional branch
                if !blocks[i].cond.is_always() {
                    let next = blocks[i].end;
                    blocks[i].branch_false = block_start_to_index.get(&next).copied();
                    if let Some(target_index) = blocks[i].branch_false {
                        propagate_stack(
                            &mut entry_stacks,
                            &mut worklist,
                            target_index,
                            &block_stack,
                        );
                    }
                }
            }
            Some(MaxwellOpcode::SYNC) | Some(MaxwellOpcode::BRK) | Some(MaxwellOpcode::CONT) => {
                let kind = match last_opcode {
                    Some(MaxwellOpcode::SYNC) => StackKind::Ssy,
                    Some(MaxwellOpcode::BRK) => StackKind::Pbk,
                    Some(MaxwellOpcode::CONT) => StackKind::Pcnt,
                    _ => unreachable!(),
                };
                let popped = pop_stack_token(&block_stack, kind);
                if let Some((target, target_stack)) = popped.as_ref() {
                    blocks[i].branch_true = block_start_to_index.get(target).copied();
                    if let Some(target_index) = blocks[i].branch_true {
                        propagate_stack(
                            &mut entry_stacks,
                            &mut worklist,
                            target_index,
                            target_stack,
                        );
                    }
                }
                if !blocks[i].cond.is_always() {
                    let next = blocks[i].end;
                    blocks[i].branch_false = block_start_to_index.get(&next).copied();
                    if let Some(target_index) = blocks[i].branch_false {
                        propagate_stack(
                            &mut entry_stacks,
                            &mut worklist,
                            target_index,
                            &block_stack,
                        );
                    }
                }
            }
            Some(MaxwellOpcode::EXIT) | Some(MaxwellOpcode::KIL) | Some(MaxwellOpcode::RET) => {
                // No successors (or conditional fall-through)
                if !blocks[i].cond.is_always() {
                    let next = blocks[i].end;
                    blocks[i].branch_false = block_start_to_index.get(&next).copied();
                    if let Some(target_index) = blocks[i].branch_false {
                        propagate_stack(
                            &mut entry_stacks,
                            &mut worklist,
                            target_index,
                            &block_stack,
                        );
                    }
                }
            }
            _ => {
                // Fall through to next block.
                let next = blocks[i].end;
                blocks[i].branch_true = block_start_to_index.get(&next).copied();
                if let Some(target_index) = blocks[i].branch_true {
                    propagate_stack(&mut entry_stacks, &mut worklist, target_index, &block_stack);
                }
            }
        }
    }

    blocks
}

fn propagate_stack(
    entry_stacks: &mut [Option<Vec<StackEntry>>],
    worklist: &mut std::collections::VecDeque<usize>,
    target_index: usize,
    stack: &[StackEntry],
) {
    match &entry_stacks[target_index] {
        Some(existing) if existing == stack => {}
        Some(_) => {
            // Upstream AddLabel is keyed by address; if different paths reach the
            // same label with different stacks, the first label owns the stack.
        }
        None => {
            entry_stacks[target_index] = Some(stack.to_vec());
            worklist.push_back(target_index);
        }
    }
}

fn pop_stack_token(stack: &[StackEntry], kind: StackKind) -> Option<(u32, Vec<StackEntry>)> {
    let pos = stack.iter().rposition(|entry| entry.kind == kind)?;
    let target = stack[pos].target;
    Some((target, stack[..pos].to_vec()))
}

/// Decode the branch offset from a branch instruction (BRA, SSY, PBK, PCNT).
///
/// The offset is a signed 24-bit value in bits [23:5] of the instruction word,
/// or a different field depending on the encoding.
fn decode_branch_offset(insn: u64) -> i32 {
    // Upstream `Instruction::branch.offset` is `BitField<20, 24, s64>`.
    // `BranchOffset(pc, inst)` computes `pc.Offset() + inst.branch.Offset() + 8`,
    // where `Location` is byte-addressed. This Rust CFG is word-indexed, so
    // return the signed word delta; callers add `pc + delta + 1`.
    let raw = ((insn >> 20) & 0x00ff_ffff) as i32;
    let offset_bytes = (raw << 8) >> 8;
    offset_bytes / 8
}

/// Decode the predicate condition from bits [19:16] of the instruction.
fn decode_condition(insn: u64, opcode: MaxwellOpcode) -> Condition {
    let instruction = Instruction::new(insn);
    condition_from_flow(
        has_flow_test(opcode).then(|| instruction.branch_flow_test().unwrap_or(FlowTest::T)),
        instruction.pred(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::TexturePassCaches;
    use crate::program_header::ProgramHeader;
    use crate::shader_info::{ReplaceConstant, TexturePixelFormat, TextureType};
    use crate::stage::Stage;

    struct PretEnvironment {
        texture_pass_caches: TexturePassCaches,
        sph: ProgramHeader,
    }

    impl Default for PretEnvironment {
        fn default() -> Self {
            Self {
                texture_pass_caches: TexturePassCaches::default(),
                sph: ProgramHeader::default(),
            }
        }
    }

    impl Environment for PretEnvironment {
        fn texture_pass_caches(&mut self) -> &mut TexturePassCaches {
            &mut self.texture_pass_caches
        }

        fn read_instruction(&mut self, _address: u32) -> u64 {
            0xe270_0000_0007_000f
        }

        fn read_cbuf_value(&mut self, _cbuf_index: u32, _cbuf_offset: u32) -> u32 {
            0
        }

        fn read_texture_type(&mut self, _raw_handle: u32) -> TextureType {
            TextureType::Color2D
        }

        fn read_texture_pixel_format(&mut self, _raw_handle: u32) -> TexturePixelFormat {
            TexturePixelFormat::A8B8G8R8Unorm
        }

        fn is_texture_pixel_format_integer(&mut self, _raw_handle: u32) -> bool {
            false
        }

        fn read_viewport_transform_state(&mut self) -> u32 {
            0
        }

        fn texture_bound_buffer(&self) -> u32 {
            0
        }

        fn local_memory_size(&self) -> u32 {
            0
        }

        fn shared_memory_size(&self) -> u32 {
            0
        }

        fn workgroup_size(&self) -> [u32; 3] {
            [1; 3]
        }

        fn has_hle_macro_state(&self) -> bool {
            false
        }

        fn get_replace_const_buffer(
            &mut self,
            _bank: u32,
            _offset: u32,
        ) -> Option<ReplaceConstant> {
            None
        }

        fn dump(&mut self, _pipeline_hash: u64, _shader_hash: u64) {}

        fn sph(&self) -> &ProgramHeader {
            &self.sph
        }

        fn gp_passthrough_mask(&self) -> &[u32; 8] {
            static MASK: [u32; 8] = [0; 8];
            &MASK
        }

        fn shader_stage(&self) -> Stage {
            Stage::Compute
        }

        fn start_address(&self) -> u32 {
            0
        }

        fn is_proprietary_driver(&self) -> bool {
            false
        }
    }

    #[test]
    fn pret_flow_analysis_raises_typed_shader_exception() {
        let mut env = PretEnvironment::default();
        assert_eq!(
            maxwell_opcodes::decode_opcode(env.read_instruction(8)),
            Some(MaxwellOpcode::PRET)
        );

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            FlowCfg::new(&mut env, Location::new(0), false)
        }));
        let payload = match result {
            Ok(_) => panic!("PRET flow analysis must fail like upstream"),
            Err(payload) => payload,
        };
        let exception = payload
            .downcast_ref::<crate::exception::NotImplementedException>()
            .expect("PRET must use a catchable shader exception");

        assert_eq!(
            exception.to_string(),
            "PRET flow analysis is not implemented"
        );
    }

    #[test]
    fn branch_offset_uses_upstream_signed_24_bit_field() {
        let forward_two_words = 16u64 << 20;
        assert_eq!(decode_branch_offset(forward_two_words), 2);

        let backward_one_word = 0x00ff_fff8u64 << 20;
        assert_eq!(decode_branch_offset(backward_one_word), -1);
    }

    #[test]
    fn branch_condition_preserves_flow_test_and_predicate() {
        // BRA.NEU P0 from a production fragment shader. Upstream represents
        // this as IR::Condition{NEU, P0}; dropping NEU changes the branch.
        let condition = decode_condition(0xE240_0000_0680_000D, MaxwellOpcode::BRA);

        assert_eq!(condition.get_flow_test(), FlowTest::NEU);
        assert_eq!(condition.get_pred(), (IrPred::P0, false));
    }

    #[test]
    fn kil_uses_instruction_predicate_like_upstream() {
        let kil_p2_neg = 0xe330_0000_0000_0000u64 | (0b1010u64 << 16);
        let cfg = build_cfg(&[kil_p2_neg]);

        assert_eq!(cfg.len(), 1);
        assert_eq!(cfg[0].end_class, EndClass::Kill);
        assert_eq!(cfg[0].cond.get_pred(), (IrPred::P2, true));
    }

    #[test]
    fn sync_uses_matching_ssy_stack_target() {
        let ssy_to_13 = 0xe290_0000_0600_0000u64;
        let sync_p0 = 0xf0f8_0000_0000_000fu64;
        let nop = 0x50b0_0000_0007_0f00u64;
        let mut words = vec![nop; 14];
        words[0] = ssy_to_13;
        words[1] = sync_p0;

        let cfg = build_cfg(&words);
        let sync_block = cfg
            .iter()
            .find(|block| block.begin == 0 && block.end == 2)
            .expect("SSY/SYNC should remain in the first block");
        let true_target = sync_block.branch_true.expect("SYNC true target");
        let false_target = sync_block.branch_false.expect("SYNC false target");

        assert_eq!(cfg[true_target].begin, 13);
        assert_eq!(cfg[false_target].begin, 2);
        assert_eq!(sync_block.cond.get_pred(), (IrPred::P0, false));
    }

    #[test]
    fn conditional_sync_preserves_stack_for_fallthrough_path() {
        let ssy_to_13 = 0xe290_0000_0600_0000u64;
        let sync_p0 = 0xf0f8_0000_0000_000fu64;
        let sync_pt = 0xf0f8_0000_0007_000fu64;
        let nop = 0x50b0_0000_0007_0f00u64;
        let mut words = vec![nop; 14];
        words[0] = ssy_to_13;
        words[1] = sync_p0;
        words[3] = sync_pt;

        let cfg = build_cfg(&words);
        let second_sync = cfg
            .iter()
            .find(|block| block.begin == 2 && block.end == 4)
            .expect("fallthrough path should reach the second SYNC block");
        let true_target = second_sync.branch_true.expect("second SYNC true target");

        assert_eq!(cfg[true_target].begin, 13);
        assert!(second_sync.branch_false.is_none());
        assert!(second_sync.cond.is_always());
    }
}
