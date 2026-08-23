//! Port of upstream `dynarmic/frontend/A64/translate/impl/simd_scalar_x_indexed_element.cpp`.

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;

enum ExtraBehavior {
    None,
    Accumulate,
    Subtract,
    MultiplyExtended,
}

fn combine_scalar(size: u32, h: u32, l: u32, m: u32, vmlo: u32) -> (u8, Vec) {
    if size == 0b01 {
        return (((h << 2) | (l << 1) | m) as u8, Vec::from_u32(vmlo));
    }

    (((h << 1) | l) as u8, Vec::from_u32((m << 4) | vmlo))
}

fn multiply_by_element(
    visitor: &mut TranslatorVisitor<'_>,
    sz: bool,
    l: u32,
    m: u32,
    vmlo: u32,
    h: u32,
    vn: Vec,
    vd: Vec,
    extra_behavior: ExtraBehavior,
) -> bool {
    if sz && l == 1 {
        return visitor.reserved_value();
    }

    let idxdsize = if h == 1 { 128 } else { 64 };
    let index = if sz { h } else { (h << 1) | l } as u8;
    let vm = Vec::from_u32((m << 4) | vmlo);
    let esize = if sz { 64 } else { 32 };

    let indexed = visitor.v_read(idxdsize, vm);
    let element = visitor.ir.ir().vector_get_element(esize, indexed, index);
    let mut operand1 = visitor.v_scalar_read(esize, vn);
    let result = match extra_behavior {
        ExtraBehavior::None => visitor.ir.ir().fp_mul(esize, operand1, element),
        ExtraBehavior::MultiplyExtended => visitor.ir.ir().fp_mulx(esize, operand1, element),
        ExtraBehavior::Accumulate | ExtraBehavior::Subtract => {
            if matches!(extra_behavior, ExtraBehavior::Subtract) {
                operand1 = visitor.ir.ir().fp_neg(esize, operand1);
            }
            let operand2 = visitor.v_scalar_read(esize, vd);
            visitor
                .ir
                .ir()
                .fp_mul_add(esize, operand2, operand1, element)
        }
    };

    visitor.v_scalar_write(esize, vd, result);
    true
}

fn multiply_by_element_half_precision(
    visitor: &mut TranslatorVisitor<'_>,
    l: u32,
    m: u32,
    vmlo: u32,
    h: u32,
    vn: Vec,
    vd: Vec,
    extra_behavior: ExtraBehavior,
) -> bool {
    let esize = 16;
    let idxsize = if h == 1 { 128 } else { 64 };
    let index = ((h << 2) | (l << 1) | m) as u8;

    let vm = Vec::from_u32(vmlo);
    let indexed = visitor.v_read(idxsize, vm);
    let element = visitor.ir.ir().vector_get_element(esize, indexed, index);
    let mut operand1 = visitor.v_scalar_read(esize, vn);

    assert!(
        !matches!(
            extra_behavior,
            ExtraBehavior::None | ExtraBehavior::MultiplyExtended
        ),
        "half-precision scalar multiply only supports accumulate/subtract"
    );
    if matches!(extra_behavior, ExtraBehavior::Subtract) {
        operand1 = visitor.ir.ir().fp_neg(esize, operand1);
    }

    let operand2 = visitor.v_scalar_read(esize, vd);
    let result = visitor
        .ir
        .ir()
        .fp_mul_add(esize, operand2, operand1, element);

    visitor.v_scalar_write(esize, vd, result);
    true
}

impl<'a> TranslatorVisitor<'a> {
    pub fn fmla_elt_1(&mut self, inst: &DecodedInst) -> bool {
        let l = inst.bit(21) as u32;
        let m = inst.bit(20) as u32;
        let vmlo = inst.bits(19, 16);
        let h = inst.bit(11) as u32;
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        multiply_by_element_half_precision(self, l, m, vmlo, h, vn, vd, ExtraBehavior::Accumulate)
    }

    pub fn fmla_elt_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let l = inst.bit(21) as u32;
        let m = inst.bit(20) as u32;
        let vmlo = inst.bits(19, 16);
        let h = inst.bit(11) as u32;
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        multiply_by_element(self, sz, l, m, vmlo, h, vn, vd, ExtraBehavior::Accumulate)
    }

    pub fn fmls_elt_1(&mut self, inst: &DecodedInst) -> bool {
        let l = inst.bit(21) as u32;
        let m = inst.bit(20) as u32;
        let vmlo = inst.bits(19, 16);
        let h = inst.bit(11) as u32;
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        multiply_by_element_half_precision(self, l, m, vmlo, h, vn, vd, ExtraBehavior::Subtract)
    }

    pub fn fmls_elt_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let l = inst.bit(21) as u32;
        let m = inst.bit(20) as u32;
        let vmlo = inst.bits(19, 16);
        let h = inst.bit(11) as u32;
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        multiply_by_element(self, sz, l, m, vmlo, h, vn, vd, ExtraBehavior::Subtract)
    }

    /// FMUL (scalar, by element), single/double.
    /// Upstream: `TranslatorVisitor::FMUL_elt_2`.
    pub fn fmul_elt_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let l = inst.bits(21, 21);
        let m = inst.bits(20, 20);
        let vmlo = inst.bits(19, 16);
        let h = inst.bits(11, 11);
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        multiply_by_element(self, sz, l, m, vmlo, h, vn, vd, ExtraBehavior::None)
    }

    /// FMULX (scalar, by element), single/double.
    /// Upstream: `TranslatorVisitor::FMULX_elt_2`.
    pub fn fmulx_elt_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let l = inst.bits(21, 21);
        let m = inst.bits(20, 20);
        let vmlo = inst.bits(19, 16);
        let h = inst.bits(11, 11);
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        multiply_by_element(
            self,
            sz,
            l,
            m,
            vmlo,
            h,
            vn,
            vd,
            ExtraBehavior::MultiplyExtended,
        )
    }

    pub fn sqdmulh_elt_1(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.bits(23, 22);
        if size == 0b00 || size == 0b11 {
            return self.reserved_value();
        }

        let l = inst.bit(21) as u32;
        let m = inst.bit(20) as u32;
        let vmlo = inst.bits(19, 16);
        let h = inst.bit(11) as u32;
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let esize = 8usize << size;
        let (index, vm) = combine_scalar(size, h, l, m, vmlo);

        let operand1 = self.v_scalar_read(esize, vn);
        let operand2 = self.v_read(128, vm);
        let operand2 = self.ir.ir().vector_get_element(esize, operand2, index);
        let result = self
            .ir
            .ir()
            .signed_saturated_doubling_multiply_return_high(operand1, operand2);

        self.v_scalar_write(esize, vd, result);
        true
    }

    pub fn sqrdmulh_elt_1(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.bits(23, 22);
        if size == 0b00 || size == 0b11 {
            return self.reserved_value();
        }

        let l = inst.bit(21) as u32;
        let m = inst.bit(20) as u32;
        let vmlo = inst.bits(19, 16);
        let h = inst.bit(11) as u32;
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let esize = 8usize << size;
        let (index, vm) = combine_scalar(size, h, l, m, vmlo);

        let operand1 = self.v_read(128, vn);
        let operand1 = self.ir.ir().vector_get_element(esize, operand1, 0);
        let operand1 = self.ir.ir().zero_extend_to_quad(operand1);
        let operand2 = self.v_read(128, vm);
        let broadcast = self
            .ir
            .ir()
            .vector_broadcast_element(esize, operand2, index);
        let result = self
            .ir
            .ir()
            .vector_signed_saturated_doubling_multiply_high_rounding(esize, operand1, broadcast);

        self.v_write(128, vd, result);
        true
    }

    pub fn sqdmull_elt_1(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.bits(23, 22);
        if size == 0b00 || size == 0b11 {
            return self.reserved_value();
        }

        let l = inst.bit(21) as u32;
        let m = inst.bit(20) as u32;
        let vmlo = inst.bits(19, 16);
        let h = inst.bit(11) as u32;
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let esize = 8usize << size;
        let (index, vm) = combine_scalar(size, h, l, m, vmlo);

        let operand1 = self.v_read(128, vn);
        let operand1 = self.ir.ir().vector_get_element(esize, operand1, 0);
        let operand1 = self.ir.ir().zero_extend_to_quad(operand1);
        let operand2 = self.v_read(128, vm);
        let broadcast = self
            .ir
            .ir()
            .vector_broadcast_element(esize, operand2, index);
        let result = self
            .ir
            .ir()
            .vector_signed_saturated_doubling_multiply_long(esize, operand1, broadcast);

        self.v_write(128, vd, result);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a64::decoder::decode;
    use crate::ir::block::Block;
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::terminal::Terminal;

    fn translate_one(raw: u32) -> (Block, bool) {
        let decoded = decode(raw).expect("instruction should decode");
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let mut visitor = TranslatorVisitor::new(
            &mut block,
            A64LocationDescriptor::new(0x1000, 0, false),
            crate::frontend::a64::translate::visitor::TranslationOptions::default(),
        );
        let should_continue = visitor.dispatch(&decoded);
        drop(visitor);
        (block, should_continue)
    }

    fn opcode_count(block: &Block, opcode: Opcode) -> usize {
        block
            .instructions
            .iter()
            .filter(|instruction| instruction.opcode == opcode)
            .count()
    }

    #[test]
    fn combine_scalar_matches_upstream_index_and_register_concatenation() {
        assert_eq!(combine_scalar(0b01, 1, 0, 1, 7), (5, Vec::V7));
        assert_eq!(combine_scalar(0b10, 1, 0, 1, 7), (2, Vec::V23));
    }

    #[test]
    fn fmul_s_by_element_encoding_translates_without_interpret_terminal() {
        let (block, should_continue) = translate_one(0x5FA0_9010);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::FPMul32));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn indexed_fp_element_uses_vector_read_before_element_extraction() {
        let (block, should_continue) = translate_one(0x5FA0_9010);

        assert!(should_continue);
        assert_eq!(opcode_count(&block, Opcode::A64GetD), 1);
        assert_eq!(opcode_count(&block, Opcode::VectorGetElement64), 0);
    }

    #[test]
    fn fmulx_s_by_element_translates_without_interpret_terminal() {
        // FMULX Sd, Sn, Vm.S[0] — sz=0 (esize=32).
        let (block, should_continue) = translate_one(0x7F82_9020);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::FPMulX32));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn scalar_fmla_fmls_by_element_family_matches_upstream_opcodes() {
        let cases = [
            (0x5F87_1871, Opcode::FPMulAdd32),
            (0x5FC6_18A4, Opcode::FPMulAdd64),
            (0x5FA3_5041, Opcode::FPMulAdd32),
            (0x5F13_1841, Opcode::FPMulAdd16),
            (0x5F26_50A4, Opcode::FPMulAdd16),
        ];

        for (encoding, expected_opcode) in cases {
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert!(
                block
                    .instructions
                    .iter()
                    .any(|instruction| instruction.opcode == expected_opcode),
                "encoding 0x{encoding:08X} did not emit {expected_opcode:?}"
            );
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }

    #[test]
    fn scalar_saturating_multiply_by_element_family_matches_upstream() {
        let cases = [
            (
                0x5F40_B000,
                Opcode::VectorSignedSaturatedDoublingMultiplyLong16,
            ),
            (
                0x5F40_C000,
                Opcode::SignedSaturatedDoublingMultiplyReturnHigh16,
            ),
            (
                0x5F40_D000,
                Opcode::VectorSignedSaturatedDoublingMultiplyHighRounding16,
            ),
            (
                0x5F80_B000,
                Opcode::VectorSignedSaturatedDoublingMultiplyLong32,
            ),
            (
                0x5F80_C000,
                Opcode::SignedSaturatedDoublingMultiplyReturnHigh32,
            ),
            (
                0x5F80_D000,
                Opcode::VectorSignedSaturatedDoublingMultiplyHighRounding32,
            ),
        ];

        for (encoding, expected_opcode) in cases {
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert!(
                block
                    .instructions
                    .iter()
                    .any(|instruction| instruction.opcode == expected_opcode),
                "encoding 0x{encoding:08X} did not emit {expected_opcode:?}"
            );
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }

    #[test]
    fn scalar_saturating_multiply_rejects_byte_and_64_bit_sizes() {
        for encoding in [0x5F00_B000, 0x5FC0_B000, 0x5F00_C000, 0x5FC0_D000] {
            let (block, should_continue) = translate_one(encoding);
            assert!(!should_continue, "encoding 0x{encoding:08X}");
            assert!(block
                .instructions
                .iter()
                .any(|instruction| instruction.opcode == Opcode::A64ExceptionRaised));
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }
}
