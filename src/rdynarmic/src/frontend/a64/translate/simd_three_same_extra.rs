//! Port of upstream `dynarmic/frontend/A64/translate/impl/simd_three_same_extra.cpp`.

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;
use crate::ir::emitter::IREmitter;
use crate::ir::value::Value;

type ExtensionFunction<'a> = fn(&mut IREmitter<'a>, Value) -> Value;

fn dot_product<'a>(
    visitor: &mut TranslatorVisitor<'a>,
    inst: &DecodedInst,
    extension: ExtensionFunction<'a>,
) -> bool {
    let q = inst.bit(30);
    let size = inst.bits(23, 22);
    if size != 0b10 {
        return visitor.reserved_value();
    }

    let esize = 8usize << size;
    let datasize = if q { 128 } else { 64 };
    let elements = datasize / esize;
    let vm = Vec::from_u32(inst.bits(20, 16));
    let vn = Vec::from_u32(inst.bits(9, 5));
    let vd = Vec::from_u32(inst.rd());

    let operand1 = visitor.v_read(datasize, vn);
    let operand2 = visitor.v_read(datasize, vm);
    let mut result = visitor.v_read(datasize, vd);

    for i in 0..elements {
        let mut result_element = Value::ImmU32(0);

        for j in 0..4 {
            let element1 = visitor
                .ir
                .ir()
                .vector_get_element(8, operand1, (4 * i + j) as u8);
            let element1 = extension(visitor.ir.ir(), element1);
            let element2 = visitor
                .ir
                .ir()
                .vector_get_element(8, operand2, (4 * i + j) as u8);
            let element2 = extension(visitor.ir.ir(), element2);
            let product = visitor.ir.ir().mul_32(element1, element2);
            result_element = visitor
                .ir
                .ir()
                .add_32(result_element, product, Value::ImmU1(false));
        }

        let accumulator = visitor.ir.ir().vector_get_element(32, result, i as u8);
        result_element = visitor
            .ir
            .ir()
            .add_32(accumulator, result_element, Value::ImmU1(false));
        result = visitor
            .ir
            .ir()
            .vector_set_element(32, result, i as u8, result_element);
    }

    visitor.v_write(datasize, vd, result);
    true
}

impl<'a> TranslatorVisitor<'a> {
    pub fn sdot_vec(&mut self, inst: &DecodedInst) -> bool {
        dot_product(self, inst, IREmitter::sign_extend_to_word)
    }

    pub fn udot_vec(&mut self, inst: &DecodedInst) -> bool {
        dot_product(self, inst, IREmitter::zero_extend_to_word)
    }

    pub fn fcmla_vec(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0 {
            return self.reserved_value();
        }
        if !q && size == 0b11 {
            return self.reserved_value();
        }

        let esize = 8usize << size;
        assert_ne!(esize, 16, "half-precision floating point is unsupported");

        let datasize = if q { 128 } else { 64 };
        let num_elements = datasize / esize;
        let num_iterations = num_elements / 2;
        let vm = Vec::from_u32(inst.bits(20, 16));
        let rot = inst.bits(12, 11);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());

        let operand1 = self.v_read(datasize, vn);
        let operand2 = self.v_read(datasize, vm);
        let operand3 = self.v_read(datasize, vd);
        let mut result = self.ir.ir().zero_vector();

        for e in 0..num_iterations {
            let first = (e * 2) as u8;
            let second = first + 1;

            let (element1, element2, element3, element4) = match rot {
                0b00 => {
                    let element1 = self.ir.ir().vector_get_element(esize, operand2, first);
                    let element2 = self.ir.ir().vector_get_element(esize, operand1, first);
                    let element3 = self.ir.ir().vector_get_element(esize, operand2, second);
                    let element4 = self.ir.ir().vector_get_element(esize, operand1, first);
                    (element1, element2, element3, element4)
                }
                0b01 => {
                    let element1 = self.ir.ir().vector_get_element(esize, operand2, second);
                    let element1 = self.ir.ir().fp_neg(esize, element1);
                    let element2 = self.ir.ir().vector_get_element(esize, operand1, second);
                    let element3 = self.ir.ir().vector_get_element(esize, operand2, first);
                    let element4 = self.ir.ir().vector_get_element(esize, operand1, second);
                    (element1, element2, element3, element4)
                }
                0b10 => {
                    let element1 = self.ir.ir().vector_get_element(esize, operand2, first);
                    let element1 = self.ir.ir().fp_neg(esize, element1);
                    let element2 = self.ir.ir().vector_get_element(esize, operand1, first);
                    let element3 = self.ir.ir().vector_get_element(esize, operand2, second);
                    let element3 = self.ir.ir().fp_neg(esize, element3);
                    let element4 = self.ir.ir().vector_get_element(esize, operand1, first);
                    (element1, element2, element3, element4)
                }
                0b11 => {
                    let element1 = self.ir.ir().vector_get_element(esize, operand2, second);
                    let element2 = self.ir.ir().vector_get_element(esize, operand1, second);
                    let element3 = self.ir.ir().vector_get_element(esize, operand2, first);
                    let element3 = self.ir.ir().fp_neg(esize, element3);
                    let element4 = self.ir.ir().vector_get_element(esize, operand1, second);
                    (element1, element2, element3, element4)
                }
                _ => unreachable!(),
            };

            let operand3_element1 = self.ir.ir().vector_get_element(esize, operand3, first);
            let operand3_element2 = self.ir.ir().vector_get_element(esize, operand3, second);
            let first_result =
                self.ir
                    .ir()
                    .fp_mul_add(esize, operand3_element1, element2, element1);
            result = self
                .ir
                .ir()
                .vector_set_element(esize, result, first, first_result);
            let second_result =
                self.ir
                    .ir()
                    .fp_mul_add(esize, operand3_element2, element4, element3);
            result = self
                .ir
                .ir()
                .vector_set_element(esize, result, second, second_result);
        }

        self.ir.set_q(vd, result);
        true
    }

    pub fn fcadd_vec(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0 {
            return self.reserved_value();
        }
        if !q && size == 0b11 {
            return self.reserved_value();
        }

        let esize = 8usize << size;
        assert_ne!(esize, 16, "half-precision floating point is unsupported");

        let datasize = if q { 128 } else { 64 };
        let num_elements = datasize / esize;
        let num_iterations = num_elements / 2;
        let vm = Vec::from_u32(inst.bits(20, 16));
        let rot = inst.bit(12);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());

        let operand1 = self.v_read(datasize, vn);
        let operand2 = self.v_read(datasize, vm);
        let mut result = self.ir.ir().zero_vector();

        for e in 0..num_iterations {
            let first = (e * 2) as u8;
            let second = first + 1;

            let (element1, element3) = if !rot {
                let element1 = self.ir.ir().vector_get_element(esize, operand2, second);
                let element1 = self.ir.ir().fp_neg(esize, element1);
                let element3 = self.ir.ir().vector_get_element(esize, operand2, first);
                (element1, element3)
            } else {
                let element1 = self.ir.ir().vector_get_element(esize, operand2, second);
                let element3 = self.ir.ir().vector_get_element(esize, operand2, first);
                let element3 = self.ir.ir().fp_neg(esize, element3);
                (element1, element3)
            };

            let operand1_element1 = self.ir.ir().vector_get_element(esize, operand1, first);
            let operand1_element3 = self.ir.ir().vector_get_element(esize, operand1, second);
            let first_result = self.ir.ir().fp_add(esize, operand1_element1, element1);
            result = self
                .ir
                .ir()
                .vector_set_element(esize, result, first, first_result);
            let second_result = self.ir.ir().fp_add(esize, operand1_element3, element3);
            result = self
                .ir
                .ir()
                .vector_set_element(esize, result, second, second_result);
        }

        self.ir.set_q(vd, result);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a64::decoder::decode;
    use crate::frontend::a64::translate::visitor::TranslationOptions;
    use crate::ir::block::Block;
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::terminal::Terminal;

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

    fn dot_product_encoding(q: bool, size: u32, unsigned: bool) -> u32 {
        0x0E01_9443 | ((q as u32) << 30) | ((unsigned as u32) << 29) | (size << 22)
    }

    fn fcmla_encoding(q: bool, size: u32, rot: u32) -> u32 {
        0x2E01_C443 | ((q as u32) << 30) | (size << 22) | (rot << 11)
    }

    fn fcadd_encoding(q: bool, size: u32, rot: bool) -> u32 {
        0x2E01_E443 | ((q as u32) << 30) | (size << 22) | ((rot as u32) << 12)
    }

    fn opcode_count(block: &Block, opcode: Opcode) -> usize {
        block
            .instructions
            .iter()
            .filter(|instruction| instruction.opcode == opcode)
            .count()
    }

    #[test]
    fn dot_product_vectors_use_matching_signed_extensions() {
        let cases = [
            (
                dot_product_encoding(false, 2, false),
                Opcode::SignExtendByteToWord,
            ),
            (
                dot_product_encoding(false, 2, true),
                Opcode::ZeroExtendByteToWord,
            ),
        ];

        for (encoding, expected_extension) in cases {
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert_eq!(opcode_count(&block, expected_extension), 16);
            assert_eq!(opcode_count(&block, Opcode::Mul32), 8);
            assert_eq!(opcode_count(&block, Opcode::VectorSetElement32), 2);
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }

    #[test]
    fn dot_product_rejects_non_word_destination_sizes() {
        for size in [0, 1, 3] {
            let (block, should_continue) = translate_one(dot_product_encoding(false, size, false));
            assert!(!should_continue, "size {size}");
            assert!(block
                .instructions
                .iter()
                .any(|instruction| instruction.opcode == Opcode::A64ExceptionRaised));
        }
    }

    #[test]
    fn fcmla_applies_each_upstream_rotation() {
        for (rot, negations) in [(0, 0), (1, 1), (2, 2), (3, 1)] {
            let encoding = fcmla_encoding(false, 2, rot);
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "rotation {rot}");
            assert_eq!(opcode_count(&block, Opcode::FPNeg32), negations);
            assert_eq!(opcode_count(&block, Opcode::FPMulAdd32), 2);
            assert_eq!(opcode_count(&block, Opcode::VectorSetElement32), 2);
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }

    #[test]
    fn fcadd_applies_both_upstream_rotations() {
        for rot in [false, true] {
            let encoding = fcadd_encoding(false, 2, rot);
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "rotation {}", u8::from(rot));
            assert_eq!(opcode_count(&block, Opcode::FPNeg32), 1);
            assert_eq!(opcode_count(&block, Opcode::FPAdd32), 2);
            assert_eq!(opcode_count(&block, Opcode::VectorSetElement32), 2);
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }

    #[test]
    fn complex_vectors_apply_upstream_size_validation() {
        for encoding in [
            fcmla_encoding(false, 0, 0),
            fcmla_encoding(false, 3, 0),
            fcadd_encoding(false, 0, false),
            fcadd_encoding(false, 3, false),
        ] {
            let (block, should_continue) = translate_one(encoding);
            assert!(!should_continue, "encoding 0x{encoding:08X}");
            assert!(block
                .instructions
                .iter()
                .any(|instruction| instruction.opcode == Opcode::A64ExceptionRaised));
        }

        let (fcmla, should_continue) = translate_one(fcmla_encoding(true, 3, 0));
        assert!(should_continue);
        assert_eq!(opcode_count(&fcmla, Opcode::FPMulAdd64), 2);

        let (fcadd, should_continue) = translate_one(fcadd_encoding(true, 3, false));
        assert!(should_continue);
        assert_eq!(opcode_count(&fcadd, Opcode::FPAdd64), 2);
    }
}
