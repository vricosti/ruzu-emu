// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! IR Program — the top-level container for a translated shader.
//!
//! Matches zuyu's `Program` from `program.h`. Contains basic blocks,
//! a structured control flow AST, shader metadata, and stage info.

use super::basic_block::Block;
use super::types::{OutputTopology, ShaderStage};
use super::value::Value;

use crate::shader_info;

/// Abstract syntax tree node for structured control flow.
///
/// Matches zuyu's `AbstractSyntaxNode` from `abstract_syntax_list.h`.
/// The structured CF pass converts flat CFG to this nested representation,
/// which SPIR-V emission can directly consume.
#[derive(Debug, Clone)]
pub enum SyntaxNode {
    /// A basic block to execute.
    Block(u32),
    /// If-then: condition, body block, merge block.
    If { cond: Value, body: u32, merge: u32 },
    /// End of an if-then region.
    EndIf { merge: u32 },
    /// Loop: body block, continue block, merge (break target) block.
    Loop {
        body: u32,
        continue_block: u32,
        merge: u32,
    },
    /// Repeat (back-edge of a loop): condition, loop header, merge block.
    Repeat {
        cond: Value,
        loop_header: u32,
        merge: u32,
    },
    /// Break out of a loop: condition, merge (loop merge) block, skip block.
    Break { cond: Value, merge: u32, skip: u32 },
    /// Return from the shader.
    Return,
    /// Unreachable code marker.
    Unreachable,
}

/// Shader metadata collected during translation.
///
/// Now a type alias for the upstream-faithful `shader_info::Info`
/// struct. The previous simplified `ShaderInfo` with separate
/// descriptor types and boolean fields was deleted as part of the
/// cross-crate type-unification pass. The upstream `Info` carries all
/// the same data via `VaryingState` bitmasks and the full descriptor
/// types (`ConstantBufferDescriptor`, `TextureDescriptor`, etc.).
///
/// Convenience methods (`register_cbuf`, `register_texture`) that the
/// IR emitter depends on are provided as a trait impl below.
pub type ShaderInfo = shader_info::Info;

/// Re-export the upstream descriptor types under the names the rest of
/// the recompiler already expects. Code that used the old simplified
/// `CbufDescriptor` / `TexDescriptor` / `StorageDescriptor` should
/// migrate to these names.
pub type CbufDescriptor = shader_info::ConstantBufferDescriptor;
pub type TexDescriptor = shader_info::TextureDescriptor;
pub type StorageDescriptor = shader_info::StorageBufferDescriptor;

/// Convenience helpers on `ShaderInfo` (= `shader_info::Info`) used
/// by the IR emitter to register resource accesses incrementally.
pub trait ShaderInfoExt {
    fn register_cbuf(&mut self, index: u32);
    fn register_texture(
        &mut self,
        index: u32,
        texture_type: shader_info::TextureType,
        is_depth: bool,
    );
}

impl ShaderInfoExt for ShaderInfo {
    fn register_cbuf(&mut self, index: u32) {
        self.constant_buffer_mask |= 1u32 << index;
        if !self
            .constant_buffer_descriptors
            .iter()
            .any(|d| d.index == index)
        {
            self.constant_buffer_descriptors
                .push(shader_info::ConstantBufferDescriptor { index, count: 1 });
        }
    }

    fn register_texture(
        &mut self,
        index: u32,
        texture_type: shader_info::TextureType,
        is_depth: bool,
    ) {
        if !self
            .texture_descriptors
            .iter()
            .any(|d| d.cbuf_index == index)
        {
            self.texture_descriptors
                .push(shader_info::TextureDescriptor {
                    texture_type,
                    is_depth,
                    is_multisample: false,
                    is_integer: false,
                    has_secondary: false,
                    cbuf_index: index,
                    cbuf_offset: 0,
                    shift_left: 0,
                    secondary_cbuf_index: 0,
                    secondary_cbuf_offset: 0,
                    secondary_shift_left: 0,
                    count: 1,
                    size_shift: 0,
                });
        }
    }
}

/// A complete translated shader program.
#[derive(Debug, Clone)]
pub struct Program {
    /// Structured control flow AST.
    pub syntax_list: Vec<SyntaxNode>,
    /// All basic blocks.
    pub blocks: Vec<Block>,
    /// Post-order traversal of blocks (for optimization passes).
    pub post_order_blocks: Vec<u32>,
    /// Shader metadata collected during translation.
    pub info: ShaderInfo,
    /// Shader stage.
    pub stage: ShaderStage,
    /// Local memory size in bytes.
    /// Upstream: `IR::Program::local_memory_size`.
    pub local_memory_size: u32,
    /// Shared memory size in bytes (compute shaders).
    /// Upstream: `IR::Program::shared_memory_size`.
    pub shared_memory_size: u32,
    /// Workgroup size (compute shaders).
    pub workgroup_size: [u32; 3],
    /// Output topology (geometry shaders).
    pub output_topology: OutputTopology,
    /// Number of output vertices (geometry shaders).
    pub output_vertices: u32,
    /// Number of invocations (geometry shaders).
    pub invocations: u32,
    /// Whether this is a passthrough geometry shader.
    pub is_geometry_passthrough: bool,
    /// Upstream `IR::Program::mesh` (Eglantine 900ab5a8 program.h).
    pub mesh: shader_info::MeshInfo,
}

impl Program {
    /// Create a new empty program for the given stage.
    pub fn new(stage: ShaderStage) -> Self {
        Self {
            syntax_list: Vec::new(),
            blocks: Vec::new(),
            post_order_blocks: Vec::new(),
            info: ShaderInfo::default(),
            stage,
            local_memory_size: 0,
            shared_memory_size: 0,
            workgroup_size: [1, 1, 1],
            output_topology: OutputTopology::TriangleStrip,
            output_vertices: 0,
            invocations: 1,
            is_geometry_passthrough: false,
            mesh: shader_info::MeshInfo::default(),
        }
    }

    /// Add a new block and return its index.
    pub fn add_block(&mut self) -> u32 {
        let idx = self.blocks.len() as u32;
        self.blocks.push(Block::new());
        idx
    }

    /// Get a block by index.
    pub fn block(&self, idx: u32) -> &Block {
        &self.blocks[idx as usize]
    }

    /// Get a mutable block by index.
    pub fn block_mut(&mut self, idx: u32) -> &mut Block {
        &mut self.blocks[idx as usize]
    }

    /// Invalidate and erase an instruction while preserving stable slot IDs.
    ///
    /// Upstream instruction pointers unlink pseudo-operations from their
    /// parent in `Inst::Invalidate` -> `ClearArgs` -> `UndoUse`. Rust stores
    /// indexed references instead, so the owning `Program` performs that
    /// cross-block unlink before leaving a tombstone in the block.
    pub fn erase_inst(&mut self, inst_ref: super::value::InstRef) {
        let (opcode, arg_count, phi_count) = {
            let inst = self.block(inst_ref.block).inst(inst_ref.inst);
            (inst.opcode, inst.args.len(), inst.phi_args.len())
        };
        // Copy one indexed operand at a time, like ClearArgs -> UndoUse.
        // Building a temporary Vec here allocates for every erased instruction.
        for index in 0..arg_count + phi_count {
            let value = {
                let inst = self.block(inst_ref.block).inst(inst_ref.inst);
                if index < arg_count { inst.args[index] }
                else { inst.phi_args[index - arg_count].1 }
            };
            let super::value::Value::Inst(parent) = value else { continue; };
            let parent_inst = self.block_mut(parent.block).inst_mut(parent.inst);
            assert!(parent_inst.use_count > 0, "instruction use count underflow");
            parent_inst.use_count -= 1;
            if opcode.is_pseudo_instruction() {
                parent_inst.remove_associated_pseudo(opcode, inst_ref);
            }
        }
        self.block_mut(inst_ref.block).erase_inst(inst_ref.inst);
    }

    /// Rebuild instruction use counts from the indexed IR graph.
    ///
    /// Upstream updates its intrusive use-def chain in every `Inst::Use`,
    /// `Inst::UndoUse`, and `Inst::SetArg` call. The Rust IR still constructs
    /// and rewrites indexed `InstRef` values directly, so passes which perform
    /// upstream-equivalent invalidation must first materialize that same
    /// invariant from the current graph.
    pub(crate) fn recompute_use_counts(&mut self) {
        for block in &mut self.blocks {
            for inst in block.iter_mut() {
                inst.use_count = 0;
            }
        }

        let mut use_counts = self
            .blocks
            .iter()
            .map(|block| vec![0u32; block.instructions.len()])
            .collect::<Vec<_>>();

        let count_value = |counts: &mut [Vec<u32>], value: &super::value::Value| {
            if let super::value::Value::Inst(inst_ref) = value {
                if let Some(count) = counts
                    .get_mut(inst_ref.block as usize)
                    .and_then(|block| block.get_mut(inst_ref.inst as usize))
                {
                    *count += 1;
                }
            }
        };
        for block in &self.blocks {
            for inst in block.iter() {
                for arg in &inst.args {
                    count_value(&mut use_counts, arg);
                }
                for (_, arg) in &inst.phi_args {
                    count_value(&mut use_counts, arg);
                }
            }
        }
        for node in &self.syntax_list {
            match node {
                SyntaxNode::If { cond, .. }
                | SyntaxNode::Repeat { cond, .. }
                | SyntaxNode::Break { cond, .. } => count_value(&mut use_counts, cond),
                _ => {}
            }
        }

        for (block_idx, block) in self.blocks.iter_mut().enumerate() {
            for (inst_idx, inst) in block.indexed_iter_mut() {
                inst.use_count = use_counts[block_idx][inst_idx as usize];
            }
        }
    }

    /// Number of blocks.
    pub fn num_blocks(&self) -> usize {
        self.blocks.len()
    }
}

#[cfg(test)]
mod tests {
    use super::Program;
    use crate::ir::{Inst, InstRef, Opcode, ShaderStage, Value};

    #[test]
    fn erase_phi_undoes_repeated_cross_block_uses_and_keeps_live_consumers() {
        let mut program = Program::new(ShaderStage::Fragment);
        let source = program.add_block();
        let destination = program.add_block();
        let producer = InstRef { block: source, inst: program.block_mut(source)
            .append_inst(Inst::new(Opcode::IAdd32, vec![])) };
        let live = program.block_mut(source).append_inst(Inst::new(
            Opcode::Identity, vec![Value::Inst(producer)]));
        let mut phi = Inst::new(Opcode::Phi, vec![]);
        for predecessor in 0..256 {
            phi.add_phi_operand(predecessor, Value::Inst(producer));
        }
        phi.add_phi_operand(256, Value::ImmU32(42));
        let dead = InstRef { block: destination,
            inst: program.block_mut(destination).append_inst(phi) };
        program.recompute_use_counts();
        assert_eq!(program.block(source).inst(producer.inst).use_count, 257);
        program.erase_inst(dead);
        assert_eq!(program.block(source).inst(producer.inst).use_count, 1);
        assert_eq!(program.block(source).inst(live).args[0], Value::Inst(producer));
        assert!(program.block(destination).instructions[dead.inst as usize].is_none());
    }

    #[test]
    fn erase_inst_undoes_each_argument_use() {
        let mut program = Program::new(ShaderStage::Fragment);
        let block = program.add_block();
        let producer = program
            .block_mut(block)
            .append_inst(Inst::new(Opcode::IAdd32, vec![]));
        let producer_ref = InstRef {
            block,
            inst: producer,
        };
        program.block_mut(block).inst_mut(producer).use_count = 2;
        let consumer = program.block_mut(block).append_inst(Inst::new(
            Opcode::IAdd32,
            vec![Value::Inst(producer_ref), Value::Inst(producer_ref)],
        ));

        program.erase_inst(InstRef {
            block,
            inst: consumer,
        });

        assert_eq!(program.block(block).inst(producer).use_count, 0);
        assert!(program.block(block).instructions[consumer as usize].is_none());
    }
}
