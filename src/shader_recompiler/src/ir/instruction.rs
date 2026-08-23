// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! IR Instruction — a single SSA instruction in the IR.
//!
//! Matches zuyu's `Inst` class from `value.h`. Each instruction has an opcode,
//! up to 5 arguments, a use count for dead code elimination, opcode-specific
//! flags, and optional associated pseudo-operations (zero/sign/carry/overflow).

use super::opcodes::Opcode;
use super::value::{InstRef, Value};

/// Associated pseudo-instructions that extract flags from the result of an
/// instruction. Matches zuyu's `AssociatedInsts`.
#[derive(Debug, Clone, Default)]
pub struct AssociatedInsts {
    /// GetZeroFromOp / GetInBoundsFromOp / GetSparseFromOp (first union member).
    pub zero_inst: Option<InstRef>,
    /// GetSignFromOp.
    pub sign_inst: Option<InstRef>,
    /// GetCarryFromOp.
    pub carry_inst: Option<InstRef>,
    /// GetOverflowFromOp.
    pub overflow_inst: Option<InstRef>,
}

/// A single IR instruction (SSA form).
#[derive(Debug, Clone)]
pub struct Inst {
    /// The operation this instruction performs.
    pub opcode: Opcode,
    /// Arguments to the instruction. Most instructions have 0-4 args.
    /// Phi nodes may have variable args via `phi_args`.
    pub args: Vec<Value>,
    /// Reference count — how many other instructions use this one's result.
    pub use_count: u32,
    /// Opcode-specific flags (FpControl, TextureInstInfo, etc.).
    pub flags: u32,
    /// Associated pseudo-operations for flag extraction.
    pub associated: Option<Box<AssociatedInsts>>,
    /// Phi node block-value pairs: `(block_index, value)`.
    /// Only used when opcode == Phi.
    pub phi_args: Vec<(u32, Value)>,
    /// Backend-specific definition storage (e.g., SPIR-V Word ID).
    pub definition: u32,
}

impl Inst {
    /// Create a new instruction with the given opcode and arguments.
    pub fn new(opcode: Opcode, args: Vec<Value>) -> Self {
        Self {
            opcode,
            args,
            use_count: 0,
            flags: 0,
            associated: None,
            phi_args: Vec::new(),
            definition: 0,
        }
    }

    /// Create a new instruction with flags.
    pub fn with_flags(opcode: Opcode, args: Vec<Value>, flags: u32) -> Self {
        Self {
            opcode,
            args,
            use_count: 0,
            flags,
            associated: None,
            phi_args: Vec::new(),
            definition: 0,
        }
    }

    /// Create a Phi instruction (no initial args).
    pub fn phi() -> Self {
        Self {
            opcode: Opcode::Phi,
            args: Vec::new(),
            use_count: 0,
            flags: 0,
            associated: None,
            phi_args: Vec::new(),
            definition: 0,
        }
    }

    /// Get the return type of this instruction.
    pub fn return_type(&self) -> super::types::Type {
        if self.opcode == Opcode::Phi {
            match super::types::Type::from_raw(self.flags) {
                Some(super::types::Type::Void) | None => super::types::Type::Opaque,
                Some(ty) => ty,
            }
        } else {
            self.opcode.return_type()
        }
    }

    /// Number of arguments.
    pub fn num_args(&self) -> usize {
        if self.opcode == Opcode::Phi {
            self.phi_args.len()
        } else {
            self.args.len()
        }
    }

    /// Get argument at index.
    pub fn arg(&self, index: usize) -> &Value {
        if self.opcode == Opcode::Phi {
            &self.phi_args[index].1
        } else {
            &self.args[index]
        }
    }

    /// Set argument at index.
    pub fn set_arg(&mut self, index: usize, value: Value) {
        if self.opcode == Opcode::Phi {
            self.phi_args[index].1 = value;
        } else {
            self.args[index] = value;
        }
    }

    /// Whether this instruction has any uses.
    pub fn has_uses(&self) -> bool {
        self.use_count > 0
    }

    /// Whether this instruction may have side effects.
    pub fn may_have_side_effects(&self) -> bool {
        self.opcode.may_have_side_effects()
    }

    /// Whether this is a pseudo-instruction (flag extraction).
    pub fn is_pseudo_instruction(&self) -> bool {
        self.opcode.is_pseudo_instruction()
    }

    /// Add a phi operand (block, value) pair.
    pub fn add_phi_operand(&mut self, block: u32, value: Value) {
        debug_assert_eq!(self.opcode, Opcode::Phi);
        self.phi_args.push((block, value));
    }

    /// Order Phi operands from the farthest predecessor to the nearest.
    ///
    /// Upstream: `Inst::OrderPhiArgs()` in `frontend/ir/microinstruction.cpp`.
    pub fn order_phi_args(&mut self, block_orders: &[u32]) {
        assert_eq!(self.opcode, Opcode::Phi);
        self.phi_args
            .sort_by_key(|(block, _)| block_orders[*block as usize]);
    }

    /// Get or create the associated pseudo-instructions structure.
    pub fn get_or_create_associated(&mut self) -> &mut AssociatedInsts {
        if self.associated.is_none() {
            self.associated = Some(Box::default());
        }
        self.associated.as_mut().unwrap()
    }

    /// Get the associated pseudo-instruction for a specific pseudo-opcode.
    pub fn get_associated_pseudo(&self, pseudo_op: Opcode) -> Option<InstRef> {
        let assoc = self.associated.as_ref()?;
        match pseudo_op {
            Opcode::GetZeroFromOp | Opcode::GetSparseFromOp | Opcode::GetInBoundsFromOp => {
                assoc.zero_inst
            }
            Opcode::GetSignFromOp => assoc.sign_inst,
            Opcode::GetCarryFromOp => assoc.carry_inst,
            Opcode::GetOverflowFromOp => assoc.overflow_inst,
            _ => None,
        }
    }

    /// Set the associated pseudo-instruction for a specific pseudo-opcode.
    pub fn set_associated_pseudo(&mut self, pseudo_op: Opcode, inst_ref: InstRef) {
        let assoc = self.get_or_create_associated();
        let slot = match pseudo_op {
            Opcode::GetZeroFromOp | Opcode::GetSparseFromOp | Opcode::GetInBoundsFromOp => {
                &mut assoc.zero_inst
            }
            Opcode::GetSignFromOp => &mut assoc.sign_inst,
            Opcode::GetCarryFromOp => &mut assoc.carry_inst,
            Opcode::GetOverflowFromOp => &mut assoc.overflow_inst,
            _ => return,
        };
        assert!(
            slot.is_none(),
            "only one of each associated pseudo-instruction is allowed"
        );
        *slot = Some(inst_ref);
    }

    /// Detach an associated pseudo-instruction when that pseudo-instruction is
    /// invalidated. This is the indexed-reference equivalent of upstream
    /// `Inst::UndoUse` calling `RemovePseudoInstruction`.
    pub fn remove_associated_pseudo(&mut self, pseudo_op: Opcode, inst_ref: InstRef) {
        let Some(assoc) = self.associated.as_mut() else {
            panic!("removing a pseudo-instruction from a parent without associations");
        };
        let slot = match pseudo_op {
            Opcode::GetZeroFromOp | Opcode::GetSparseFromOp | Opcode::GetInBoundsFromOp => {
                &mut assoc.zero_inst
            }
            Opcode::GetSignFromOp => &mut assoc.sign_inst,
            Opcode::GetCarryFromOp => &mut assoc.carry_inst,
            Opcode::GetOverflowFromOp => &mut assoc.overflow_inst,
            _ => return,
        };
        assert_eq!(
            *slot,
            Some(inst_ref),
            "removing an invalid associated pseudo-instruction"
        );
        *slot = None;
    }

    /// Upstream `Inst::ReplaceUsesWith`.
    ///
    /// The name does not mean that Eden walks every user. Instruction identity
    /// is pointer-stable there, so the definition itself becomes
    /// `Identity(replacement)` and existing users keep referring to it. Rust's
    /// stable `InstRef` slots preserve the same constant-time operation.
    pub fn replace_uses_with(&mut self, replacement: Value) {
        self.opcode = Opcode::Identity;
        self.args.clear();
        self.phi_args.clear();
        self.args.push(replacement);
    }

    /// Upstream `Inst::Invalidate` without cross-instruction use bookkeeping.
    /// The owning `Program` handles indexed pseudo links and recomputes use
    /// counts before passes which consume them.
    pub fn invalidate(&mut self) {
        self.opcode = Opcode::Void;
        self.args.clear();
        self.phi_args.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phi_argument_access_mutation_and_invalidation_match_upstream() {
        let mut inst = Inst::phi();
        inst.flags = super::super::types::Type::U32 as u32;
        inst.add_phi_operand(3, Value::ImmU32(10));

        assert_eq!(inst.return_type(), super::super::types::Type::U32);
        assert_eq!(inst.num_args(), 1);
        assert_eq!(*inst.arg(0), Value::ImmU32(10));
        inst.set_arg(0, Value::ImmU32(20));
        assert_eq!(inst.phi_args, vec![(3, Value::ImmU32(20))]);

        inst.invalidate();
        assert!(inst.phi_args.is_empty());
        assert_eq!(inst.opcode, Opcode::Void);
    }

    #[test]
    fn phi_without_a_concrete_type_remains_opaque() {
        assert_eq!(Inst::phi().return_type(), super::super::types::Type::Opaque);
    }

    #[test]
    fn replace_uses_with_keeps_definition_identity_and_use_count() {
        let mut inst = Inst::phi();
        inst.flags = 0x44;
        inst.use_count = 7;
        inst.add_phi_operand(3, Value::ImmU32(10));

        inst.replace_uses_with(Value::ImmU32(20));

        assert_eq!(inst.opcode, Opcode::Identity);
        assert_eq!(inst.args, vec![Value::ImmU32(20)]);
        assert!(inst.phi_args.is_empty());
        assert_eq!(inst.use_count, 7);
        assert_eq!(inst.flags, 0x44);
    }
}

impl std::fmt::Display for Inst {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.opcode)?;
        if !self.args.is_empty() {
            write!(f, " ")?;
            for (i, arg) in self.args.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}", arg)?;
            }
        }
        if self.opcode == Opcode::Phi && !self.phi_args.is_empty() {
            write!(f, " ")?;
            for (i, (block, val)) in self.phi_args.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "[block{}: {}]", block, val)?;
            }
        }
        Ok(())
    }
}
