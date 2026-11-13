// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of Fritchii frontend/maxwell/translate/impl/tensor_mul_add.cpp.
use super::{bit, field, TranslatorVisitor};
use crate::ir::Value;
use std::sync::OnceLock;

pub fn tensor_mul_add(v: &mut TranslatorVisitor<'_>, insn: u64) {
    let ar = field(insn, 8, 8);
    let br = field(insn, 20, 8);
    let cr = field(insn, 39, 8);
    let dr = field(insn, 0, 8);
    static DISABLED: OnceLock<bool> = OnceLock::new();
    if *DISABLED.get_or_init(|| std::env::var_os("EDEN_NX2_NO_TENSOR").is_some()) || !bit(insn, 48) {
        std::panic::panic_any(crate::exception::NotImplementedException::new("Unsupported matrix multiply encoding"));
    }
    let short = bit(insn, 49);
    let zero = Value::ImmU32(0);
    let a0 = v.x(ar);
    let a1 = v.x(ar + 1);
    let a2 = if short { zero } else { v.x(ar + 2) };
    let a3 = if short { zero } else { v.x(ar + 3) };
    let b0 = v.x(br);
    let b1 = if short { zero } else { v.x(br + 1) };
    let c0 = if cr != 255 { v.x(cr) } else { zero };
    let c1 = if cr != 255 { v.x(cr + 1) } else { zero };
    let a = v.ir.composite_construct_u32x4(a0,a1,a2,a3);
    let b = v.ir.composite_construct_u32x2(b0,b1);
    let c = v.ir.composite_construct_u32x2(c0,c1);
    let result = v.ir.tensor_mul_add(a,b,c,if short {8} else {16})
        .unwrap_or_else(|error| std::panic::panic_any(error));
    let low = v.ir.composite_extract_u32x2_idx(result, 0);
    v.set_x(dr, low);
    let high = v.ir.composite_extract_u32x2_idx(result, 1);
    v.set_x(dr+1, high);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Program, Opcode, types::ShaderStage};
    use crate::frontend::ampere::lower_to_maxwell::{Instruction, LoweringOptions, lower_to_maxwell_with_options};
    #[test]
    fn compact_matrix_lowering_decodes_both_depths_and_snapshots_operands() {
        for depth in [8,16] {
            for c in [8,255] {
                let native = Instruction { words: [0x0402723c, 6, c | if depth == 16 {0x800} else {0}, 0] };
                let exit = Instruction { words: [0x794d, 0, 0x03800000, 0] };
                let lowered = lower_to_maxwell_with_options(&[native, exit], LoweringOptions {tensor:true});
                assert!(lowered.is_valid(), "{}", lowered.error);
                assert_eq!(lowered.source_instruction_offsets[1], 1);
                assert_eq!(lowered.instructions[0], 0x5d30000000000000 | (7 << 16) | 2 | (4 << 8)
                    | (6 << 20) | (u64::from(c) << 39) | (1 << 48) | (u64::from(depth==8) << 49));
                let mut p = Program::new(ShaderStage::Compute);
                let block = p.add_block();
                let mut v = TranslatorVisitor::new(&mut p, block);
                v.translate_instruction(lowered.instructions[0]);
                let insts: Vec<_> = p.block(block).iter().collect();
                let matrix = insts.iter().find(|i| i.opcode == Opcode::TensorMulAdd).unwrap();
                assert_eq!(matrix.flags, depth);
                let reads: Vec<_> = insts.iter().filter(|i| i.opcode == Opcode::GetRegister).map(|i| i.args[0]).collect();
                let mut expected: Vec<_> = if depth == 8 {vec![4,5,6]} else {vec![4,5,6,7,6,7]};
                if c != 255 { expected.extend([c,c+1]); }
                assert_eq!(reads, expected.into_iter().map(|r| Value::Reg(crate::ir::Reg(r as u64))).collect::<Vec<_>>());
                let writes: Vec<_> = insts.iter().filter(|i| i.opcode == Opcode::SetRegister).map(|i| i.args[0]).collect();
                assert_eq!(writes, vec![Value::Reg(crate::ir::Reg(2)), Value::Reg(crate::ir::Reg(3))]);
            }
        }
    }
}
