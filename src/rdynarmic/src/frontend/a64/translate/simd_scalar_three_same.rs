//! Port of upstream
//! `dynarmic/frontend/A64/translate/impl/simd_scalar_three_same.cpp`
//! (subset of integer/FP scalar comparisons + add/sub/shifts/reciprocal steps) and the
//! "scalar (zero)" comparison forms which upstream lumps into the same
//! family.

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;
use crate::ir::value::Value;

#[derive(Copy, Clone)]
enum CmpKind {
    Eq,
    Ge,
    Gt,
    Hi,
    Hs,
    Le,
    Lt,
}

#[derive(Copy, Clone)]
enum CmpVariant {
    Register(Vec),
    Zero,
}

impl<'a> TranslatorVisitor<'a> {
    /// All scalar integer compares operate on 64-bit operands; size==0b11 only.
    fn scalar_compare(
        &mut self,
        size: u32,
        vn: Vec,
        vd: Vec,
        variant: CmpVariant,
        kind: CmpKind,
    ) -> bool {
        if size != 0b11 {
            return self.reserved_value();
        }
        let esize = 64usize;
        let datasize = 64usize;
        let operand1 = self.v_scalar_read(datasize, vn);
        let operand2 = match variant {
            CmpVariant::Register(vm) => self.v_scalar_read(datasize, vm),
            CmpVariant::Zero => self.ir.ir().zero_vector(),
        };
        let result = match kind {
            CmpKind::Eq => self.ir.ir().vector_equal(esize, operand1, operand2),
            CmpKind::Ge => self
                .ir
                .ir()
                .vector_greater_equal_signed(esize, operand1, operand2),
            CmpKind::Gt => self
                .ir
                .ir()
                .vector_greater_signed(esize, operand1, operand2),
            CmpKind::Hi => self
                .ir
                .ir()
                .vector_greater_unsigned(esize, operand1, operand2),
            CmpKind::Hs => self
                .ir
                .ir()
                .vector_greater_equal_unsigned(esize, operand1, operand2),
            CmpKind::Le => self
                .ir
                .ir()
                .vector_less_equal_signed(esize, operand1, operand2),
            CmpKind::Lt => self
                .ir
                .ir()
                .vector_less_signed(esize, operand1, operand2),
        };
        let elem = self.ir.ir().vector_get_element(esize, result, 0);
        self.v_scalar_write(esize, vd, elem);
        true
    }

    fn scalar_three_same_args(&mut self, inst: &DecodedInst) -> (u32, Vec, Vec, Vec) {
        let size = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        (size, vm, vn, vd)
    }

    fn scalar_two_zero_args(&mut self, inst: &DecodedInst) -> (u32, Vec, Vec) {
        let size = inst.bits(23, 22);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        (size, vn, vd)
    }

    /// ADD (vector, scalar). `01011110zz1mmmmm100001nnnnnddddd`.
    pub fn add_1(&mut self, inst: &DecodedInst) -> bool {
        let (size, vm, vn, vd) = self.scalar_three_same_args(inst);
        if size != 0b11 {
            return self.reserved_value();
        }
        let esize = 64usize;
        let op1 = self.v_scalar_read(esize, vn);
        let op2 = self.v_scalar_read(esize, vm);
        let elem1 = self.ir.ir().vector_get_element(esize, op1, 0);
        let elem2 = self.ir.ir().vector_get_element(esize, op2, 0);
        let zero = self.ir.ir().imm1(false);
        let result = self.ir.ir().add_64(elem1, elem2, zero);
        self.v_scalar_write(esize, vd, result);
        true
    }

    /// SUB (vector, scalar). `01111110zz1mmmmm100001nnnnnddddd`.
    pub fn sub_1(&mut self, inst: &DecodedInst) -> bool {
        let (size, vm, vn, vd) = self.scalar_three_same_args(inst);
        if size != 0b11 {
            return self.reserved_value();
        }
        let esize = 64usize;
        let op1 = self.v_scalar_read(esize, vn);
        let op2 = self.v_scalar_read(esize, vm);
        let elem1 = self.ir.ir().vector_get_element(esize, op1, 0);
        let elem2 = self.ir.ir().vector_get_element(esize, op2, 0);
        let one = self.ir.ir().imm1(true);
        let result = self.ir.ir().sub_64(elem1, elem2, one);
        self.v_scalar_write(esize, vd, result);
        true
    }

    pub fn cmeq_reg_1(&mut self, inst: &DecodedInst) -> bool {
        let (size, vm, vn, vd) = self.scalar_three_same_args(inst);
        self.scalar_compare(size, vn, vd, CmpVariant::Register(vm), CmpKind::Eq)
    }
    pub fn cmge_reg_1(&mut self, inst: &DecodedInst) -> bool {
        let (size, vm, vn, vd) = self.scalar_three_same_args(inst);
        self.scalar_compare(size, vn, vd, CmpVariant::Register(vm), CmpKind::Ge)
    }
    pub fn cmgt_reg_1(&mut self, inst: &DecodedInst) -> bool {
        let (size, vm, vn, vd) = self.scalar_three_same_args(inst);
        self.scalar_compare(size, vn, vd, CmpVariant::Register(vm), CmpKind::Gt)
    }
    pub fn cmhi_1(&mut self, inst: &DecodedInst) -> bool {
        let (size, vm, vn, vd) = self.scalar_three_same_args(inst);
        self.scalar_compare(size, vn, vd, CmpVariant::Register(vm), CmpKind::Hi)
    }
    pub fn cmhs_1(&mut self, inst: &DecodedInst) -> bool {
        let (size, vm, vn, vd) = self.scalar_three_same_args(inst);
        self.scalar_compare(size, vn, vd, CmpVariant::Register(vm), CmpKind::Hs)
    }

    /// CMTST (scalar). `01011110zz1mmmmm100011nnnnnddddd`.
    pub fn cmtst_1(&mut self, inst: &DecodedInst) -> bool {
        let (size, vm, vn, vd) = self.scalar_three_same_args(inst);
        if size != 0b11 {
            return self.reserved_value();
        }
        let op1 = self.v_scalar_read(64, vn);
        let op2 = self.v_scalar_read(64, vm);
        let anded = self.ir.ir().vector_and(op1, op2);
        let zero = self.ir.ir().zero_vector();
        let eq = self.ir.ir().vector_equal(64, anded, zero);
        let result = self.ir.ir().vector_not(eq);
        self.v_scalar_write(64, vd, result);
        true
    }

    /// SSHL (scalar). `01011110zz1mmmmm010001nnnnnddddd`.
    pub fn sshl_1(&mut self, inst: &DecodedInst) -> bool {
        let (size, vm, vn, vd) = self.scalar_three_same_args(inst);
        if size != 0b11 {
            return self.reserved_value();
        }
        let op1 = self.v_scalar_read(64, vn);
        let op2 = self.v_scalar_read(64, vm);
        let result = self.ir.ir().vector_arithmetic_v_shift(64, op1, op2);
        self.v_scalar_write(64, vd, result);
        true
    }

    /// USHL (scalar). `01111110zz1mmmmm010001nnnnnddddd`.
    pub fn ushl_1(&mut self, inst: &DecodedInst) -> bool {
        let (size, vm, vn, vd) = self.scalar_three_same_args(inst);
        if size != 0b11 {
            return self.reserved_value();
        }
        let op1 = self.v_scalar_read(64, vn);
        let op2 = self.v_scalar_read(64, vm);
        let result = self.ir.ir().vector_logical_v_shift(64, op1, op2);
        self.v_scalar_write(64, vd, result);
        true
    }

    pub fn cmeq_zero_1(&mut self, inst: &DecodedInst) -> bool {
        let (size, vn, vd) = self.scalar_two_zero_args(inst);
        self.scalar_compare(size, vn, vd, CmpVariant::Zero, CmpKind::Eq)
    }
    pub fn cmge_zero_1(&mut self, inst: &DecodedInst) -> bool {
        let (size, vn, vd) = self.scalar_two_zero_args(inst);
        self.scalar_compare(size, vn, vd, CmpVariant::Zero, CmpKind::Ge)
    }
    pub fn cmgt_zero_1(&mut self, inst: &DecodedInst) -> bool {
        let (size, vn, vd) = self.scalar_two_zero_args(inst);
        self.scalar_compare(size, vn, vd, CmpVariant::Zero, CmpKind::Gt)
    }
    pub fn cmle_1(&mut self, inst: &DecodedInst) -> bool {
        let (size, vn, vd) = self.scalar_two_zero_args(inst);
        self.scalar_compare(size, vn, vd, CmpVariant::Zero, CmpKind::Le)
    }
    pub fn cmlt_1(&mut self, inst: &DecodedInst) -> bool {
        let (size, vn, vd) = self.scalar_two_zero_args(inst);
        self.scalar_compare(size, vn, vd, CmpVariant::Zero, CmpKind::Lt)
    }

    /// FCMEQ (register, scalar, half-precision).
    /// `01011110010mmmmm001001nnnnnddddd` — esize=16.
    pub fn fcmeq_reg_1(&mut self, inst: &DecodedInst) -> bool {
        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let lhs = self.v_scalar_read(128, vn);
        let rhs = self.v_scalar_read(128, vm);
        let result = self.ir.ir().fp_vector_equal(16, lhs, rhs, true);
        let elem = self.ir.ir().vector_get_element(16, result, 0);
        self.v_scalar_write(16, vd, elem);
        true
    }

    /// FCMEQ (register, scalar, single/double).
    /// `010111100z1mmmmm111001nnnnnddddd` — sz at bit 22.
    pub fn fcmeq_reg_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        self.scalar_fp_compare_register(sz, vm, vn, vd, FpCmpKind::Eq)
    }

    fn scalar_fp_compare_register(
        &mut self,
        sz: bool,
        vm: Vec,
        vn: Vec,
        vd: Vec,
        kind: FpCmpKind,
    ) -> bool {
        let esize = if sz { 64 } else { 32 };
        let op1 = self.v_scalar_read(esize, vn);
        let op2 = self.v_scalar_read(esize, vm);
        let result: Value = match kind {
            FpCmpKind::Eq => self.ir.ir().fp_vector_equal(esize, op1, op2, true),
            FpCmpKind::Ge => self.ir.ir().fp_vector_greater_equal(esize, op1, op2, true),
            FpCmpKind::Gt => self.ir.ir().fp_vector_greater(esize, op1, op2, true),
        };
        let elem = self.ir.ir().vector_get_element(esize, result, 0);
        self.v_scalar_write(esize, vd, elem);
        true
    }

    /// FCMGE (register, scalar). `011111100z1mmmmm111001nnnnnddddd`. sz at bit 22.
    pub fn fcmge_reg_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        self.scalar_fp_compare_register(sz, vm, vn, vd, FpCmpKind::Ge)
    }

    /// FCMGT (register, scalar). `011111101z1mmmmm111001nnnnnddddd`. sz at bit 22.
    pub fn fcmgt_reg_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        self.scalar_fp_compare_register(sz, vm, vn, vd, FpCmpKind::Gt)
    }

    /// FABD (scalar, single/double). `011111101z1mmmmm110101nnnnnddddd`.
    /// Upstream: `TranslatorVisitor::FABD_2`.
    pub fn fabd_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let esize = if sz { 64 } else { 32 };
        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());

        let op1 = self.v_scalar_read(esize, vn);
        let op2 = self.v_scalar_read(esize, vm);
        let diff = self.ir.ir().fp_sub(esize, op1, op2);
        let result = self.ir.ir().fp_abs(esize, diff);
        self.v_scalar_write(esize, vd, result);
        true
    }

    /// FRECPS (scalar, half-precision). `01011110010mmmmm001111nnnnnddddd`.
    pub fn frecps_1(&mut self, inst: &DecodedInst) -> bool {
        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let operand1 = self.v_scalar_read(16, vn);
        let operand2 = self.v_scalar_read(16, vm);
        let result = self.ir.ir().fp_recip_step_fused(16, operand1, operand2);
        self.v_scalar_write(16, vd, result);
        true
    }

    /// FRECPS (scalar, single/double). `010111100z1mmmmm111111nnnnnddddd`.
    pub fn frecps_2(&mut self, inst: &DecodedInst) -> bool {
        let esize = if inst.bit(22) { 64 } else { 32 };
        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let operand1 = self.v_scalar_read(esize, vn);
        let operand2 = self.v_scalar_read(esize, vm);
        let result = self.ir.ir().fp_recip_step_fused(esize, operand1, operand2);
        self.v_scalar_write(esize, vd, result);
        true
    }

    /// FRSQRTS (scalar, half-precision). `01011110110mmmmm001111nnnnnddddd`.
    pub fn frsqrts_1(&mut self, inst: &DecodedInst) -> bool {
        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let operand1 = self.v_scalar_read(16, vn);
        let operand2 = self.v_scalar_read(16, vm);
        let result = self.ir.ir().fp_rsqrt_step_fused(16, operand1, operand2);
        self.v_scalar_write(16, vd, result);
        true
    }

    /// FRSQRTS (scalar, single/double). `010111101z1mmmmm111111nnnnnddddd`.
    pub fn frsqrts_2(&mut self, inst: &DecodedInst) -> bool {
        let esize = if inst.bit(22) { 64 } else { 32 };
        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let operand1 = self.v_scalar_read(esize, vn);
        let operand2 = self.v_scalar_read(esize, vm);
        let result = self.ir.ir().fp_rsqrt_step_fused(esize, operand1, operand2);
        self.v_scalar_write(esize, vd, result);
        true
    }
}

#[derive(Copy, Clone)]
enum FpCmpKind {
    Eq,
    Ge,
    Gt,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a64::decoder::{decode, A64InstructionName};
    use crate::ir::block::Block;
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::terminal::Terminal;

    fn translate_one(raw: u32) -> (Block, bool, A64InstructionName) {
        let decoded = decode(raw).expect("instruction should decode");
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let mut visitor = TranslatorVisitor::new(
            &mut block,
            A64LocationDescriptor::new(0x1000, 0, false),
            crate::frontend::a64::translate::visitor::TranslationOptions::default(),
        );
        let should_continue = visitor.dispatch(&decoded);
        drop(visitor);
        (block, should_continue, decoded.name)
    }

    #[test]
    fn fabd_2_encoding_translates_without_interpret_terminal() {
        let (block, should_continue, name) = translate_one(0x7EA8_D560);
        assert_eq!(name, A64InstructionName::FABD_2);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::FPAbs32));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn observed_frsqrts_scalar_encoding_translates_instead_of_interpreting() {
        let (block, should_continue, name) = translate_one(0x5EB0_FE52);
        assert_eq!(name, A64InstructionName::FRSQRTS_2);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::FPRSqrtStepFused32));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn scalar_fp_reciprocal_step_families_use_matching_ir_opcodes() {
        let cases = [
            (0x5E40_3C00, Opcode::FPRecipStepFused16),
            (0x5E20_FC00, Opcode::FPRecipStepFused32),
            (0x5E60_FC00, Opcode::FPRecipStepFused64),
            (0x5EC0_3C00, Opcode::FPRSqrtStepFused16),
            (0x5EA0_FC00, Opcode::FPRSqrtStepFused32),
            (0x5EE0_FC00, Opcode::FPRSqrtStepFused64),
        ];

        for (encoding, expected_opcode) in cases {
            let (block, should_continue, _) = translate_one(encoding);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert!(
                block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == expected_opcode),
                "encoding 0x{encoding:08X} did not emit {expected_opcode:?}"
            );
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }

    #[test]
    fn scalar_fcmeq_register_single_and_double_match_upstream() {
        let cases = [
            (0x5E20_E400, Opcode::FPVectorEqual32),
            (0x5E60_E400, Opcode::FPVectorEqual64),
        ];

        for (encoding, expected_opcode) in cases {
            let (block, should_continue, name) = translate_one(encoding);
            assert_eq!(name, A64InstructionName::FCMEQ_reg_2);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert!(block
                .instructions
                .iter()
                .any(|inst| inst.opcode == expected_opcode));
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }

    #[test]
    fn scalar_integer_comparisons_use_edens_ir_helpers() {
        let cases = [
            (
                0x7EE2_3420,
                A64InstructionName::CMHI_1,
                vec![Opcode::VectorMinU64, Opcode::VectorEqual64, Opcode::VectorNot],
            ),
            (
                0x7EE0_9820,
                A64InstructionName::CMLE_1,
                vec![Opcode::VectorGreaterS64, Opcode::VectorNot],
            ),
            (
                0x5EE0_A820,
                A64InstructionName::CMLT_1,
                vec![
                    Opcode::VectorGreaterS64,
                    Opcode::VectorEqual64,
                    Opcode::VectorOr,
                    Opcode::VectorNot,
                ],
            ),
        ];

        for (encoding, expected_name, expected_opcodes) in cases {
            let (block, should_continue, name) = translate_one(encoding);
            assert_eq!(name, expected_name);
            assert!(should_continue);
            for opcode in expected_opcodes {
                assert!(block.instructions.iter().any(|inst| inst.opcode == opcode));
            }
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }
}
