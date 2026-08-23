// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Maxwell shader frontend: decoder, CFG analysis, structured control flow,
//! and translator that emits IR.

pub mod control_flow;
pub mod decode;
pub mod indirect_branch_table_track;
pub mod instruction;
pub mod location;
pub mod maxwell_opcodes;
pub mod structured_control_flow;
pub mod translate;
pub mod translate_program;

#[cfg(test)]
mod tests {
    use super::control_flow;
    use super::maxwell_opcodes::{decode_opcode, MaxwellOpcode};

    // ── Opcode decoder tests ─────────────────────────────────────────

    /// Encode a simple NOP (opcode 0x50B on bits [63:52]).
    fn make_nop() -> u64 {
        0x50B0_0000_0000_0000
    }

    /// Encode IADD with a register source (`0101 1100 0001 0---`).
    fn make_iadd_reg() -> u64 {
        0x5C10_0000_0000_0000
    }

    #[test]
    fn test_decode_nop() {
        assert_eq!(decode_opcode(make_nop()), Some(MaxwellOpcode::NOP));
    }

    #[test]
    fn test_decode_iadd_register() {
        assert_eq!(
            decode_opcode(make_iadd_reg()),
            Some(MaxwellOpcode::IADD_reg)
        );
    }

    #[test]
    fn test_decode_all_zeros() {
        // All-zero instruction should either decode to something or return None
        let result = decode_opcode(0);
        // Just verify no panic
        let _ = result;
    }

    #[test]
    fn test_decode_all_ones() {
        let result = decode_opcode(0xFFFF_FFFF_FFFF_FFFF);
        let _ = result;
    }

    #[test]
    fn test_opcode_src_type() {
        use super::maxwell_opcodes::SrcType;
        assert_eq!(MaxwellOpcode::FADD_reg.src_type(), SrcType::Register);
        assert_eq!(MaxwellOpcode::FADD_cbuf.src_type(), SrcType::ConstantBuffer);
        assert_eq!(MaxwellOpcode::FADD_imm.src_type(), SrcType::Immediate);
    }

    #[test]
    fn test_opcode_base_name() {
        assert_eq!(MaxwellOpcode::FADD_reg.base_name(), "FADD");
        assert_eq!(MaxwellOpcode::IADD_reg.base_name(), "IADD");
        assert_eq!(MaxwellOpcode::MOV_reg.base_name(), "MOV");
        assert_eq!(MaxwellOpcode::TEX.base_name(), "TEX");
        assert_eq!(MaxwellOpcode::NOP.base_name(), "NOP");
    }

    // ── CFG builder tests ────────────────────────────────────────────

    #[test]
    fn test_cfg_empty() {
        let blocks = control_flow::build_cfg(&[]);
        assert!(blocks.is_empty());
    }

    #[test]
    fn test_cfg_single_instruction() {
        // A single EXIT instruction
        let code = vec![0xE300_0000_0000_0000u64]; // EXIT-like encoding
        let blocks = control_flow::build_cfg(&code);
        // Should produce at least one block
        assert!(!blocks.is_empty());
    }

    #[test]
    fn test_cfg_linear_sequence() {
        // Three NOP-like instructions (no branches)
        let code = vec![
            0x50B0_0000_0000_0000u64, // NOP
            0x50B0_0000_0000_0000u64, // NOP
            0x50B0_0000_0000_0000u64, // NOP
        ];
        let blocks = control_flow::build_cfg(&code);
        // Should produce one block containing all three
        assert!(!blocks.is_empty());
    }
}
