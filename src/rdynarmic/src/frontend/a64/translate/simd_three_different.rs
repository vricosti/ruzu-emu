//! Port of upstream `dynarmic/frontend/A64/translate/impl/simd_three_different.cpp`.

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LongOperationBehavior {
    Addition,
    Subtraction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WideOperationBehavior {
    Addition,
    Subtraction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MultiplyLongBehavior {
    None,
    Accumulate,
    Subtract,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AbsoluteDifferenceBehavior {
    None,
    Accumulate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SignednessStd {
    Signed,
    Unsigned,
}

fn absolute_difference_long(
    visitor: &mut TranslatorVisitor<'_>,
    inst: &DecodedInst,
    behavior: AbsoluteDifferenceBehavior,
    signedness: SignednessStd,
) -> bool {
    let q = inst.bit(30);
    let size = inst.bits(23, 22);
    if size == 0b11 {
        return visitor.reserved_value();
    }

    let esize = 8usize << size;
    let vm = Vec::from_u32(inst.bits(20, 16));
    let vn = Vec::from_u32(inst.bits(9, 5));
    let vd = Vec::from_u32(inst.rd());
    let operand1 = visitor.vpart_read_64(vn, usize::from(q));
    let operand1 = visitor.ir.ir().vector_zero_extend(esize, operand1);
    let operand2 = visitor.vpart_read_64(vm, usize::from(q));
    let operand2 = visitor.ir.ir().vector_zero_extend(esize, operand2);
    let mut result = match signedness {
        SignednessStd::Signed => visitor
            .ir
            .ir()
            .vector_signed_absolute_difference(esize, operand1, operand2),
        SignednessStd::Unsigned => visitor
            .ir
            .ir()
            .vector_unsigned_absolute_difference(esize, operand1, operand2),
    };
    if matches!(behavior, AbsoluteDifferenceBehavior::Accumulate) {
        let accumulator = visitor.v_read(128, vd);
        result = visitor.ir.ir().vector_add(2 * esize, result, accumulator);
    }
    visitor.v_write(128, vd, result);
    true
}

fn multiply_long(
    visitor: &mut TranslatorVisitor<'_>,
    inst: &DecodedInst,
    behavior: MultiplyLongBehavior,
    signedness: SignednessStd,
) -> bool {
    let q = inst.bit(30);
    let size = inst.bits(23, 22);
    if size == 0b11 {
        return visitor.reserved_value();
    }

    let esize = 8usize << size;
    let vm = Vec::from_u32(inst.bits(20, 16));
    let vn = Vec::from_u32(inst.bits(9, 5));
    let vd = Vec::from_u32(inst.rd());
    let operand1 = visitor.vpart_read_64(vn, usize::from(q));
    let operand2 = visitor.vpart_read_64(vm, usize::from(q));
    let mut result = match signedness {
        SignednessStd::Signed => visitor
            .ir
            .ir()
            .vector_multiply_signed_widen(esize, operand1, operand2),
        SignednessStd::Unsigned => visitor
            .ir
            .ir()
            .vector_multiply_unsigned_widen(esize, operand1, operand2),
    };
    match behavior {
        MultiplyLongBehavior::None => {}
        MultiplyLongBehavior::Accumulate => {
            let addend = visitor.v_read(128, vd);
            result = visitor.ir.ir().vector_add(2 * esize, addend, result);
        }
        MultiplyLongBehavior::Subtract => {
            let minuend = visitor.v_read(128, vd);
            result = visitor.ir.ir().vector_sub(2 * esize, minuend, result);
        }
    }
    visitor.v_write(128, vd, result);
    true
}

fn long_operation(
    visitor: &mut TranslatorVisitor<'_>,
    inst: &DecodedInst,
    behavior: LongOperationBehavior,
    signedness: SignednessStd,
) -> bool {
    let q = inst.bit(30);
    let size = inst.bits(23, 22);
    if size == 0b11 {
        return visitor.reserved_value();
    }

    let esize = 8usize << size;
    let vm = Vec::from_u32(inst.bits(20, 16));
    let vn = Vec::from_u32(inst.bits(9, 5));
    let vd = Vec::from_u32(inst.rd());
    let operand1 = visitor.vpart_read_64(vn, usize::from(q));
    let operand1 = match signedness {
        SignednessStd::Signed => visitor.ir.ir().vector_sign_extend(esize, operand1),
        SignednessStd::Unsigned => visitor.ir.ir().vector_zero_extend(esize, operand1),
    };
    let operand2 = visitor.vpart_read_64(vm, usize::from(q));
    let operand2 = match signedness {
        SignednessStd::Signed => visitor.ir.ir().vector_sign_extend(esize, operand2),
        SignednessStd::Unsigned => visitor.ir.ir().vector_zero_extend(esize, operand2),
    };
    let result = match behavior {
        LongOperationBehavior::Addition => {
            visitor.ir.ir().vector_add(2 * esize, operand1, operand2)
        }
        LongOperationBehavior::Subtraction => {
            visitor.ir.ir().vector_sub(2 * esize, operand1, operand2)
        }
    };
    visitor.v_write(128, vd, result);
    true
}

fn wide_operation(
    visitor: &mut TranslatorVisitor<'_>,
    inst: &DecodedInst,
    behavior: WideOperationBehavior,
    signedness: SignednessStd,
) -> bool {
    let q = inst.bit(30);
    let size = inst.bits(23, 22);
    if size == 0b11 {
        return visitor.reserved_value();
    }

    let esize = 8usize << size;
    let vm = Vec::from_u32(inst.bits(20, 16));
    let vn = Vec::from_u32(inst.bits(9, 5));
    let vd = Vec::from_u32(inst.rd());
    let operand1 = visitor.v_read(128, vn);
    let operand2 = visitor.vpart_read_64(vm, usize::from(q));
    let operand2 = match signedness {
        SignednessStd::Signed => visitor.ir.ir().vector_sign_extend(esize, operand2),
        SignednessStd::Unsigned => visitor.ir.ir().vector_zero_extend(esize, operand2),
    };
    let result = match behavior {
        WideOperationBehavior::Addition => {
            visitor.ir.ir().vector_add(2 * esize, operand1, operand2)
        }
        WideOperationBehavior::Subtraction => {
            visitor.ir.ir().vector_sub(2 * esize, operand1, operand2)
        }
    };
    visitor.v_write(128, vd, result);
    true
}

impl<'a> TranslatorVisitor<'a> {
    pub fn pmull(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b01 || size == 0b10 {
            return self.reserved_value();
        }

        let esize = 8usize << size;
        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let operand1 = self.vpart_read_64(vn, usize::from(q));
        let operand2 = self.vpart_read_64(vm, usize::from(q));
        let result = self
            .ir
            .ir()
            .vector_polynomial_multiply_long(esize, operand1, operand2);
        self.v_write(128, vd, result);
        true
    }

    pub fn saddl(&mut self, inst: &DecodedInst) -> bool {
        long_operation(
            self,
            inst,
            LongOperationBehavior::Addition,
            SignednessStd::Signed,
        )
    }

    pub fn ssubl(&mut self, inst: &DecodedInst) -> bool {
        long_operation(
            self,
            inst,
            LongOperationBehavior::Subtraction,
            SignednessStd::Signed,
        )
    }

    pub fn uaddl(&mut self, inst: &DecodedInst) -> bool {
        long_operation(
            self,
            inst,
            LongOperationBehavior::Addition,
            SignednessStd::Unsigned,
        )
    }

    pub fn usubl(&mut self, inst: &DecodedInst) -> bool {
        long_operation(
            self,
            inst,
            LongOperationBehavior::Subtraction,
            SignednessStd::Unsigned,
        )
    }

    pub fn saddw(&mut self, inst: &DecodedInst) -> bool {
        wide_operation(
            self,
            inst,
            WideOperationBehavior::Addition,
            SignednessStd::Signed,
        )
    }

    pub fn ssubw(&mut self, inst: &DecodedInst) -> bool {
        wide_operation(
            self,
            inst,
            WideOperationBehavior::Subtraction,
            SignednessStd::Signed,
        )
    }

    pub fn uaddw(&mut self, inst: &DecodedInst) -> bool {
        wide_operation(
            self,
            inst,
            WideOperationBehavior::Addition,
            SignednessStd::Unsigned,
        )
    }

    pub fn usubw(&mut self, inst: &DecodedInst) -> bool {
        wide_operation(
            self,
            inst,
            WideOperationBehavior::Subtraction,
            SignednessStd::Unsigned,
        )
    }

    pub fn smlal_vec(&mut self, inst: &DecodedInst) -> bool {
        multiply_long(
            self,
            inst,
            MultiplyLongBehavior::Accumulate,
            SignednessStd::Signed,
        )
    }

    pub fn smlsl_vec(&mut self, inst: &DecodedInst) -> bool {
        multiply_long(
            self,
            inst,
            MultiplyLongBehavior::Subtract,
            SignednessStd::Signed,
        )
    }

    pub fn smull_vec(&mut self, inst: &DecodedInst) -> bool {
        multiply_long(
            self,
            inst,
            MultiplyLongBehavior::None,
            SignednessStd::Signed,
        )
    }

    pub fn umlal_vec(&mut self, inst: &DecodedInst) -> bool {
        multiply_long(
            self,
            inst,
            MultiplyLongBehavior::Accumulate,
            SignednessStd::Unsigned,
        )
    }

    pub fn umlsl_vec(&mut self, inst: &DecodedInst) -> bool {
        multiply_long(
            self,
            inst,
            MultiplyLongBehavior::Subtract,
            SignednessStd::Unsigned,
        )
    }

    pub fn umull_vec(&mut self, inst: &DecodedInst) -> bool {
        multiply_long(
            self,
            inst,
            MultiplyLongBehavior::None,
            SignednessStd::Unsigned,
        )
    }

    pub fn sabal(&mut self, inst: &DecodedInst) -> bool {
        absolute_difference_long(
            self,
            inst,
            AbsoluteDifferenceBehavior::Accumulate,
            SignednessStd::Signed,
        )
    }

    pub fn sabdl(&mut self, inst: &DecodedInst) -> bool {
        absolute_difference_long(
            self,
            inst,
            AbsoluteDifferenceBehavior::None,
            SignednessStd::Signed,
        )
    }

    pub fn uabal(&mut self, inst: &DecodedInst) -> bool {
        absolute_difference_long(
            self,
            inst,
            AbsoluteDifferenceBehavior::Accumulate,
            SignednessStd::Unsigned,
        )
    }

    pub fn uabdl(&mut self, inst: &DecodedInst) -> bool {
        absolute_difference_long(
            self,
            inst,
            AbsoluteDifferenceBehavior::None,
            SignednessStd::Unsigned,
        )
    }

    pub fn sqdmull_vec_2(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b00 || size == 0b11 {
            return self.reserved_value();
        }

        let esize = 8usize << size;
        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let operand1 = self.vpart_read_64(vn, usize::from(q));
        let operand2 = self.vpart_read_64(vm, usize::from(q));
        let result = self
            .ir
            .ir()
            .vector_signed_saturated_doubling_multiply_long(esize, operand1, operand2);
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

    fn translate_one(raw: u32) -> (Block, bool) {
        let decoded = decode(raw).expect("instruction should decode");
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let mut visitor = TranslatorVisitor::new(
            &mut block,
            A64LocationDescriptor::new(0x1000, 0, false),
            crate::frontend::a64::translate::TranslationOptions::default(),
        );
        let should_continue = visitor.dispatch(&decoded);
        drop(visitor);
        (block, should_continue)
    }

    fn three_different_encoding(q: bool, size: u32, opcode: u32) -> u32 {
        0x0E20_0000 | ((q as u32) << 30) | (size << 22) | (1 << 16) | (opcode << 10) | (2 << 5)
    }

    #[test]
    fn saddl_spacecadet_encoding_translates_without_interpret_terminal() {
        let (block, should_continue) = translate_one(0x0E650000);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorSignExtend16));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorAdd32));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A64SetQ));
    }

    #[test]
    fn uaddw_encoding_translates_without_interpret_terminal() {
        let (block, should_continue) = translate_one(0x2E24_1046);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorZeroExtend8));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorAdd16));
    }

    #[test]
    fn absolute_difference_long_family_uses_matching_ir_opcodes() {
        let cases = [
            (0x0E27_5085, Opcode::VectorSignedAbsoluteDifference8),
            (0x0E27_7085, Opcode::VectorSignedAbsoluteDifference8),
            (0x2E27_5085, Opcode::VectorUnsignedAbsoluteDifference8),
            (0x2E27_7085, Opcode::VectorUnsignedAbsoluteDifference8),
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

    #[test]
    fn multiply_long_family_uses_matching_ir_opcodes() {
        let cases = [
            (
                0x0E22_8001,
                Opcode::VectorMultiplySignedWiden8,
                Some(Opcode::VectorAdd16),
            ),
            (
                0x0E22_A001,
                Opcode::VectorMultiplySignedWiden8,
                Some(Opcode::VectorSub16),
            ),
            (0x0E22_C001, Opcode::VectorMultiplySignedWiden8, None),
            (
                0x2E22_8001,
                Opcode::VectorMultiplyUnsignedWiden8,
                Some(Opcode::VectorAdd16),
            ),
            (
                0x2E22_A001,
                Opcode::VectorMultiplyUnsignedWiden8,
                Some(Opcode::VectorSub16),
            ),
            (0x2E22_C001, Opcode::VectorMultiplyUnsignedWiden8, None),
        ];

        for (encoding, multiply_opcode, combine_opcode) in cases {
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert!(
                block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == multiply_opcode),
                "encoding 0x{encoding:08X} did not emit {multiply_opcode:?}"
            );
            if let Some(combine_opcode) = combine_opcode {
                assert!(
                    block
                        .instructions
                        .iter()
                        .any(|inst| inst.opcode == combine_opcode),
                    "encoding 0x{encoding:08X} did not emit {combine_opcode:?}"
                );
            }
        }
    }

    #[test]
    fn pmull_accepts_only_upstream_element_sizes() {
        let cases = [
            (
                three_different_encoding(false, 0, 0b111000),
                Some(Opcode::VectorPolynomialMultiplyLong8),
            ),
            (three_different_encoding(false, 1, 0b111000), None),
            (three_different_encoding(false, 2, 0b111000), None),
            (
                three_different_encoding(true, 3, 0b111000),
                Some(Opcode::VectorPolynomialMultiplyLong64),
            ),
        ];

        for (encoding, expected_opcode) in cases {
            let (block, should_continue) = translate_one(encoding);
            if let Some(expected_opcode) = expected_opcode {
                assert!(should_continue, "encoding 0x{encoding:08X}");
                assert!(block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == expected_opcode));
            } else {
                assert!(!should_continue, "encoding 0x{encoding:08X}");
                assert!(block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == Opcode::A64ExceptionRaised));
            }
        }
    }

    #[test]
    fn sqdmull_accepts_only_16_and_32_bit_elements() {
        let cases = [
            (three_different_encoding(false, 0, 0b110100), None),
            (
                three_different_encoding(false, 1, 0b110100),
                Some(Opcode::VectorSignedSaturatedDoublingMultiplyLong16),
            ),
            (
                three_different_encoding(true, 2, 0b110100),
                Some(Opcode::VectorSignedSaturatedDoublingMultiplyLong32),
            ),
            (three_different_encoding(true, 3, 0b110100), None),
        ];

        for (encoding, expected_opcode) in cases {
            let (block, should_continue) = translate_one(encoding);
            if let Some(expected_opcode) = expected_opcode {
                assert!(should_continue, "encoding 0x{encoding:08X}");
                assert!(block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == expected_opcode));
            } else {
                assert!(!should_continue, "encoding 0x{encoding:08X}");
                assert!(block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == Opcode::A64ExceptionRaised));
            }
        }
    }
}
