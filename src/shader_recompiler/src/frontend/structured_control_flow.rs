// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Structured control flow conversion.
//!
//! Ref: zuyu `frontend/maxwell/structured_control_flow.cpp`

use super::control_flow::{CfgBlock, Condition, EndClass, FlowCfg};
use crate::ir::program::SyntaxNode;
use crate::ir::value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatementType {
    Code,
    Goto,
    Label,
    If,
    Loop,
    Break,
    Return,
    Kill,
    Unreachable,
    SetVariable,
    SetIndirectBranchVariable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Expr {
    Identity(Condition),
    Not(Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Variable(u32),
    IndirectBranchCond(u32),
}

impl Expr {
    fn true_value() -> Self {
        Self::Identity(Condition::always())
    }

    fn false_value() -> Self {
        Self::Not(Box::new(Self::true_value()))
    }

    fn syntax_placeholder(&self) -> Value {
        Value::ImmU1(true)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StructuredAction {
    TranslateCode {
        block: u32,
        cfg_block: usize,
    },
    SetVariable {
        block: u32,
        id: u32,
        expr: Expr,
    },
    SetIndirectBranchVariable {
        block: u32,
        branch_reg: u32,
        branch_offset: i32,
    },
    DemoteToHelperInvocation {
        block: u32,
    },
    Condition {
        syntax_index: usize,
        block: u32,
        expr: Expr,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct StructuredSyntax {
    pub syntax: Vec<SyntaxNode>,
    pub actions: Vec<StructuredAction>,
    pub block_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Statement {
    Code {
        block: usize,
    },
    Goto {
        cond: Expr,
        label_id: u32,
    },
    Label {
        id: u32,
    },
    If {
        cond: Expr,
        children: Vec<Statement>,
    },
    Loop {
        cond: Expr,
        children: Vec<Statement>,
    },
    Break {
        cond: Expr,
    },
    Return,
    Kill,
    Unreachable,
    SetVariable {
        id: u32,
        op: Expr,
    },
    SetIndirectBranchVariable {
        branch_reg: u32,
        branch_offset: i32,
    },
}

impl Statement {
    fn statement_type(&self) -> StatementType {
        match self {
            Self::Code { .. } => StatementType::Code,
            Self::Goto { .. } => StatementType::Goto,
            Self::Label { .. } => StatementType::Label,
            Self::If { .. } => StatementType::If,
            Self::Loop { .. } => StatementType::Loop,
            Self::Break { .. } => StatementType::Break,
            Self::Return => StatementType::Return,
            Self::Kill => StatementType::Kill,
            Self::Unreachable => StatementType::Unreachable,
            Self::SetVariable { .. } => StatementType::SetVariable,
            Self::SetIndirectBranchVariable { .. } => StatementType::SetIndirectBranchVariable,
        }
    }
}

/// Convert a CFG into a structured abstract syntax list.
pub fn structure_cfg(cfg_blocks: &[CfgBlock]) -> Vec<SyntaxNode> {
    structure_cfg_detailed(cfg_blocks).syntax
}

/// Convert a CFG into structured ASL plus the IR actions that upstream
/// `TranslatePass::Visit` emits into each header/merge block.
pub(crate) fn structure_cfg_detailed(cfg_blocks: &[CfgBlock]) -> StructuredSyntax {
    structure_cfg_detailed_with_exit_convergence(cfg_blocks, false)
}

/// Upstream BuildASL/GotoPass `converge_exits` option (Eglantine 900ab5a8).
/// The task/mesh frontend must request this before SSA and epilogue emission;
/// changing only the backend Return instruction would leave divergent barriers.
pub(crate) fn structure_cfg_detailed_with_exit_convergence(
    cfg_blocks: &[CfgBlock],
    converge_exits: bool,
) -> StructuredSyntax {
    if cfg_blocks.is_empty() {
        return StructuredSyntax {
            syntax: Vec::new(),
            actions: Vec::new(),
            block_count: 0,
        };
    }

    let mut pass = GotoPass::new(cfg_blocks, converge_exits);
    pass.run();

    let mut translator = TranslatePass::new();
    translator.visit(pass.root_children(), None, None);
    translator.finish()
}

struct GotoPass {
    root: Vec<Statement>,
    goto_count: usize,
    exit_label: Option<u32>,
}

/// Upstream BuildASL over the full function graph. TranslateCode actions refer
/// to global FlowBlock indices (the matching view is FlowCfg::all_cfg_blocks).
pub(crate) fn structure_flow_cfg_detailed(cfg: &FlowCfg, converge_exits: bool) -> StructuredSyntax {
    let mut pass = GotoPass::from_flow_cfg(cfg, converge_exits);
    pass.run();
    let mut translator = TranslatePass::new();
    translator.visit(pass.root_children(), None, None);
    translator.finish()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StatementPath {
    parent: Vec<usize>,
    index: usize,
}

impl GotoPass {
    fn from_flow_cfg(cfg: &FlowCfg, converge_exits: bool) -> Self {
        let mut pass = Self {
            root: Vec::new(),
            goto_count: 0,
            exit_label: converge_exits.then_some(0),
        };
        let mut next_label = 0;
        if converge_exits {
            pass.root.push(Statement::Label { id: 0 });
            pass.root.push(Statement::Return);
            next_label = 1;
        }
        if !cfg.functions.is_empty() {
            pass.build_function_tree(cfg, 0, &mut next_label, 0, None);
        }
        pass
    }

    /// Upstream recursive BuildTree overload. Labels are local to each call
    /// expansion, even when multiple callers share the same FlowFunction.
    fn build_function_tree(
        &mut self,
        cfg: &FlowCfg,
        function: usize,
        next_label: &mut u32,
        insert_point: usize,
        return_label: Option<u32>,
    ) {
        let mut local_labels = std::collections::HashMap::new();
        for (offset, &block) in cfg.functions[function].blocks.iter().enumerate() {
            let id = *next_label;
            *next_label += 1;
            self.root
                .insert(insert_point + offset, Statement::Label { id });
            local_labels.insert(block, id);
        }
        for &block_index in &cfg.functions[function].blocks {
            let block = &cfg.blocks[block_index];
            let label = local_labels[&block_index];
            let mut ip = self.find_label_position(label).unwrap() + 1;
            self.root.insert(
                0,
                Statement::SetVariable {
                    id: label,
                    op: Expr::false_value(),
                },
            );
            ip += 1;
            self.root.insert(
                ip,
                Statement::SetVariable {
                    id: label,
                    op: Expr::false_value(),
                },
            );
            ip += 1;
            self.root.insert(ip, Statement::Code { block: block_index });
            ip += 1;
            match block.end_class {
                EndClass::Branch => {
                    if block.cond.is_always() {
                        self.insert_goto(
                            ip,
                            Expr::true_value(),
                            local_labels[&block.branch_true.unwrap()],
                        );
                    } else if block.cond == Condition::from_bool(false) {
                        self.insert_goto(
                            ip,
                            Expr::true_value(),
                            local_labels[&block.branch_false.unwrap()],
                        );
                    } else {
                        self.insert_goto(
                            ip,
                            Expr::Identity(block.cond),
                            local_labels[&block.branch_true.unwrap()],
                        );
                        self.insert_goto(
                            ip + 1,
                            Expr::true_value(),
                            local_labels[&block.branch_false.unwrap()],
                        );
                    }
                }
                EndClass::IndirectBranch => {
                    self.root.insert(
                        ip,
                        Statement::SetIndirectBranchVariable {
                            branch_reg: block.branch_reg,
                            branch_offset: block.branch_offset,
                        },
                    );
                    ip += 1;
                    for indirect in &block.indirect_branches {
                        self.insert_goto(
                            ip,
                            Expr::IndirectBranchCond(indirect.address),
                            local_labels[&indirect.block],
                        );
                        ip += 1;
                    }
                    self.root.insert(ip, Statement::Unreachable);
                }
                EndClass::Call => {
                    let continuation =
                        local_labels[&block.return_block.expect("call continuation")];
                    self.build_function_tree(
                        cfg,
                        block.function_call,
                        next_label,
                        ip,
                        Some(continuation),
                    );
                }
                EndClass::Return => self.insert_goto(
                    ip,
                    Expr::Identity(block.cond),
                    return_label.expect("RET requires a caller"),
                ),
                EndClass::Exit => {
                    if let Some(label) = self.exit_label {
                        self.insert_goto(ip, Expr::true_value(), label);
                    } else {
                        self.root.insert(ip, Statement::Return);
                    }
                }
                EndClass::Kill => self.root.insert(ip, Statement::Kill),
            }
        }
    }

    fn new(cfg_blocks: &[CfgBlock], converge_exits: bool) -> Self {
        let mut pass = Self {
            root: Vec::new(),
            goto_count: 0,
            // Rust CFG labels are block indices; reserve the first unused id
            // for upstream's synthetic exit label instead of renumbering them.
            exit_label: converge_exits.then_some(cfg_blocks.len() as u32),
        };
        pass.build_tree(cfg_blocks);
        pass
    }

    fn run(&mut self) {
        while self.goto_count > 0 {
            let Some(goto_path) = self.find_last_goto_path() else {
                break;
            };
            self.remove_goto(goto_path);
            self.goto_count -= 1;
        }
    }

    fn root_children(&self) -> &[Statement] {
        &self.root
    }

    fn build_tree(&mut self, cfg_blocks: &[CfgBlock]) {
        for block in 0..cfg_blocks.len() {
            self.root.push(Statement::Label { id: block as u32 });
        }
        if let Some(id) = self.exit_label {
            self.root.push(Statement::Label { id });
            self.root.push(Statement::Return);
        }

        for (block_index, block) in cfg_blocks.iter().enumerate() {
            let Some(label_pos) = self.find_label_position(block_index as u32) else {
                continue;
            };
            let mut ip = label_pos + 1;

            self.root.insert(
                0,
                Statement::SetVariable {
                    id: block_index as u32,
                    op: Expr::false_value(),
                },
            );
            ip += 1;

            self.root.insert(
                ip,
                Statement::SetVariable {
                    id: block_index as u32,
                    op: Expr::false_value(),
                },
            );
            ip += 1;

            self.root.insert(ip, Statement::Code { block: block_index });
            ip += 1;

            match block.end_class {
                EndClass::Branch => self.build_branch(block, &mut ip),
                EndClass::IndirectBranch => {
                    self.root.insert(
                        ip,
                        Statement::SetIndirectBranchVariable {
                            branch_reg: block.branch_reg,
                            branch_offset: block.branch_offset,
                        },
                    );
                    ip += 1;
                    for indirect in &block.indirect_branches {
                        self.insert_goto(
                            ip,
                            Expr::IndirectBranchCond(indirect.address),
                            indirect.block as u32,
                        );
                        ip += 1;
                    }
                    self.root.insert(ip, Statement::Unreachable);
                }
                EndClass::Call => {}
                EndClass::Exit if self.exit_label.is_some() => {
                    // CFGs built from the environment have already split
                    // predicated EXITs. Retain the predicate for the flat CFG
                    // builder too, matching its existing conditional semantics.
                    self.insert_goto(ip, Expr::Identity(block.cond), self.exit_label.unwrap());
                }
                EndClass::Exit if !block.cond.is_always() => {
                    self.root.insert(
                        ip,
                        Statement::If {
                            cond: Expr::Identity(block.cond),
                            children: vec![Statement::Return],
                        },
                    );
                }
                EndClass::Exit | EndClass::Return => self.root.insert(ip, Statement::Return),
                EndClass::Kill => self.root.insert(ip, Statement::Kill),
            }
        }
    }

    fn build_branch(&mut self, block: &CfgBlock, ip: &mut usize) {
        if block.cond.is_always() {
            if let Some(true_target) = block.branch_true {
                self.insert_goto(*ip, Expr::true_value(), true_target as u32);
            }
            return;
        }

        if let Some(true_target) = block.branch_true {
            self.insert_goto(*ip, Expr::Identity(block.cond), true_target as u32);
            *ip += 1;
        }
        if let Some(false_target) = block.branch_false {
            self.insert_goto(*ip, Expr::true_value(), false_target as u32);
        }
    }

    fn insert_goto(&mut self, index: usize, cond: Expr, label_id: u32) {
        self.root.insert(index, Statement::Goto { cond, label_id });
        self.goto_count += 1;
    }

    fn remove_goto(&mut self, goto_path: StatementPath) {
        let Some(Statement::Goto { label_id, .. }) = self.statement_at(&goto_path).cloned() else {
            return;
        };
        let Some(mut label_path) = self.find_label_path(label_id) else {
            return;
        };
        let mut goto_path = goto_path;

        if self.is_indirectly_related(&goto_path, &label_path) {
            while !self.is_directly_related(&goto_path, &label_path) {
                let Some(new_path) = self.move_outward(goto_path) else {
                    return;
                };
                goto_path = new_path;
                label_path = self.find_label_path(label_id).unwrap_or(label_path);
            }
        }

        if self.is_directly_related(&goto_path, &label_path) {
            let label_level = Self::level(&label_path);
            let mut goto_level = Self::level(&goto_path);
            if goto_level > label_level {
                while goto_level > label_level {
                    let Some(new_path) = self.move_outward(goto_path) else {
                        return;
                    };
                    goto_path = new_path;
                    label_path = self.find_label_path(label_id).unwrap_or(label_path);
                    goto_level -= 1;
                }
            } else if goto_level < label_level {
                if self.needs_lift(&goto_path, &label_path) {
                    let Some(new_path) = self.lift(goto_path, &label_path) else {
                        return;
                    };
                    goto_path = new_path;
                    label_path = self.find_label_path(label_id).unwrap_or(label_path);
                }
                while goto_level < label_level {
                    let Some(new_path) = self.move_inward(goto_path, &label_path) else {
                        return;
                    };
                    goto_path = new_path;
                    label_path = self.find_label_path(label_id).unwrap_or(label_path);
                    goto_level += 1;
                }
            }
        }

        if goto_path.parent != label_path.parent {
            return;
        }

        let goto_pos = goto_path.index;
        let label_pos = label_path.index;

        if goto_pos + 1 == label_pos {
            self.tree_mut(&goto_path.parent).remove(goto_pos);
        } else if goto_pos < label_pos {
            self.eliminate_as_conditional(&goto_path.parent, goto_pos, label_pos);
        } else {
            self.eliminate_as_loop(&goto_path.parent, goto_pos, label_pos);
        }
    }

    fn eliminate_as_conditional(
        &mut self,
        parent_path: &[usize],
        goto_pos: usize,
        label_pos: usize,
    ) {
        let tree = self.tree_mut(parent_path);
        let Statement::Goto { cond, .. } = tree[goto_pos].clone() else {
            return;
        };
        let children: Vec<_> = tree.drain(goto_pos + 1..label_pos).collect();
        tree.insert(
            goto_pos,
            Statement::If {
                cond: Expr::Not(Box::new(cond)),
                children,
            },
        );
        tree.remove(goto_pos + 1);
    }

    fn eliminate_as_loop(&mut self, parent_path: &[usize], goto_pos: usize, label_pos: usize) {
        let tree = self.tree_mut(parent_path);
        let Statement::Goto { cond, .. } = tree[goto_pos].clone() else {
            return;
        };
        let children: Vec<_> = tree.drain(label_pos..goto_pos).collect();
        let insert_pos = label_pos;
        tree.insert(insert_pos, Statement::Loop { cond, children });
        tree.remove(insert_pos + 1);
    }

    fn find_label_position(&self, label_id: u32) -> Option<usize> {
        self.root
            .iter()
            .position(|statement| matches!(statement, Statement::Label { id } if *id == label_id))
    }

    fn find_last_goto_path(&self) -> Option<StatementPath> {
        find_last_goto_in_tree(&self.root, &mut Vec::new())
    }

    fn level(path: &StatementPath) -> usize {
        path.parent.len() + 1
    }

    fn is_directly_related(&self, goto_path: &StatementPath, label_path: &StatementPath) -> bool {
        let goto_level = Self::level(goto_path);
        let label_level = Self::level(label_path);
        let (min_path, max_path, min_level, max_level) = if label_level < goto_level {
            (label_path, goto_path, label_level, goto_level)
        } else {
            (goto_path, label_path, goto_level, label_level)
        };
        let mut max_parent = max_path.parent.clone();
        for _ in min_level..max_level {
            let Some(parent_index) = max_parent.pop() else {
                return false;
            };
            let _ = parent_index;
        }
        min_path.parent == max_parent
    }

    fn is_indirectly_related(&self, goto_path: &StatementPath, label_path: &StatementPath) -> bool {
        goto_path.parent != label_path.parent && !self.is_directly_related(goto_path, label_path)
    }

    fn sibling_from_nephew(
        &self,
        uncle_path: &StatementPath,
        nephew_path: &StatementPath,
    ) -> Option<usize> {
        if nephew_path.parent.len() < uncle_path.parent.len() {
            return None;
        }
        nephew_path.parent.get(uncle_path.parent.len()).copied()
    }

    fn are_ordered(&self, parent_path: &[usize], left: usize, right: usize) -> bool {
        let tree = self.tree(parent_path);
        right < tree.len() && left <= right
    }

    fn needs_lift(&self, goto_path: &StatementPath, label_path: &StatementPath) -> bool {
        let Some(sibling) = self.sibling_from_nephew(goto_path, label_path) else {
            return false;
        };
        self.are_ordered(&goto_path.parent, sibling, goto_path.index)
    }

    fn move_outward(&mut self, goto_path: StatementPath) -> Option<StatementPath> {
        let (&parent_index, parent_parent_path) = goto_path.parent.split_last()?;
        let parent_parent_path = parent_parent_path.to_vec();
        match self
            .tree(&parent_parent_path)
            .get(parent_index)?
            .statement_type()
        {
            StatementType::If => self.move_outward_if(goto_path, &parent_parent_path, parent_index),
            StatementType::Loop => {
                self.move_outward_loop(goto_path, &parent_parent_path, parent_index)
            }
            _ => None,
        }
    }

    fn move_outward_if(
        &mut self,
        goto_path: StatementPath,
        parent_parent_path: &[usize],
        if_index: usize,
    ) -> Option<StatementPath> {
        let Statement::Goto { cond, label_id } = self.statement_at(&goto_path)?.clone() else {
            return None;
        };
        let if_tree = self.tree_mut(&goto_path.parent);
        if_tree.insert(
            goto_path.index,
            Statement::SetVariable {
                id: label_id,
                op: cond,
            },
        );
        let mut if_body: Vec<_> = if_tree.drain(goto_path.index + 2..).collect();
        if !if_body.is_empty() {
            if_tree.insert(
                goto_path.index + 1,
                Statement::If {
                    cond: Expr::Not(Box::new(Expr::Variable(label_id))),
                    children: std::mem::take(&mut if_body),
                },
            );
            if_tree.remove(goto_path.index + 2);
        } else {
            if_tree.remove(goto_path.index + 1);
        }

        let parent_tree = self.tree_mut(parent_parent_path);
        let insert_pos = if_index + 1;
        parent_tree.insert(
            insert_pos,
            Statement::Goto {
                cond: Expr::Variable(label_id),
                label_id,
            },
        );
        Some(StatementPath {
            parent: parent_parent_path.to_vec(),
            index: insert_pos,
        })
    }

    fn move_outward_loop(
        &mut self,
        goto_path: StatementPath,
        parent_parent_path: &[usize],
        loop_index: usize,
    ) -> Option<StatementPath> {
        let Statement::Goto { cond, label_id } = self.statement_at(&goto_path)?.clone() else {
            return None;
        };
        let loop_tree = self.tree_mut(&goto_path.parent);
        loop_tree.insert(
            goto_path.index,
            Statement::SetVariable {
                id: label_id,
                op: cond,
            },
        );
        loop_tree.insert(
            goto_path.index + 1,
            Statement::Break {
                cond: Expr::Variable(label_id),
            },
        );
        loop_tree.remove(goto_path.index + 2);

        let parent_tree = self.tree_mut(parent_parent_path);
        let insert_pos = loop_index + 1;
        parent_tree.insert(
            insert_pos,
            Statement::Goto {
                cond: Expr::Variable(label_id),
                label_id,
            },
        );
        Some(StatementPath {
            parent: parent_parent_path.to_vec(),
            index: insert_pos,
        })
    }

    fn move_inward(
        &mut self,
        goto_path: StatementPath,
        label_path: &StatementPath,
    ) -> Option<StatementPath> {
        let Statement::Goto { cond, label_id } = self.statement_at(&goto_path)?.clone() else {
            return None;
        };
        let label_nested_index = self.sibling_from_nephew(&goto_path, label_path)?;
        let tree = self.tree_mut(&goto_path.parent);
        tree.insert(
            goto_path.index,
            Statement::SetVariable {
                id: label_id,
                op: cond,
            },
        );
        let adjusted_label_nested_index = if label_nested_index > goto_path.index {
            label_nested_index + 1
        } else {
            label_nested_index
        };
        let if_body: Vec<_> = tree
            .drain(goto_path.index + 2..adjusted_label_nested_index)
            .collect();
        let if_body_len = if_body.len();
        let nested_index_after_drain = adjusted_label_nested_index - if_body_len;
        let nested_index = if !if_body.is_empty() {
            tree.insert(
                goto_path.index + 1,
                Statement::If {
                    cond: Expr::Not(Box::new(Expr::Variable(label_id))),
                    children: if_body,
                },
            );
            tree.remove(goto_path.index + 2);
            nested_index_after_drain
        } else {
            tree.remove(goto_path.index + 1);
            nested_index_after_drain - 1
        };
        match &mut tree[nested_index] {
            Statement::If {
                cond: nested_cond,
                children,
            } => {
                *nested_cond = Expr::Or(
                    Box::new(Expr::Variable(label_id)),
                    Box::new(nested_cond.clone()),
                );
                children.insert(
                    0,
                    Statement::Goto {
                        cond: Expr::Variable(label_id),
                        label_id,
                    },
                );
            }
            Statement::Loop { children, .. } => {
                children.insert(
                    0,
                    Statement::Goto {
                        cond: Expr::Variable(label_id),
                        label_id,
                    },
                );
            }
            _ => return None,
        }
        let mut parent = goto_path.parent;
        parent.push(nested_index);
        Some(StatementPath { parent, index: 0 })
    }

    fn lift(
        &mut self,
        goto_path: StatementPath,
        label_path: &StatementPath,
    ) -> Option<StatementPath> {
        let Statement::Goto { cond, label_id } = self.statement_at(&goto_path)?.clone() else {
            return None;
        };
        let label_nested_index = self.sibling_from_nephew(&goto_path, label_path)?;
        let tree = self.tree_mut(&goto_path.parent);
        if tree[label_nested_index..goto_path.index]
            .iter()
            .any(contains_break)
        {
            return None;
        }
        let loop_body: Vec<_> = tree.drain(label_nested_index..goto_path.index).collect();
        let insert_pos = label_nested_index;
        tree.insert(
            insert_pos,
            Statement::Loop {
                cond: Expr::Variable(label_id),
                children: loop_body,
            },
        );
        tree.remove(insert_pos + 1);
        if let Statement::Loop { children, .. } = &mut tree[insert_pos] {
            children.insert(
                0,
                Statement::Goto {
                    cond: Expr::Variable(label_id),
                    label_id,
                },
            );
            children.push(Statement::SetVariable {
                id: label_id,
                op: cond,
            });
        }
        let mut parent = goto_path.parent;
        parent.push(insert_pos);
        Some(StatementPath { parent, index: 0 })
    }

    fn find_label_path(&self, label_id: u32) -> Option<StatementPath> {
        find_label_in_tree(&self.root, &mut Vec::new(), label_id)
    }

    fn statement_at(&self, path: &StatementPath) -> Option<&Statement> {
        self.tree(&path.parent).get(path.index)
    }

    fn tree(&self, path: &[usize]) -> &[Statement] {
        let mut tree = self.root.as_slice();
        for &index in path {
            tree = match tree.get(index) {
                Some(Statement::If { children, .. }) | Some(Statement::Loop { children, .. }) => {
                    children.as_slice()
                }
                _ => &[],
            };
        }
        tree
    }

    fn tree_mut(&mut self, path: &[usize]) -> &mut Vec<Statement> {
        tree_mut_at(&mut self.root, path)
    }
}

fn tree_mut_at<'a>(tree: &'a mut Vec<Statement>, path: &[usize]) -> &'a mut Vec<Statement> {
    if path.is_empty() {
        return tree;
    }
    let (head, tail) = path.split_first().expect("non-empty path");
    match &mut tree[*head] {
        Statement::If { children, .. } | Statement::Loop { children, .. } => {
            tree_mut_at(children, tail)
        }
        _ => panic!("path does not point to a statement with children"),
    }
}

/// Tree scan with a backtracking path buffer: the path Vec is cloned only
/// when a match is recorded, not per visited node. The per-node clones the
/// naive version did dominated shader translation for large CFGs (Vec grow
/// churn under `structure_cfg_detailed` in GPU-thread profiles; worst
/// observed shader: 1.77s to translate).
fn find_last_goto_in_tree(tree: &[Statement], parent: &mut Vec<usize>) -> Option<StatementPath> {
    let mut result = None;
    for (index, statement) in tree.iter().enumerate() {
        match statement {
            Statement::Goto { .. } => {
                result = Some(StatementPath {
                    parent: parent.clone(),
                    index,
                });
            }
            Statement::If { children, .. } | Statement::Loop { children, .. } => {
                parent.push(index);
                if let Some(path) = find_last_goto_in_tree(children, parent) {
                    result = Some(path);
                }
                parent.pop();
            }
            _ => {}
        }
    }
    result
}

fn find_label_in_tree(
    tree: &[Statement],
    parent: &mut Vec<usize>,
    label_id: u32,
) -> Option<StatementPath> {
    for (index, statement) in tree.iter().enumerate() {
        if matches!(statement, Statement::Label { id } if *id == label_id) {
            return Some(StatementPath {
                parent: parent.clone(),
                index,
            });
        }
        match statement {
            Statement::If { children, .. } | Statement::Loop { children, .. } => {
                parent.push(index);
                if let Some(path) = find_label_in_tree(children, parent, label_id) {
                    return Some(path);
                }
                parent.pop();
            }
            _ => {}
        }
    }
    None
}

fn contains_break(statement: &Statement) -> bool {
    match statement {
        Statement::Break { .. } => true,
        Statement::If { children, .. } | Statement::Loop { children, .. } => {
            children.iter().any(contains_break)
        }
        _ => false,
    }
}

struct TranslatePass {
    syntax: Vec<SyntaxNode>,
    actions: Vec<StructuredAction>,
    next_block: u32,
}

impl TranslatePass {
    fn new() -> Self {
        Self {
            syntax: Vec::new(),
            actions: Vec::new(),
            next_block: 0,
        }
    }

    fn finish(self) -> StructuredSyntax {
        StructuredSyntax {
            syntax: self.syntax,
            actions: self.actions,
            block_count: self.next_block as usize,
        }
    }

    fn create_block(&mut self) -> u32 {
        let block = self.next_block;
        self.next_block += 1;
        block
    }

    fn ensure_block(&mut self, current_block: &mut Option<u32>) -> u32 {
        if let Some(block) = *current_block {
            return block;
        }
        let block = self.create_block();
        self.syntax.push(SyntaxNode::Block(block));
        *current_block = Some(block);
        block
    }

    fn merge_block(&mut self) -> u32 {
        // Upstream returns a fresh IR block even when it can attach it to an
        // existing forward statement. Keeping merge blocks explicit preserves
        // SPIR-V dominance and avoids detached selection/loop merge nodes.
        self.create_block()
    }

    fn visit(
        &mut self,
        tree: &[Statement],
        break_block: Option<u32>,
        fallthrough_block: Option<u32>,
    ) {
        // Upstream `TranslatePass::Visit` owns a local current block for each
        // recursive tree. Sharing it across recursion emits the child's
        // fallthrough block once in the child and again in the parent.
        let mut current_block = None;
        for statement in tree {
            match statement {
                Statement::Label { .. } => {}
                Statement::Code { block } => {
                    let ir_block = self.ensure_block(&mut current_block);
                    self.actions.push(StructuredAction::TranslateCode {
                        block: ir_block,
                        cfg_block: *block,
                    });
                }
                Statement::SetVariable { id, op } => {
                    let block = self.ensure_block(&mut current_block);
                    self.actions.push(StructuredAction::SetVariable {
                        block,
                        id: *id,
                        expr: op.clone(),
                    });
                }
                Statement::SetIndirectBranchVariable {
                    branch_reg,
                    branch_offset,
                } => {
                    let block = self.ensure_block(&mut current_block);
                    self.actions
                        .push(StructuredAction::SetIndirectBranchVariable {
                            block,
                            branch_reg: *branch_reg,
                            branch_offset: *branch_offset,
                        });
                }
                Statement::If { cond, children } => {
                    let header_block = self.ensure_block(&mut current_block);
                    let merge = self.merge_block();
                    let if_index = self.syntax.len();
                    self.syntax.push(SyntaxNode::If {
                        cond: cond.syntax_placeholder(),
                        body: merge,
                        merge,
                    });
                    self.actions.push(StructuredAction::Condition {
                        syntax_index: if_index,
                        block: header_block,
                        expr: cond.clone(),
                    });

                    let body_index = self.syntax.len();
                    self.visit(children, break_block, Some(merge));
                    let body = first_block_from_syntax(&self.syntax[body_index..]).unwrap_or(merge);
                    if let SyntaxNode::If { body: slot, .. } = &mut self.syntax[if_index] {
                        *slot = body;
                    }

                    self.syntax.push(SyntaxNode::EndIf { merge });
                    self.syntax.push(SyntaxNode::Block(merge));
                    current_block = Some(merge);
                }
                Statement::Loop { cond, children } => {
                    let loop_header = self.create_block();
                    self.syntax.push(SyntaxNode::Block(loop_header));

                    let continue_block = self.create_block();
                    let merge = self.merge_block();
                    let loop_index = self.syntax.len();
                    self.syntax.push(SyntaxNode::Loop {
                        body: merge,
                        continue_block,
                        merge,
                    });

                    let body_index = self.syntax.len();
                    self.visit(children, Some(merge), Some(continue_block));
                    let body = first_block_from_syntax(&self.syntax[body_index..]).unwrap_or(merge);
                    if let SyntaxNode::Loop { body: slot, .. } = &mut self.syntax[loop_index] {
                        *slot = body;
                    }

                    self.syntax.push(SyntaxNode::Block(continue_block));
                    let repeat_index = self.syntax.len();
                    self.syntax.push(SyntaxNode::Repeat {
                        cond: cond.syntax_placeholder(),
                        loop_header,
                        merge,
                    });
                    self.actions.push(StructuredAction::Condition {
                        syntax_index: repeat_index,
                        block: continue_block,
                        expr: cond.clone(),
                    });

                    self.syntax.push(SyntaxNode::Block(merge));
                    current_block = Some(merge);
                }
                Statement::Break { cond } => {
                    let header_block = self.ensure_block(&mut current_block);
                    let skip = self.merge_block();
                    let merge = break_block.unwrap_or(skip);
                    let break_index = self.syntax.len();
                    self.syntax.push(SyntaxNode::Break {
                        cond: cond.syntax_placeholder(),
                        merge,
                        skip,
                    });
                    self.actions.push(StructuredAction::Condition {
                        syntax_index: break_index,
                        block: header_block,
                        expr: cond.clone(),
                    });
                    self.syntax.push(SyntaxNode::Block(skip));
                    current_block = Some(skip);
                }
                Statement::Return => {
                    self.ensure_block(&mut current_block);
                    let return_block = self.create_block();
                    self.syntax.push(SyntaxNode::Block(return_block));
                    current_block = None;
                    self.syntax.push(SyntaxNode::Return);
                }
                Statement::Kill => {
                    let block = self.ensure_block(&mut current_block);
                    self.actions
                        .push(StructuredAction::DemoteToHelperInvocation { block });
                    let demote_block = self.merge_block();
                    self.syntax.push(SyntaxNode::Block(demote_block));
                    current_block = Some(demote_block);
                }
                Statement::Unreachable | Statement::Goto { .. } => {
                    self.ensure_block(&mut current_block);
                    current_block = None;
                    self.syntax.push(SyntaxNode::Unreachable);
                }
            }
        }

        // Upstream adds a CFG branch to `fallthrough_block` here; the parent
        // emits that block exactly once after Visit returns. The Rust CFG is
        // rebuilt from this syntax immediately afterwards, so emitting the
        // block in the child would duplicate its SPIR-V OpLabel.
        if current_block.is_some() && fallthrough_block.is_none() {
            self.syntax.push(SyntaxNode::Unreachable);
        }
    }
}

fn first_block_from_syntax(nodes: &[SyntaxNode]) -> Option<u32> {
    nodes.iter().find_map(|node| match node {
        SyntaxNode::Block(block) => Some(*block),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(end_class: EndClass, cond: Condition) -> CfgBlock {
        CfgBlock {
            begin: 0,
            end: 1,
            end_class,
            branch_true: None,
            branch_false: None,
            cond,
            stack_depth: 0,
            branch_reg: 0,
            branch_offset: 0,
            indirect_branches: Vec::new(),
        }
    }

    #[test]
    fn indirect_branch_materializes_each_resolved_target() {
        let mut indirect = block(EndClass::IndirectBranch, Condition::always());
        indirect.branch_reg = 12;
        indirect.branch_offset = 0x201_0080;
        indirect.indirect_branches = vec![
            super::super::control_flow::IndirectBranch {
                block: 1,
                address: 0x201_0498,
            },
            super::super::control_flow::IndirectBranch {
                block: 2,
                address: 0x201_04A8,
            },
        ];
        let cfg = vec![
            indirect,
            block(EndClass::Return, Condition::always()),
            block(EndClass::Return, Condition::always()),
        ];

        let structured = structure_cfg_detailed(&cfg);

        assert!(structured.actions.iter().any(|action| matches!(
            action,
            StructuredAction::SetIndirectBranchVariable {
                branch_reg: 12,
                branch_offset: 0x201_0080,
                ..
            }
        )));
        fn contains_target(expr: &Expr, target: u32) -> bool {
            match expr {
                Expr::IndirectBranchCond(address) => *address == target,
                Expr::Not(expr) => contains_target(expr, target),
                Expr::Or(lhs, rhs) => contains_target(lhs, target) || contains_target(rhs, target),
                Expr::Identity(_) | Expr::Variable(_) => false,
            }
        }
        for target in [0x201_0498, 0x201_04A8] {
            assert!(structured.actions.iter().any(|action| {
                match action {
                    StructuredAction::Condition { expr, .. }
                    | StructuredAction::SetVariable { expr, .. } => contains_target(expr, target),
                    _ => false,
                }
            }));
        }
    }

    #[test]
    fn conditional_exit_does_not_emit_flat_return() {
        let mut cfg_block = block(
            EndClass::Exit,
            Condition::from_pred(crate::ir::condition::IrPred::P0, false),
        );
        cfg_block.branch_false = Some(1);
        let syntax = structure_cfg(&[cfg_block, block(EndClass::Return, Condition::always())]);

        assert!(
            matches!(
                syntax.as_slice(),
                [
                    SyntaxNode::Block(0),
                    SyntaxNode::If { .. },
                    SyntaxNode::Block(2),
                    SyntaxNode::Block(3),
                    SyntaxNode::Return,
                    SyntaxNode::EndIf { merge: 1 },
                    SyntaxNode::Block(1),
                    SyntaxNode::Block(4),
                    SyntaxNode::Return
                ]
            ),
            "{syntax:#?}"
        );
    }

    #[test]
    fn unconditional_exit_still_returns() {
        let syntax = structure_cfg(&[block(EndClass::Exit, Condition::always())]);

        assert!(
            matches!(
                syntax.as_slice(),
                [
                    SyntaxNode::Block(0),
                    SyntaxNode::Block(1),
                    SyntaxNode::Return
                ]
            ),
            "{syntax:#?}"
        );
    }

    #[test]
    fn converged_conditional_exits_skip_later_code_and_share_outer_return() {
        use crate::ir::condition::IrPred;
        let cfg = [
            block(EndClass::Exit, Condition::from_pred(IrPred::P0, false)),
            block(EndClass::Exit, Condition::from_pred(IrPred::P1, false)),
            block(EndClass::Exit, Condition::always()),
        ];
        for (predicates, expected) in [
            ([false, false], vec![0, 1, 2]),
            ([false, true], vec![0, 1]),
            ([true, false], vec![0]),
            ([true, true], vec![0]),
        ] {
            assert_converged_trace(&cfg, predicates, &expected);
        }
        // Ordinary programs retain their existing multiple-return behavior.
        assert_eq!(
            structure_cfg(&cfg)
                .iter()
                .filter(|n| matches!(n, SyntaxNode::Return))
                .count(),
            3
        );
    }

    #[test]
    fn converged_nested_branches_reach_return_outside_all_selections() {
        use crate::ir::condition::IrPred;
        let mut outer = block(EndClass::Branch, Condition::from_pred(IrPred::P0, false));
        outer.branch_true = Some(1);
        outer.branch_false = Some(4);
        let mut inner = block(EndClass::Branch, Condition::from_pred(IrPred::P1, false));
        inner.branch_true = Some(2);
        inner.branch_false = Some(3);
        let cfg = [
            outer,
            inner,
            block(EndClass::Exit, Condition::always()),
            block(EndClass::Exit, Condition::always()),
            block(EndClass::Exit, Condition::always()),
        ];
        assert_converged_trace(&cfg, [false, false], &[0, 4]);
        assert_converged_trace(&cfg, [false, true], &[0, 4]);
        assert_converged_trace(&cfg, [true, false], &[0, 1, 3]);
        assert_converged_trace(&cfg, [true, true], &[0, 1, 2]);
    }

    #[test]
    fn converged_exit_inside_loop_reaches_return_outside_loop() {
        use crate::ir::condition::IrPred;
        let mut branch = block(EndClass::Branch, Condition::from_pred(IrPred::P0, false));
        branch.branch_true = Some(1);
        branch.branch_false = Some(3);
        let early_exit = block(EndClass::Exit, Condition::from_pred(IrPred::P1, false));
        let mut back_edge = block(EndClass::Branch, Condition::always());
        back_edge.branch_true = Some(0);
        let cfg = [
            branch,
            early_exit,
            back_edge,
            block(EndClass::Exit, Condition::always()),
        ];
        assert_converged_trace(&cfg, [false, false], &[0, 3]);
        assert_converged_trace(&cfg, [true, true], &[0, 1]);
    }

    fn assert_converged_trace(cfg: &[CfgBlock], predicates: [bool; 2], expected: &[usize]) {
        let structured = structure_cfg_detailed_with_exit_convergence(cfg, true);
        let mut depth = 0;
        let mut returns = 0;
        for node in &structured.syntax {
            match node {
                SyntaxNode::If { .. } | SyntaxNode::Loop { .. } => depth += 1,
                SyntaxNode::EndIf { .. } | SyntaxNode::Repeat { .. } => depth -= 1,
                SyntaxNode::Return => {
                    assert_eq!(depth, 0, "{:#?}", structured.syntax);
                    returns += 1;
                }
                _ => {}
            }
        }
        assert_eq!(returns, 1);
        let mut pass = GotoPass::new(cfg, true);
        pass.run();
        let mut trace = Vec::new();
        let mut variables = std::collections::HashMap::new();
        assert_eq!(
            visit_test_tree(&pass.root, predicates, &mut variables, &mut trace, &mut 0),
            2
        );
        assert_eq!(trace, expected);
    }

    fn test_expr(
        expr: &Expr,
        predicates: [bool; 2],
        variables: &std::collections::HashMap<u32, bool>,
    ) -> bool {
        match expr {
            Expr::Identity(condition) => {
                assert_eq!(condition.get_flow_test(), crate::ir::flow_test::FlowTest::T);
                let (predicate, negate) = condition.get_pred();
                let value = if predicate == crate::ir::condition::IrPred::PT {
                    true
                } else {
                    predicates[predicate.index()]
                };
                value ^ negate
            }
            Expr::Not(expr) => !test_expr(expr, predicates, variables),
            Expr::Or(a, b) => {
                test_expr(a, predicates, variables) || test_expr(b, predicates, variables)
            }
            Expr::Variable(id) => *variables.get(id).expect("goto variable initialized"),
            Expr::IndirectBranchCond(_) => panic!("not an indirect test"),
        }
    }

    // Execute the transformed statement tree to check which guest blocks still
    // execute after predicated exits. 0=fallthrough, 1=break, 2=return.
    fn visit_test_tree(
        tree: &[Statement],
        predicates: [bool; 2],
        variables: &mut std::collections::HashMap<u32, bool>,
        trace: &mut Vec<usize>,
        steps: &mut usize,
    ) -> u8 {
        for statement in tree {
            *steps += 1;
            assert!(*steps < 500, "unexpected nontermination");
            match statement {
                Statement::Code { block } => trace.push(*block),
                Statement::Label { .. } => {}
                Statement::SetVariable { id, op } => {
                    let value = test_expr(op, predicates, variables);
                    variables.insert(*id, value);
                }
                Statement::If { cond, children } => {
                    if test_expr(cond, predicates, variables) {
                        let control =
                            visit_test_tree(children, predicates, variables, trace, steps);
                        if control != 0 {
                            return control;
                        }
                    }
                }
                Statement::Loop { cond, children } => loop {
                    match visit_test_tree(children, predicates, variables, trace, steps) {
                        2 => return 2,
                        1 => break,
                        _ => {}
                    }
                    if !test_expr(cond, predicates, variables) {
                        break;
                    }
                },
                Statement::Break { cond } => {
                    if test_expr(cond, predicates, variables) {
                        return 1;
                    }
                }
                Statement::Return => return 2,
                _ => panic!("unexpected structured statement {statement:?}"),
            }
        }
        0
    }

    #[test]
    fn conditional_forward_branch_bodies_use_inverted_condition() {
        let mut branch = block(
            EndClass::Branch,
            Condition::from_pred(crate::ir::condition::IrPred::P0, false),
        );
        branch.branch_true = Some(2);
        branch.branch_false = Some(1);
        let branch_cond = branch.cond;

        let structured = structure_cfg_detailed(&[
            branch,
            block(EndClass::Branch, Condition::always()),
            block(EndClass::Return, Condition::always()),
        ]);

        assert!(
            matches!(
                structured.syntax.as_slice(),
                [
                    SyntaxNode::Block(0),
                    SyntaxNode::If {
                        body: 2,
                        merge: 1,
                        ..
                    },
                    SyntaxNode::Block(2),
                    SyntaxNode::EndIf { merge: 1 },
                    SyntaxNode::Block(1),
                    SyntaxNode::Block(3),
                    SyntaxNode::Return,
                ]
            ),
            "{:#?}",
            structured.syntax
        );
        assert!(structured.actions.iter().any(|action| matches!(
            action,
            StructuredAction::Condition {
                expr: Expr::Not(inner),
                ..
            } if matches!(inner.as_ref(), Expr::Identity(cond) if *cond == branch_cond)
        )));
    }

    #[test]
    fn loop_emits_continue_block_once_like_upstream_visit() {
        let mut pass = TranslatePass::new();
        pass.visit(
            &[Statement::Loop {
                cond: Expr::true_value(),
                children: vec![Statement::Code { block: 0 }],
            }],
            None,
            None,
        );
        let structured = pass.finish();
        let continue_block = structured
            .syntax
            .iter()
            .find_map(|node| match node {
                SyntaxNode::Loop { continue_block, .. } => Some(*continue_block),
                _ => None,
            })
            .expect("loop node was not emitted");

        assert_eq!(
            structured
                .syntax
                .iter()
                .filter(|node| matches!(node, SyntaxNode::Block(block) if *block == continue_block))
                .count(),
            1
        );
    }

    #[test]
    fn nested_goto_is_moved_outward_from_if_before_elimination() {
        let mut pass = GotoPass {
            root: vec![
                Statement::If {
                    cond: Expr::true_value(),
                    children: vec![
                        Statement::Code { block: 0 },
                        Statement::Goto {
                            cond: Expr::true_value(),
                            label_id: 1,
                        },
                        Statement::Code { block: 1 },
                    ],
                },
                Statement::Label { id: 1 },
                Statement::Code { block: 2 },
            ],
            goto_count: 1,
            exit_label: None,
        };

        pass.run();

        assert_eq!(count_gotos(pass.root_children()), 0);
        assert!(matches!(
            pass.root_children(),
            [
                Statement::If { children, .. },
                Statement::Label { id: 1 },
                Statement::Code { block: 2 },
            ] if matches!(
                children.as_slice(),
                [
                    Statement::Code { block: 0 },
                    Statement::SetVariable { id: 1, .. },
                    Statement::If { children, .. },
                ] if matches!(children.as_slice(), [Statement::Code { block: 1 }])
            )
        ));
    }

    #[test]
    fn goto_moves_inward_to_nested_label() {
        let mut pass = GotoPass {
            root: vec![
                Statement::Goto {
                    cond: Expr::true_value(),
                    label_id: 1,
                },
                Statement::If {
                    cond: Expr::true_value(),
                    children: vec![Statement::Label { id: 1 }, Statement::Code { block: 0 }],
                },
                Statement::Code { block: 1 },
            ],
            goto_count: 1,
            exit_label: None,
        };

        pass.run();

        assert_eq!(count_gotos(pass.root_children()), 0);
        assert!(matches!(
            pass.root_children(),
            [
                Statement::SetVariable { id: 1, .. },
                Statement::If {
                    cond: Expr::Or(_, _),
                    children,
                },
                Statement::Code { block: 1 },
            ] if matches!(
                children.as_slice(),
                [Statement::Label { id: 1 }, Statement::Code { block: 0 }]
            )
        ));
    }

    #[test]
    fn goto_lifts_when_nested_label_sibling_precedes_goto() {
        let mut pass = GotoPass {
            root: vec![
                Statement::If {
                    cond: Expr::true_value(),
                    children: vec![Statement::Label { id: 1 }, Statement::Code { block: 0 }],
                },
                Statement::Code { block: 1 },
                Statement::Goto {
                    cond: Expr::true_value(),
                    label_id: 1,
                },
                Statement::Code { block: 2 },
            ],
            goto_count: 1,
            exit_label: None,
        };

        pass.run();

        assert_eq!(count_gotos(pass.root_children()), 0);
        assert!(matches!(
            pass.root_children(),
            [
                Statement::Loop { children, .. },
                Statement::Code { block: 2 },
            ] if children.iter().any(|stmt| matches!(
                stmt,
                Statement::SetVariable { id: 1, .. }
            ))
        ));
    }

    fn count_gotos(tree: &[Statement]) -> usize {
        tree.iter()
            .map(|statement| match statement {
                Statement::Goto { .. } => 1,
                Statement::If { children, .. } | Statement::Loop { children, .. } => {
                    count_gotos(children)
                }
                _ => 0,
            })
            .sum()
    }
}
