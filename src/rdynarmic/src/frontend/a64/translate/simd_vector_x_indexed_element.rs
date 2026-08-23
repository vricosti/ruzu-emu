//! Port of dynarmic/frontend/A64/translate/impl/simd_vector_x_indexed_element.cpp.
//!
//! SIMD vector x indexed element instructions.

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;
use crate::ir::value::Value;

#[derive(Clone, Copy)]
enum ExtraBehavior {
    None,
    Extended,
    Accumulate,
    Subtract,
}

#[derive(Clone, Copy)]
enum Signedness {
    Signed,
    Unsigned,
}

impl<'a> TranslatorVisitor<'a> {
    fn vector_multiply_by_element(
        &mut self,
        inst: &DecodedInst,
        extra_behavior: ExtraBehavior,
    ) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size != 0b01 && size != 0b10 {
            return self.reserved_value();
        }

        let l = inst.bit(21) as u32;
        let m = inst.bit(20) as u32;
        let vmlo = inst.bits(19, 16);
        let h = inst.bit(11) as u32;
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let (index, vm) = if size == 0b01 {
            ((h << 2) | (l << 1) | m, Vec::from_u32(vmlo))
        } else {
            ((h << 1) | l, Vec::from_u32((m << 4) | vmlo))
        };
        let idxdsize = if h == 1 { 128 } else { 64 };
        let esize = 8usize << size;
        let datasize = if q { 128 } else { 64 };

        let operand1 = self.v_read(datasize, vn);
        let operand2 = self.v_read(idxdsize, vm);
        let index_vector = self
            .ir
            .ir()
            .vector_broadcast_element(esize, operand2, index as u8);
        let product = self.ir.ir().vector_multiply(esize, operand1, index_vector);
        let result = match extra_behavior {
            ExtraBehavior::None => product,
            ExtraBehavior::Accumulate => {
                let accumulator = self.v_read(datasize, vd);
                self.ir.ir().vector_add(esize, accumulator, product)
            }
            ExtraBehavior::Subtract => {
                let accumulator = self.v_read(datasize, vd);
                self.ir.ir().vector_sub(esize, accumulator, product)
            }
            ExtraBehavior::Extended => unreachable!(),
        };

        self.v_write(datasize, vd, result);
        true
    }

    pub fn mla_elt(&mut self, inst: &DecodedInst) -> bool {
        self.vector_multiply_by_element(inst, ExtraBehavior::Accumulate)
    }

    pub fn mls_elt(&mut self, inst: &DecodedInst) -> bool {
        self.vector_multiply_by_element(inst, ExtraBehavior::Subtract)
    }

    pub fn mul_elt(&mut self, inst: &DecodedInst) -> bool {
        self.vector_multiply_by_element(inst, ExtraBehavior::None)
    }

    pub fn fcmla_elt(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        let l = inst.bit(21) as u32;
        let m = inst.bit(20) as u32;
        let vmlo = inst.bits(19, 16);
        let rot = inst.bits(14, 13);
        let h = inst.bit(11) as u32;
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());

        if size == 0b00 || size == 0b11 {
            return self.reserved_value();
        }
        if size == 0b01 && h == 1 && !q {
            return self.reserved_value();
        }
        if size == 0b10 && (l == 1 || !q) {
            return self.reserved_value();
        }

        let esize = 8usize << size;
        assert_ne!(esize, 16, "half-precision floating point is unsupported");

        let index = if size == 0b01 { (h << 1) | l } else { h };
        let vm = Vec::from_u32((m << 4) | vmlo);
        let datasize = if q { 128 } else { 64 };
        let num_iterations = datasize / esize / 2;
        let operand1 = self.v_read(datasize, vn);
        let operand2 = self.v_read(datasize, vm);
        let operand3 = self.v_read(datasize, vd);
        let mut result = self.ir.ir().zero_vector();

        for e in 0..num_iterations {
            let first = (e * 2) as u8;
            let second = first + 1;
            let index_first = (index * 2) as u8;
            let index_second = index_first + 1;

            let (element1, element2, element3, element4) = match rot {
                0b00 => (
                    self.ir
                        .ir()
                        .vector_get_element(esize, operand2, index_first),
                    self.ir.ir().vector_get_element(esize, operand1, first),
                    self.ir
                        .ir()
                        .vector_get_element(esize, operand2, index_second),
                    self.ir.ir().vector_get_element(esize, operand1, first),
                ),
                0b01 => {
                    let element1 = self
                        .ir
                        .ir()
                        .vector_get_element(esize, operand2, index_second);
                    (
                        self.ir.ir().fp_neg(esize, element1),
                        self.ir.ir().vector_get_element(esize, operand1, second),
                        self.ir
                            .ir()
                            .vector_get_element(esize, operand2, index_first),
                        self.ir.ir().vector_get_element(esize, operand1, second),
                    )
                }
                0b10 => {
                    let element1 = self
                        .ir
                        .ir()
                        .vector_get_element(esize, operand2, index_first);
                    let element3 = self
                        .ir
                        .ir()
                        .vector_get_element(esize, operand2, index_second);
                    (
                        self.ir.ir().fp_neg(esize, element1),
                        self.ir.ir().vector_get_element(esize, operand1, first),
                        self.ir.ir().fp_neg(esize, element3),
                        self.ir.ir().vector_get_element(esize, operand1, first),
                    )
                }
                0b11 => {
                    let element3 = self
                        .ir
                        .ir()
                        .vector_get_element(esize, operand2, index_first);
                    (
                        self.ir
                            .ir()
                            .vector_get_element(esize, operand2, index_second),
                        self.ir.ir().vector_get_element(esize, operand1, second),
                        self.ir.ir().fp_neg(esize, element3),
                        self.ir.ir().vector_get_element(esize, operand1, second),
                    )
                }
                _ => unreachable!(),
            };

            let addend1 = self.ir.ir().vector_get_element(esize, operand3, first);
            let addend2 = self.ir.ir().vector_get_element(esize, operand3, second);
            let value1 = self.ir.ir().fp_mul_add(esize, addend1, element2, element1);
            let value2 = self.ir.ir().fp_mul_add(esize, addend2, element4, element3);
            result = self
                .ir
                .ir()
                .vector_set_element(esize, result, first, value1);
            result = self
                .ir
                .ir()
                .vector_set_element(esize, result, second, value2);
        }

        self.v_write(128, vd, result);
        true
    }

    fn fp_multiply_by_element_half_precision(
        &mut self,
        inst: &DecodedInst,
        extra_behavior: ExtraBehavior,
    ) -> bool {
        let q = inst.bit(30);
        let l = inst.bit(21) as u32;
        let m = inst.bit(20) as u32;
        let vmlo = inst.bits(19, 16);
        let h = inst.bit(11) as u32;
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let idxdsize = if h == 1 { 128 } else { 64 };
        let index = ((h << 2) | (l << 1) | m) as u8;
        let vm = Vec::from_u32(vmlo);
        let esize = 16;
        let datasize = if q { 128 } else { 64 };
        let operand1 = self.v_read(datasize, vn);
        let index_source = self.v_read(idxdsize, vm);
        let operand2 = if q {
            self.ir
                .ir()
                .vector_broadcast_element(esize, index_source, index)
        } else {
            let element = self.ir.ir().vector_get_element(esize, index_source, index);
            self.ir.ir().vector_broadcast_lower(esize, element)
        };
        let operand3 = self.v_read(datasize, vd);
        let result = match extra_behavior {
            ExtraBehavior::Accumulate => self
                .ir
                .ir()
                .fp_vector_mul_add(esize, operand3, operand1, operand2, true),
            ExtraBehavior::Subtract => {
                let neg_operand1 = self.ir.ir().fp_vector_neg(esize, operand1);
                self.ir
                    .ir()
                    .fp_vector_mul_add(esize, operand3, neg_operand1, operand2, true)
            }
            ExtraBehavior::None | ExtraBehavior::Extended => unreachable!(),
        };

        self.v_write(datasize, vd, result);
        true
    }

    pub fn fmla_elt_3(&mut self, inst: &DecodedInst) -> bool {
        self.fp_multiply_by_element_half_precision(inst, ExtraBehavior::Accumulate)
    }

    pub fn fmls_elt_3(&mut self, inst: &DecodedInst) -> bool {
        self.fp_multiply_by_element_half_precision(inst, ExtraBehavior::Subtract)
    }

    fn multiply_long_by_element(
        &mut self,
        inst: &DecodedInst,
        extra_behavior: ExtraBehavior,
        sign: Signedness,
    ) -> bool {
        let q = inst.bit(30);
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

        let (index, vm) = if size == 0b01 {
            ((h << 2) | (l << 1) | m, Vec::from_u32(vmlo))
        } else {
            ((h << 1) | l, Vec::from_u32((m << 4) | vmlo))
        };
        let idxsize = if h == 1 { 128 } else { 64 };
        let esize = 8usize << size;

        let operand1 = self.vpart_read_64(vn, q as usize);
        let operand2 = self.v_read(idxsize, vm);
        let index_vector = self
            .ir
            .ir()
            .vector_broadcast_element(esize, operand2, index as u8);
        let product = match sign {
            Signedness::Signed => {
                self.ir
                    .ir()
                    .vector_multiply_signed_widen(esize, operand1, index_vector)
            }
            Signedness::Unsigned => {
                self.ir
                    .ir()
                    .vector_multiply_unsigned_widen(esize, operand1, index_vector)
            }
        };
        let result = match extra_behavior {
            ExtraBehavior::None => product,
            ExtraBehavior::Accumulate => {
                let accumulator = self.v_read(128, vd);
                self.ir.ir().vector_add(2 * esize, accumulator, product)
            }
            ExtraBehavior::Subtract => {
                let accumulator = self.v_read(128, vd);
                self.ir.ir().vector_sub(2 * esize, accumulator, product)
            }
            ExtraBehavior::Extended => unreachable!(),
        };

        self.v_write(128, vd, result);
        true
    }

    fn fp_multiply_by_element(
        &mut self,
        q: bool,
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
            return self.reserved_value();
        }
        if sz && !q {
            return self.reserved_value();
        }

        let idxdsize = if h == 1 { 128 } else { 64 };
        let index = if sz { h } else { (h << 1) | l } as u8;
        let vm = Vec::from_u32((m << 4) | vmlo);
        let esize = if sz { 64 } else { 32 };
        let datasize = if q { 128 } else { 64 };

        let operand1 = self.v_read(datasize, vn);
        let operand2 = {
            let vector = self.v_read(idxdsize, vm);
            if q {
                self.ir.ir().vector_broadcast_element(esize, vector, index)
            } else {
                let element = self.ir.ir().vector_get_element(esize, vector, index);
                self.ir.ir().vector_broadcast_lower(esize, element)
            }
        };
        let operand3 = self.v_read(datasize, vd);

        let result = match extra_behavior {
            ExtraBehavior::None => self.ir.ir().fp_vector_mul(esize, operand1, operand2, true),
            ExtraBehavior::Extended => self.ir.ir().fp_vector_mulx(esize, operand1, operand2, true),
            ExtraBehavior::Accumulate => self
                .ir
                .ir()
                .fp_vector_mul_add(esize, operand3, operand1, operand2, true),
            ExtraBehavior::Subtract => {
                let neg_operand1 = self.ir.ir().fp_vector_neg(esize, operand1);
                self.ir
                    .ir()
                    .fp_vector_mul_add(esize, operand3, neg_operand1, operand2, true)
            }
        };

        self.v_write(datasize, vd, result);
        true
    }

    fn fp_multiply_by_element_fields(
        &mut self,
        inst: &DecodedInst,
        extra_behavior: ExtraBehavior,
    ) -> bool {
        let q = inst.bit(30);
        let sz = inst.bit(22);
        let l = inst.bits(21, 21);
        let m = inst.bits(20, 20);
        let vmlo = inst.bits(19, 16);
        let h = inst.bits(11, 11);
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        self.fp_multiply_by_element(q, sz, l, m, vmlo, h, vn, vd, extra_behavior)
    }

    /// FMLA (vector, by element), single/double.
    /// Upstream: `TranslatorVisitor::FMLA_elt_4`.
    pub fn fmla_elt_4(&mut self, inst: &DecodedInst) -> bool {
        self.fp_multiply_by_element_fields(inst, ExtraBehavior::Accumulate)
    }

    /// FMLS (vector, by element), single/double.
    /// Upstream: `TranslatorVisitor::FMLS_elt_4`.
    pub fn fmls_elt_4(&mut self, inst: &DecodedInst) -> bool {
        self.fp_multiply_by_element_fields(inst, ExtraBehavior::Subtract)
    }

    /// FMUL (vector, by element), single/double.
    /// Upstream: `TranslatorVisitor::FMUL_elt_4`.
    pub fn fmul_elt_4(&mut self, inst: &DecodedInst) -> bool {
        self.fp_multiply_by_element_fields(inst, ExtraBehavior::None)
    }

    /// FMULX (vector, by element), single/double.
    /// Upstream: `TranslatorVisitor::FMULX_elt_4`.
    pub fn fmulx_elt_4(&mut self, inst: &DecodedInst) -> bool {
        self.fp_multiply_by_element_fields(inst, ExtraBehavior::Extended)
    }

    pub fn smlal_elt(&mut self, inst: &DecodedInst) -> bool {
        self.multiply_long_by_element(inst, ExtraBehavior::Accumulate, Signedness::Signed)
    }

    pub fn smlsl_elt(&mut self, inst: &DecodedInst) -> bool {
        self.multiply_long_by_element(inst, ExtraBehavior::Subtract, Signedness::Signed)
    }

    pub fn smull_elt(&mut self, inst: &DecodedInst) -> bool {
        self.multiply_long_by_element(inst, ExtraBehavior::None, Signedness::Signed)
    }

    pub fn umlal_elt(&mut self, inst: &DecodedInst) -> bool {
        self.multiply_long_by_element(inst, ExtraBehavior::Accumulate, Signedness::Unsigned)
    }

    pub fn umlsl_elt(&mut self, inst: &DecodedInst) -> bool {
        self.multiply_long_by_element(inst, ExtraBehavior::Subtract, Signedness::Unsigned)
    }

    pub fn umull_elt(&mut self, inst: &DecodedInst) -> bool {
        self.multiply_long_by_element(inst, ExtraBehavior::None, Signedness::Unsigned)
    }

    pub fn sqdmull_elt_2(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
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
        let (index, vm) = if size == 0b01 {
            ((h << 2) | (l << 1) | m, Vec::from_u32(vmlo))
        } else {
            ((h << 1) | l, Vec::from_u32((m << 4) | vmlo))
        };
        let idxsize = if h == 1 { 128 } else { 64 };
        let esize = 8usize << size;
        let operand1 = self.vpart_read_64(vn, q as usize);
        let operand2 = self.v_read(idxsize, vm);
        let index_vector = self
            .ir
            .ir()
            .vector_broadcast_element(esize, operand2, index as u8);
        let result = self.ir.ir().vector_signed_saturated_doubling_multiply_long(
            esize,
            operand1,
            index_vector,
        );

        self.v_write(128, vd, result);
        true
    }

    fn dot_product_by_element(&mut self, inst: &DecodedInst, sign: Signedness) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size != 0b10 {
            return self.reserved_value();
        }

        let l = inst.bit(21) as u32;
        let m = inst.bit(20) as u32;
        let vmlo = inst.bits(19, 16);
        let h = inst.bit(11) as u32;
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let vm = Vec::from_u32((m << 4) | vmlo);
        let esize = 8usize << size;
        let datasize = if q { 128 } else { 64 };
        let elements = datasize / esize;
        let index = (h << 1) | l;
        let operand1 = self.v_read(datasize, vn);
        let operand2 = self.v_read(128, vm);
        let mut result = self.v_read(datasize, vd);

        for i in 0..elements {
            let mut result_element = Value::ImmU32(0);
            for j in 0..4 {
                let element1 = self
                    .ir
                    .ir()
                    .vector_get_element(8, operand1, (4 * i + j) as u8);
                let element2 =
                    self.ir
                        .ir()
                        .vector_get_element(8, operand2, (4 * index as usize + j) as u8);
                let element1 = match sign {
                    Signedness::Signed => self.ir.ir().sign_extend_byte_to_word(element1),
                    Signedness::Unsigned => self.ir.ir().zero_extend_byte_to_word(element1),
                };
                let element2 = match sign {
                    Signedness::Signed => self.ir.ir().sign_extend_byte_to_word(element2),
                    Signedness::Unsigned => self.ir.ir().zero_extend_byte_to_word(element2),
                };
                let product = self.ir.ir().mul_32(element1, element2);
                result_element = self
                    .ir
                    .ir()
                    .add_32(result_element, product, Value::ImmU1(false));
            }

            let accumulator = self.ir.ir().vector_get_element(32, result, i as u8);
            result_element = self
                .ir
                .ir()
                .add_32(accumulator, result_element, Value::ImmU1(false));
            result = self
                .ir
                .ir()
                .vector_set_element(32, result, i as u8, result_element);
        }

        self.v_write(datasize, vd, result);
        true
    }

    pub fn sdot_elt(&mut self, inst: &DecodedInst) -> bool {
        self.dot_product_by_element(inst, Signedness::Signed)
    }

    pub fn udot_elt(&mut self, inst: &DecodedInst) -> bool {
        self.dot_product_by_element(inst, Signedness::Unsigned)
    }

    pub fn sqdmulh_elt_2(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
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
        let (index, vm) = if size == 0b01 {
            ((h << 2) | (l << 1) | m, Vec::from_u32(vmlo))
        } else {
            ((h << 1) | l, Vec::from_u32((m << 4) | vmlo))
        };
        let idxdsize = if h == 1 { 128 } else { 64 };
        let esize = 8usize << size;
        let datasize = if q { 128 } else { 64 };

        let operand1 = self.v_read(datasize, vn);
        let operand2 = self.v_read(idxdsize, vm);
        let index_vector = self
            .ir
            .ir()
            .vector_broadcast_element(esize, operand2, index as u8);
        let result = self.ir.ir().vector_signed_saturated_doubling_multiply_high(
            esize,
            operand1,
            index_vector,
        );

        self.v_write(datasize, vd, result);
        true
    }

    pub fn sqrdmulh_elt_2(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
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
        let (index, vm) = if size == 0b01 {
            ((h << 2) | (l << 1) | m, Vec::from_u32(vmlo))
        } else {
            ((h << 1) | l, Vec::from_u32((m << 4) | vmlo))
        };
        let idxdsize = if h == 1 { 128 } else { 64 };
        let esize = 8usize << size;
        let datasize = if q { 128 } else { 64 };

        let operand1 = self.v_read(datasize, vn);
        let operand2 = self.v_read(idxdsize, vm);
        let index_vector = self
            .ir
            .ir()
            .vector_broadcast_element(esize, operand2, index as u8);
        let result = self
            .ir
            .ir()
            .vector_signed_saturated_doubling_multiply_high_rounding(esize, operand1, index_vector);

        self.v_write(datasize, vd, result);
        true
    }
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
            crate::frontend::a64::translate::TranslationOptions::default(),
        );
        let should_continue = visitor.dispatch(&decoded);
        drop(visitor);
        (block, should_continue, decoded.name)
    }

    #[test]
    fn fmul_vector_by_element_encoding_translates_without_interpret_terminal() {
        let (block, should_continue, name) = translate_one(0x4F80_9051);
        assert_eq!(name, A64InstructionName::FMUL_elt_4);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::FPVectorMul32));
    }

    #[test]
    fn integer_multiply_by_element_family_uses_matching_ir_opcodes() {
        let cases = [
            (
                0x4FA0_8000,
                A64InstructionName::MUL_elt,
                Opcode::VectorMultiply32,
            ),
            (
                0x6FA0_0000,
                A64InstructionName::MLA_elt,
                Opcode::VectorAdd32,
            ),
            (
                0x6FA0_4000,
                A64InstructionName::MLS_elt,
                Opcode::VectorSub32,
            ),
        ];

        for (encoding, expected_name, expected_opcode) in cases {
            let (block, should_continue, name) = translate_one(encoding);
            assert_eq!(name, expected_name, "encoding 0x{encoding:08X}");
            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert!(
                block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == expected_opcode),
                "encoding 0x{encoding:08X} did not emit {expected_opcode:?}"
            );
            assert!(block
                .instructions
                .iter()
                .any(|inst| inst.opcode == Opcode::VectorMultiply32));
        }
    }

    #[test]
    fn remaining_vector_by_element_family_translates_without_interpret_terminal() {
        let cases = [
            (
                0x4F00_1000,
                A64InstructionName::FMLA_elt_3,
                Opcode::FPVectorMulAdd16,
            ),
            (
                0x4F00_5000,
                A64InstructionName::FMLS_elt_3,
                Opcode::FPVectorMulAdd16,
            ),
            (
                0x6F80_1000,
                A64InstructionName::FCMLA_elt,
                Opcode::FPMulAdd32,
            ),
            (
                0x4F60_B253,
                A64InstructionName::SQDMULL_elt_2,
                Opcode::VectorSignedSaturatedDoublingMultiplyLong16,
            ),
            (0x4FA0_E000, A64InstructionName::SDOT_elt, Opcode::Mul32),
            (0x6FA0_E000, A64InstructionName::UDOT_elt, Opcode::Mul32),
        ];

        for (encoding, expected_name, expected_opcode) in cases {
            let (block, should_continue, name) = translate_one(encoding);
            assert_eq!(name, expected_name, "encoding 0x{encoding:08X}");
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
    #[should_panic(expected = "half-precision floating point is unsupported")]
    fn fcmla_half_precision_asserts_like_upstream() {
        let _ = translate_one(0x6F40_1000);
    }

    #[test]
    fn fmul_vector_by_element_double_q0_is_reserved() {
        let (block, should_continue, name) = translate_one(0x0FC0_9051);
        assert_eq!(name, A64InstructionName::FMUL_elt_4);
        assert!(!should_continue);
        assert!(matches!(block.terminal, Terminal::CheckHalt { .. }));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A64ExceptionRaised));
    }

    #[test]
    fn fmls_vector_by_element_translates_without_interpret_terminal() {
        // FMLS Vd.4S, Vn.4S, Vm.S[0] — Q=1, sz=0 (esize=32). Subtract path negates op1.
        let (block, should_continue, name) = translate_one(0x4F82_5020);
        assert_eq!(name, A64InstructionName::FMLS_elt_4);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::FPVectorMulAdd32));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::FPVectorNeg32));
    }

    #[test]
    fn fmulx_vector_by_element_translates_without_interpret_terminal() {
        // FMULX Vd.4S, Vn.4S, Vm.S[0] — Q=1, sz=0 (esize=32).
        let (block, should_continue, name) = translate_one(0x6F82_9020);
        assert_eq!(name, A64InstructionName::FMULX_elt_4);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::FPVectorMulX32));
    }

    #[test]
    fn multiply_long_by_element_family_uses_matching_ir_opcodes() {
        let cases = [
            (0x4F60_2253, Opcode::VectorMultiplySignedWiden16),
            (0x4F60_6253, Opcode::VectorMultiplySignedWiden16),
            (0x4F60_A253, Opcode::VectorMultiplySignedWiden16),
            (0x6F60_2253, Opcode::VectorMultiplyUnsignedWiden16),
            (0x6F60_6253, Opcode::VectorMultiplyUnsignedWiden16),
            (0x6F60_A253, Opcode::VectorMultiplyUnsignedWiden16),
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
        }
    }

    #[test]
    fn sqrdmulh_vector_by_element_encoding_uses_rounding_ir() {
        let (block, should_continue, name) = translate_one(0x4F77_D021);
        assert_eq!(name, A64InstructionName::SQRDMULH_elt_2);
        assert!(should_continue);
        assert!(block.instructions.iter().any(|inst| {
            inst.opcode == Opcode::VectorSignedSaturatedDoublingMultiplyHighRounding16
        }));
    }
}
