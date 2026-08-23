//! Port of upstream
//! `dynarmic/frontend/A64/translate/impl/simd_scalar_pairwise.cpp`.

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;

#[derive(Clone, Copy)]
enum MinMaxOperation {
    Max,
    MaxNumeric,
    Min,
    MinNumeric,
}

impl<'a> TranslatorVisitor<'a> {
    fn fp_pairwise_min_max(&mut self, inst: &DecodedInst, operation: MinMaxOperation) -> bool {
        let esize = if inst.bit(22) { 64 } else { 32 };
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());

        let operand = self.v_scalar_read(128, vn);
        let element1 = self.ir.ir().vector_get_element(esize, operand, 0);
        let element2 = self.ir.ir().vector_get_element(esize, operand, 1);
        let result = match operation {
            MinMaxOperation::Max => self.ir.ir().fp_max(esize, element1, element2),
            MinMaxOperation::MaxNumeric => self.ir.ir().fp_max_numeric(esize, element1, element2),
            MinMaxOperation::Min => self.ir.ir().fp_min(esize, element1, element2),
            MinMaxOperation::MinNumeric => self.ir.ir().fp_min_numeric(esize, element1, element2),
        };
        let result = self.ir.ir().zero_extend_to_quad(result);
        self.v_scalar_write(128, vd, result);
        true
    }

    /// ADDP (scalar). `01011110zz110001101110nnnnnddddd` — size==0b11 only.
    pub fn addp_pair(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.bits(23, 22);
        if size != 0b11 {
            return self.reserved_value();
        }
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());

        let operand = self.v_scalar_read(128, vn);
        let elem0 = self.ir.ir().vector_get_element(64, operand, 0);
        let elem1 = self.ir.ir().vector_get_element(64, operand, 1);
        let zero = self.ir.ir().imm1(false);
        let sum = self.ir.ir().add_64(elem0, elem1, zero);
        let result = self.ir.ir().zero_extend_to_quad(sum);
        self.v_scalar_write(128, vd, result);
        true
    }

    /// FADDP (scalar, single/double).
    pub fn faddp_pair_2(&mut self, inst: &DecodedInst) -> bool {
        let esize = if inst.bit(22) { 64 } else { 32 };
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());

        let operand = self.v_scalar_read(128, vn);
        let operand1 = self.ir.ir().vector_get_element(esize, operand, 0);
        let operand2 = self.ir.ir().vector_get_element(esize, operand, 1);
        let result = self.ir.ir().fp_add(esize, operand1, operand2);
        let result = self.ir.ir().zero_extend_to_quad(result);
        self.v_scalar_write(128, vd, result);
        true
    }

    pub fn fmaxnmp_pair_2(&mut self, inst: &DecodedInst) -> bool {
        self.fp_pairwise_min_max(inst, MinMaxOperation::MaxNumeric)
    }

    pub fn fmaxp_pair_2(&mut self, inst: &DecodedInst) -> bool {
        self.fp_pairwise_min_max(inst, MinMaxOperation::Max)
    }

    pub fn fminnmp_pair_2(&mut self, inst: &DecodedInst) -> bool {
        self.fp_pairwise_min_max(inst, MinMaxOperation::MinNumeric)
    }

    pub fn fminp_pair_2(&mut self, inst: &DecodedInst) -> bool {
        self.fp_pairwise_min_max(inst, MinMaxOperation::Min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a64::decoder::decode;
    use crate::frontend::a64::translate::TranslationOptions;
    use crate::ir::block::Block;
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;

    fn translate_one(raw: u32) -> (Block, bool) {
        let decoded = decode(raw).expect("instruction should decode");
        let location = A64LocationDescriptor::new(0x1000, 0, false);
        let mut block = Block::new(location.to_location());
        let mut visitor =
            TranslatorVisitor::new(&mut block, location, TranslationOptions::default());
        let should_continue = visitor.dispatch(&decoded);
        drop(visitor);
        (block, should_continue)
    }

    #[test]
    fn scalar_fp_pairwise_family_matches_upstream_opcodes() {
        let cases = [
            (0x7E30_D8A5, Opcode::FPAdd32),
            (0x7E70_D8A5, Opcode::FPAdd64),
            (0x7E30_C8A5, Opcode::FPMaxNumeric32),
            (0x7E30_F8A5, Opcode::FPMax32),
            (0x7EB0_C8A5, Opcode::FPMinNumeric32),
            (0x7EB0_F8A5, Opcode::FPMin32),
        ];

        for (encoding, expected_opcode) in cases {
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert!(
                block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == expected_opcode),
                "encoding 0x{encoding:08X} did not emit {expected_opcode:?}"
            );
        }
    }
}
