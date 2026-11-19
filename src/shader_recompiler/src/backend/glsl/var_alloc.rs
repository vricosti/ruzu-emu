// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GLSL variable allocator.
//!
//! Maps to upstream `backend/glsl/var_alloc.h` and `var_alloc.cpp`.

use std::fmt;

use crate::ir;
use crate::ir::instruction::Inst;
use crate::ir::program::SyntaxNode;
use crate::ir::types::Type;
use crate::ir::value::{InstRef, Value};

/// GLSL variable type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlslVarType {
    U1,
    F16x2,
    U32,
    F32,
    U64,
    F64,
    U32x2,
    F32x2,
    U32x3,
    F32x3,
    U32x4,
    F32x4,
    PrecF32,
    PrecF64,
    Void,
}

/// Variable identifier packed into a u32.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Id {
    pub raw: u32,
}

impl Id {
    pub fn new() -> Self {
        Self { raw: 0 }
    }

    pub fn is_valid(&self) -> bool {
        self.raw & 1 != 0
    }

    pub fn set_valid(&mut self, v: bool) {
        if v {
            self.raw |= 1;
        } else {
            self.raw &= !1;
        }
    }

    pub fn var_type(&self) -> GlslVarType {
        let bits = (self.raw >> 1) & 0xF;
        match bits {
            0 => GlslVarType::U1,
            1 => GlslVarType::F16x2,
            2 => GlslVarType::U32,
            3 => GlslVarType::F32,
            4 => GlslVarType::U64,
            5 => GlslVarType::F64,
            6 => GlslVarType::U32x2,
            7 => GlslVarType::F32x2,
            8 => GlslVarType::U32x3,
            9 => GlslVarType::F32x3,
            10 => GlslVarType::U32x4,
            11 => GlslVarType::F32x4,
            12 => GlslVarType::PrecF32,
            13 => GlslVarType::PrecF64,
            _ => GlslVarType::Void,
        }
    }

    pub fn set_type(&mut self, t: GlslVarType) {
        let bits = match t {
            GlslVarType::U1 => 0u32,
            GlslVarType::F16x2 => 1,
            GlslVarType::U32 => 2,
            GlslVarType::F32 => 3,
            GlslVarType::U64 => 4,
            GlslVarType::F64 => 5,
            GlslVarType::U32x2 => 6,
            GlslVarType::F32x2 => 7,
            GlslVarType::U32x3 => 8,
            GlslVarType::F32x3 => 9,
            GlslVarType::U32x4 => 10,
            GlslVarType::F32x4 => 11,
            GlslVarType::PrecF32 => 12,
            GlslVarType::PrecF64 => 13,
            GlslVarType::Void => 14,
        };
        self.raw = (self.raw & !(0xF << 1)) | (bits << 1);
    }

    pub fn index(&self) -> u32 {
        self.raw >> 6
    }

    pub fn set_index(&mut self, idx: u32) {
        self.raw = (self.raw & 0x3F) | (idx << 6);
    }
}

impl Default for Id {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id(raw=0x{:08X})", self.raw)
    }
}

/// Return the GLSL type prefix for variable naming.
pub fn type_prefix(t: GlslVarType) -> &'static str {
    match t {
        GlslVarType::U1 => "b_",
        GlslVarType::F16x2 => "f16x2_",
        GlslVarType::U32 => "u_",
        GlslVarType::F32 => "f_",
        GlslVarType::U64 => "u64_",
        GlslVarType::F64 => "d_",
        GlslVarType::U32x2 => "u2_",
        GlslVarType::F32x2 => "f2_",
        GlslVarType::U32x3 => "u3_",
        GlslVarType::F32x3 => "f3_",
        GlslVarType::U32x4 => "u4_",
        GlslVarType::F32x4 => "f4_",
        GlslVarType::PrecF32 => "pf_",
        GlslVarType::PrecF64 => "pd_",
        GlslVarType::Void => "",
    }
}

/// Return the GLSL type string.
pub fn glsl_type_str(t: GlslVarType) -> &'static str {
    match t {
        GlslVarType::U1 => "bool",
        GlslVarType::F16x2 => "f16vec2",
        GlslVarType::U32 => "uint",
        GlslVarType::F32 | GlslVarType::PrecF32 => "float",
        GlslVarType::U64 => "uint64_t",
        GlslVarType::F64 | GlslVarType::PrecF64 => "double",
        GlslVarType::U32x2 => "uvec2",
        GlslVarType::F32x2 => "vec2",
        GlslVarType::U32x3 => "uvec3",
        GlslVarType::F32x3 => "vec3",
        GlslVarType::U32x4 => "uvec4",
        GlslVarType::F32x4 => "vec4",
        GlslVarType::Void => "",
    }
}

/// Per-type usage tracker.
pub struct UseTracker {
    pub uses_temp: bool,
    pub num_used: usize,
    pub var_use: Vec<bool>,
}

impl UseTracker {
    pub fn new() -> Self {
        Self {
            uses_temp: false,
            num_used: 0,
            var_use: Vec::new(),
        }
    }
}

impl Default for UseTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// GLSL variable allocator.
///
/// Matches upstream `VarAlloc` class.
pub struct VarAlloc {
    var_bool: UseTracker,
    var_f16x2: UseTracker,
    var_u32: UseTracker,
    var_u32x2: UseTracker,
    var_u32x3: UseTracker,
    var_u32x4: UseTracker,
    var_f32: UseTracker,
    var_f32x2: UseTracker,
    var_f32x3: UseTracker,
    var_f32x4: UseTracker,
    var_u64: UseTracker,
    var_f64: UseTracker,
    var_precf32: UseTracker,
    var_precf64: UseTracker,
}

impl VarAlloc {
    pub fn new() -> Self {
        Self {
            var_bool: UseTracker::new(),
            var_f16x2: UseTracker::new(),
            var_u32: UseTracker::new(),
            var_u32x2: UseTracker::new(),
            var_u32x3: UseTracker::new(),
            var_u32x4: UseTracker::new(),
            var_f32: UseTracker::new(),
            var_f32x2: UseTracker::new(),
            var_f32x3: UseTracker::new(),
            var_f32x4: UseTracker::new(),
            var_u64: UseTracker::new(),
            var_f64: UseTracker::new(),
            var_precf32: UseTracker::new(),
            var_precf64: UseTracker::new(),
        }
    }

    /// Used for explicit usages of variables, may revert to temporaries.
    ///
    /// Port of upstream `VarAlloc::Define(IR::Inst&, GlslVarType)`.
    pub fn define(&mut self, inst: &mut Inst, var_type: GlslVarType) -> String {
        if inst.has_uses() {
            inst.definition = self.alloc(var_type).raw;
            self.representation(Id {
                raw: inst.definition,
            })
        } else {
            let mut id = Id::new();
            id.set_type(var_type);
            self.get_use_tracker_mut(var_type).uses_temp = true;
            inst.definition = id.raw;
            format!("t{}", self.representation(id))
        }
    }

    pub fn define_ir_type(&mut self, inst: &mut Inst, ir_type: Type) -> String {
        self.define(inst, self.reg_type(ir_type))
    }

    /// Used to assign variables used by the IR. May return a blank string if
    /// the instruction's result is unused in the IR.
    ///
    /// Port of upstream `VarAlloc::AddDefine(IR::Inst&, GlslVarType)`.
    pub fn add_define(&mut self, inst: &mut Inst, var_type: GlslVarType) -> String {
        if inst.has_uses() {
            inst.definition = self.alloc(var_type).raw;
            self.representation(Id {
                raw: inst.definition,
            })
        } else {
            String::new()
        }
    }

    pub fn add_define_ir_type(&mut self, inst: &mut Inst, ir_type: Type) -> String {
        self.add_define(inst, self.reg_type(ir_type))
    }

    /// Port of upstream `VarAlloc::PhiDefine(IR::Inst&, IR::Type)`.
    pub fn phi_define(&mut self, inst: &mut Inst, ir_type: Type) -> String {
        self.add_define(inst, self.reg_type(ir_type))
    }

    pub fn definition_repr(&self, inst: &Inst) -> String {
        self.representation(Id {
            raw: inst.definition,
        })
    }

    /// Port of upstream `VarAlloc::Consume(const IR::Value&)`.
    pub fn consume(&mut self, program: &mut ir::Program, value: &Value) -> String {
        let resolved = resolve_identity_value(program, *value);
        // Upstream Value::IsImmediate also includes Type::Void; its MakeImm
        // returns an empty string for absent optional GLSL operands.
        if resolved.is_immediate() || resolved.is_void() {
            return make_imm(&resolved);
        }
        match resolved {
            Value::Inst(inst_ref) => self.consume_inst(program, inst_recursive(program, inst_ref)),
            _ => panic!("Cannot consume non-instruction GLSL value {:?}", value),
        }
    }

    /// Port of upstream `VarAlloc::ConsumeInst(IR::Inst&)`.
    pub fn consume_inst(&mut self, program: &mut ir::Program, inst_ref: InstRef) -> String {
        let inst = program.block_mut(inst_ref.block).inst_mut(inst_ref.inst);
        let id = Id {
            raw: inst.definition,
        };
        if !id.is_valid() {
            if let Some(imm) = undefined_immediate(inst.opcode) {
                if inst.use_count > 0 {
                    inst.use_count -= 1;
                }
                return imm.to_string();
            }
        }
        if inst.use_count > 0 {
            inst.use_count -= 1;
        }
        let repr = self.representation(id);
        if !inst.has_uses() && id.is_valid() {
            self.free(id);
        }
        repr
    }

    /// Get the string representation of a variable.
    pub fn representation(&self, id: Id) -> String {
        let prefix = type_prefix(id.var_type());
        format!("{}{}", prefix, id.index())
    }

    /// Get a named representation for a given index and type.
    pub fn representation_indexed(&self, index: u32, var_type: GlslVarType) -> String {
        let prefix = type_prefix(var_type);
        format!("{}{}", prefix, index)
    }

    /// Get the GLSL type string.
    pub fn get_glsl_type(&self, var_type: GlslVarType) -> &'static str {
        glsl_type_str(var_type)
    }

    /// Get the use tracker for a type.
    pub fn get_use_tracker(&self, var_type: GlslVarType) -> &UseTracker {
        match var_type {
            GlslVarType::U1 => &self.var_bool,
            GlslVarType::F16x2 => &self.var_f16x2,
            GlslVarType::U32 => &self.var_u32,
            GlslVarType::F32 => &self.var_f32,
            GlslVarType::U64 => &self.var_u64,
            GlslVarType::F64 => &self.var_f64,
            GlslVarType::U32x2 => &self.var_u32x2,
            GlslVarType::F32x2 => &self.var_f32x2,
            GlslVarType::U32x3 => &self.var_u32x3,
            GlslVarType::F32x3 => &self.var_f32x3,
            GlslVarType::U32x4 => &self.var_u32x4,
            GlslVarType::F32x4 => &self.var_f32x4,
            GlslVarType::PrecF32 => &self.var_precf32,
            GlslVarType::PrecF64 => &self.var_precf64,
            GlslVarType::Void => &self.var_bool, // fallback
        }
    }

    fn get_use_tracker_mut(&mut self, var_type: GlslVarType) -> &mut UseTracker {
        match var_type {
            GlslVarType::U1 => &mut self.var_bool,
            GlslVarType::F16x2 => &mut self.var_f16x2,
            GlslVarType::U32 => &mut self.var_u32,
            GlslVarType::F32 => &mut self.var_f32,
            GlslVarType::U64 => &mut self.var_u64,
            GlslVarType::F64 => &mut self.var_f64,
            GlslVarType::U32x2 => &mut self.var_u32x2,
            GlslVarType::F32x2 => &mut self.var_f32x2,
            GlslVarType::U32x3 => &mut self.var_u32x3,
            GlslVarType::F32x3 => &mut self.var_f32x3,
            GlslVarType::U32x4 => &mut self.var_u32x4,
            GlslVarType::F32x4 => &mut self.var_f32x4,
            GlslVarType::PrecF32 => &mut self.var_precf32,
            GlslVarType::PrecF64 => &mut self.var_precf64,
            GlslVarType::Void => &mut self.var_bool, // fallback
        }
    }

    fn alloc(&mut self, var_type: GlslVarType) -> Id {
        let tracker = self.get_use_tracker_mut(var_type);
        let num_vars = tracker.var_use.len();

        for var in 0..num_vars {
            if tracker.var_use[var] {
                continue;
            }
            tracker.num_used = tracker.num_used.max(var + 1);
            tracker.var_use[var] = true;

            let mut ret = Id::new();
            ret.set_valid(true);
            ret.set_type(var_type);
            ret.set_index(var as u32);
            return ret;
        }

        // Allocate a new variable
        tracker.var_use.push(true);
        let idx = tracker.num_used;
        tracker.num_used += 1;

        let mut ret = Id::new();
        ret.set_valid(true);
        ret.set_type(var_type);
        ret.set_index(idx as u32);
        ret
    }

    /// Free a variable.
    pub fn free(&mut self, id: Id) {
        if !id.is_valid() {
            panic!("Freeing invalid variable");
        }
        let tracker = self.get_use_tracker_mut(id.var_type());
        tracker.var_use[id.index() as usize] = false;
    }

    pub fn reg_type(&self, ir_type: Type) -> GlslVarType {
        match ir_type {
            Type::U1 => GlslVarType::U1,
            Type::U32 => GlslVarType::U32,
            Type::F32 => GlslVarType::F32,
            Type::U64 => GlslVarType::U64,
            Type::F64 => GlslVarType::F64,
            Type::U32x2 => GlslVarType::U32x2,
            Type::F32x2 => GlslVarType::F32x2,
            Type::U32x3 => GlslVarType::U32x3,
            Type::F32x3 => GlslVarType::F32x3,
            Type::U32x4 => GlslVarType::U32x4,
            Type::F32x4 => GlslVarType::F32x4,
            _ => panic!("Unsupported GLSL IR type {:?}", ir_type),
        }
    }
}

impl Default for VarAlloc {
    fn default() -> Self {
        Self::new()
    }
}

fn inst_recursive(program: &ir::Program, mut inst_ref: InstRef) -> InstRef {
    let original = inst_ref;
    loop {
        if program
            .block(inst_ref.block)
            .instructions
            .get(inst_ref.inst as usize)
            .is_none_or(|inst| inst.is_none())
        {
            panic!(
                "accessed erased instruction slot while chasing Identity chain: original={}:{} current={}:{}",
                original.block, original.inst, inst_ref.block, inst_ref.inst
            );
        }
        let inst = program.block(inst_ref.block).inst(inst_ref.inst);
        if inst.opcode != ir::opcodes::Opcode::Identity || inst.args.is_empty() {
            return inst_ref;
        }
        match inst.args[0] {
            Value::Inst(next) => inst_ref = next,
            _ => return inst_ref,
        }
    }
}

fn refs_to_value(program: &ir::Program, target: InstRef) -> Vec<String> {
    let mut refs = Vec::new();
    let target_value = Value::Inst(target);
    for (block_index, block) in program.blocks.iter().enumerate() {
        for (inst_index, inst) in block.indexed_iter() {
            for (arg_index, arg) in inst.args.iter().enumerate() {
                if *arg == target_value {
                    refs.push(format!(
                        "inst %{}:{} arg{} opcode={}",
                        block_index,
                        inst_index,
                        arg_index,
                        inst.opcode.name()
                    ));
                }
            }
            for (pred_index, value) in &inst.phi_args {
                if *value == target_value {
                    refs.push(format!(
                        "inst %{}:{} phi_pred{} opcode={}",
                        block_index,
                        inst_index,
                        pred_index,
                        inst.opcode.name()
                    ));
                }
            }
        }
    }
    for (node_index, node) in program.syntax_list.iter().enumerate() {
        let cond = match node {
            SyntaxNode::If { cond, .. }
            | SyntaxNode::Repeat { cond, .. }
            | SyntaxNode::Break { cond, .. } => Some(cond),
            _ => None,
        };
        if cond.is_some_and(|cond| *cond == target_value) {
            refs.push(format!("syntax node{} {:?}", node_index, node));
        }
    }
    refs
}

fn resolve_identity_value(program: &ir::Program, mut value: Value) -> Value {
    let original = value;
    while let Value::Inst(inst_ref) = value {
        if program
            .block(inst_ref.block)
            .instructions
            .get(inst_ref.inst as usize)
            .is_none_or(|inst| inst.is_none())
        {
            let refs = refs_to_value(program, inst_ref);
            panic!(
                "accessed erased instruction slot while resolving GLSL value: original={:?} current={}:{} refs={:?}",
                original, inst_ref.block, inst_ref.inst, refs
            );
        }
        let inst = program.block(inst_ref.block).inst(inst_ref.inst);
        if inst.opcode != ir::opcodes::Opcode::Identity || inst.args.is_empty() {
            return value;
        }
        value = inst.args[0];
    }
    value
}

fn undefined_immediate(opcode: ir::opcodes::Opcode) -> Option<&'static str> {
    match opcode {
        ir::opcodes::Opcode::UndefU1 => Some("false"),
        ir::opcodes::Opcode::UndefU8
        | ir::opcodes::Opcode::UndefU16
        | ir::opcodes::Opcode::UndefU32 => Some("0u"),
        ir::opcodes::Opcode::UndefU64 => Some("0u"),
        _ => None,
    }
}

fn make_imm(value: &Value) -> String {
    match value {
        Value::Void => String::new(),
        Value::ImmU1(v) => {
            if *v {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        Value::ImmU8(v) => format!("{}u", v),
        Value::ImmU16(v) => format!("{}u", v),
        Value::ImmU32(v) => format!("{}u", v),
        Value::ImmU64(v) => format!("{}ul", v),
        Value::ImmF16(v) => format!("uintBitsToFloat({}u)", v),
        Value::ImmF32(v) => format_float(*v),
        Value::ImmF64(v) => format!("double({})", v),
        _ => panic!("Value is not an immediate {:?}", value),
    }
}

fn format_float(value: f32) -> String {
    if value.is_nan() {
        return "uintBitsToFloat(0x7fc00000u)".to_string();
    }
    if value == f32::INFINITY {
        return "uintBitsToFloat(0x7f800000u)".to_string();
    }
    if value == f32::NEG_INFINITY {
        return "uintBitsToFloat(0xff800000u)".to_string();
    }
    let s = format!("{}", value);
    if s.contains(['.', 'e', 'E']) {
        format!("{}f", s)
    } else {
        format!("{}.f", s)
    }
}

#[cfg(test)]
mod tests {
    use crate::ir::basic_block::Block;
    use crate::ir::instruction::Inst;
    use crate::ir::opcodes::Opcode;
    use crate::ir::types::ShaderStage;
    use crate::ir::value::{InstRef, Value};

    use super::VarAlloc;

    #[test]
    fn consume_absent_operand_matches_upstream_empty_immediate() {
        let mut program = crate::ir::Program::new(ShaderStage::Compute);
        let mut alloc = VarAlloc::new();
        assert_eq!(alloc.consume(&mut program, &Value::Void), "");
        program.blocks.push(Block::new());
        let identity = program
            .block_mut(0)
            .append_inst(Inst::new(Opcode::Identity, vec![Value::Void]));
        assert_eq!(
            alloc.consume(&mut program, &Value::Inst(InstRef { block: 0, inst: identity })),
            ""
        );
    }

    #[test]
    fn consume_resolves_identity_to_immediate() {
        let mut program = crate::ir::Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        let identity = program
            .block_mut(0)
            .append_inst(Inst::new(Opcode::Identity, vec![Value::ImmU32(0)]));

        let mut alloc = VarAlloc::new();
        let value = alloc.consume(
            &mut program,
            &Value::Inst(InstRef {
                block: 0,
                inst: identity,
            }),
        );

        assert_eq!(value, "0u");
    }

    #[test]
    fn consume_unemitted_undef_uses_upstream_zero_literal() {
        let mut program = crate::ir::Program::new(ShaderStage::VertexB);
        program.blocks.push(Block::new());
        let undef = program
            .block_mut(0)
            .append_inst(Inst::new(Opcode::UndefU32, Vec::new()));
        program.block_mut(0).inst_mut(undef).use_count = 1;

        let mut alloc = VarAlloc::new();
        let value = alloc.consume(
            &mut program,
            &Value::Inst(InstRef {
                block: 0,
                inst: undef,
            }),
        );

        assert_eq!(value, "0u");
        assert_eq!(program.block(0).inst(undef).use_count, 0);
    }
}
