//! Port of upstream
//! `dynarmic/frontend/A64/translate/impl/simd_across_lanes.cpp`.
//!
use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;
use crate::ir::value::Value;

#[derive(Clone, Copy)]
enum Signedness {
    Signed,
    Unsigned,
}

#[derive(Clone, Copy)]
enum MinMaxOperation {
    Max,
    MaxNumeric,
    Min,
    MinNumeric,
}

#[derive(Clone, Copy)]
enum ScalarMinMaxOperation {
    Max,
    Min,
}

impl<'a> TranslatorVisitor<'a> {
    fn emit_fp_min_max(&mut self, lhs: Value, rhs: Value, operation: MinMaxOperation) -> Value {
        match operation {
            MinMaxOperation::Max => self.ir.ir().fp_max(32, lhs, rhs),
            MinMaxOperation::MaxNumeric => self.ir.ir().fp_max_numeric(32, lhs, rhs),
            MinMaxOperation::Min => self.ir.ir().fp_min(32, lhs, rhs),
            MinMaxOperation::MinNumeric => self.ir.ir().fp_min_numeric(32, lhs, rhs),
        }
    }

    fn fp_min_max(&mut self, inst: &DecodedInst, operation: MinMaxOperation) -> bool {
        let q = inst.bit(30);
        let sz = inst.bit(22);
        if !q || sz {
            return self.reserved_value();
        }

        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let operand = self.v_read(128, vn);

        let mut hi = self.ir.ir().vector_get_element(32, operand, 2);
        let element3 = self.ir.ir().vector_get_element(32, operand, 3);
        hi = self.emit_fp_min_max(hi, element3, operation);

        let mut lo = self.ir.ir().vector_get_element(32, operand, 0);
        let element1 = self.ir.ir().vector_get_element(32, operand, 1);
        lo = self.emit_fp_min_max(lo, element1, operation);

        let result = self.emit_fp_min_max(lo, hi, operation);
        self.v_scalar_write(32, vd, result);
        true
    }

    pub fn fmaxnmv_2(&mut self, inst: &DecodedInst) -> bool {
        self.fp_min_max(inst, MinMaxOperation::MaxNumeric)
    }

    pub fn fmaxv_2(&mut self, inst: &DecodedInst) -> bool {
        self.fp_min_max(inst, MinMaxOperation::Max)
    }

    pub fn fminnmv_2(&mut self, inst: &DecodedInst) -> bool {
        self.fp_min_max(inst, MinMaxOperation::MinNumeric)
    }

    pub fn fminv_2(&mut self, inst: &DecodedInst) -> bool {
        self.fp_min_max(inst, MinMaxOperation::Min)
    }

    pub fn addv(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if (size == 0b10 && !q) || size == 0b11 {
            return self.reserved_value();
        }
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };
        let operand = self.v_read(datasize, vn);
        let result = self.ir.ir().vector_reduce_add(esize, operand);
        self.v_write(128, vd, result);
        true
    }

    /// SADDLV / UADDLV — Add across vector long. Sums all `esize` lanes of
    /// `Vn` after sign/zero extension to 64 bits, then stores the low
    /// `2 * esize` result bits into `Vd`.
    ///
    /// Mirrors upstream `LongAdd` in `simd_across_lanes.cpp`.
    fn long_add(&mut self, inst: &DecodedInst, signedness: Signedness) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if (size == 0b10 && !q) || size == 0b11 {
            return self.reserved_value();
        }
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };
        let elements = datasize / esize;

        let operand = self.v_read(datasize, vn);
        let zero_carry = self.ir.ir().imm1(false);
        let mut sum = self.read_and_extend_to_u64_signedness(esize, operand, 0, signedness);
        for i in 1..elements {
            let elem = self.read_and_extend_to_u64_signedness(esize, operand, i, signedness);
            sum = self.ir.ir().add_64(sum, elem, zero_carry);
        }

        let result = match esize {
            8 => {
                let lsh = self.ir.ir().least_significant_half(sum);
                let widened = self.ir.ir().zero_extend_half_to_long(lsh);
                self.ir.ir().zero_extend_to_quad(widened)
            }
            16 => {
                let lsw = self.ir.ir().least_significant_word(sum);
                let widened = self.ir.ir().zero_extend_word_to_long(lsw);
                self.ir.ir().zero_extend_to_quad(widened)
            }
            32 => self.ir.ir().zero_extend_to_quad(sum),
            _ => unreachable!("esize {} excluded by reserved-value check", esize),
        };
        self.v_write(datasize, vd, result);
        true
    }

    pub fn saddlv(&mut self, inst: &DecodedInst) -> bool {
        self.long_add(inst, Signedness::Signed)
    }

    pub fn uaddlv(&mut self, inst: &DecodedInst) -> bool {
        self.long_add(inst, Signedness::Unsigned)
    }

    fn scalar_min_max(
        &mut self,
        inst: &DecodedInst,
        operation: ScalarMinMaxOperation,
        signedness: Signedness,
    ) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if (size == 0b10 && !q) || size == 0b11 {
            return self.reserved_value();
        }

        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };
        let elements = datasize / esize;
        let operand = self.v_read(datasize, vn);

        let mut value = self.read_and_extend_to_u32_signedness(esize, operand, 0, signedness);
        for index in 1..elements {
            let element = self.read_and_extend_to_u32_signedness(esize, operand, index, signedness);
            value = match (operation, signedness) {
                (ScalarMinMaxOperation::Max, Signedness::Signed) => {
                    self.ir.ir().max_signed(32, value, element)
                }
                (ScalarMinMaxOperation::Max, Signedness::Unsigned) => {
                    self.ir.ir().max_unsigned(32, value, element)
                }
                (ScalarMinMaxOperation::Min, Signedness::Signed) => {
                    self.ir.ir().min_signed(32, value, element)
                }
                (ScalarMinMaxOperation::Min, Signedness::Unsigned) => {
                    self.ir.ir().min_unsigned(32, value, element)
                }
            };
        }

        let result = match esize {
            8 => {
                let byte = self.ir.ir().least_significant_byte(value);
                let word = self.ir.ir().zero_extend_byte_to_word(byte);
                self.ir.ir().zero_extend_to_quad(word)
            }
            16 => {
                let half = self.ir.ir().least_significant_half(value);
                let word = self.ir.ir().zero_extend_half_to_word(half);
                self.ir.ir().zero_extend_to_quad(word)
            }
            32 => self.ir.ir().zero_extend_to_quad(value),
            _ => unreachable!("esize {} excluded by reserved-value check", esize),
        };
        self.v_write(datasize, vd, result);
        true
    }

    pub fn smaxv(&mut self, inst: &DecodedInst) -> bool {
        self.scalar_min_max(inst, ScalarMinMaxOperation::Max, Signedness::Signed)
    }

    pub fn sminv(&mut self, inst: &DecodedInst) -> bool {
        self.scalar_min_max(inst, ScalarMinMaxOperation::Min, Signedness::Signed)
    }

    pub fn umaxv(&mut self, inst: &DecodedInst) -> bool {
        self.scalar_min_max(inst, ScalarMinMaxOperation::Max, Signedness::Unsigned)
    }

    pub fn uminv(&mut self, inst: &DecodedInst) -> bool {
        self.scalar_min_max(inst, ScalarMinMaxOperation::Min, Signedness::Unsigned)
    }

    fn read_and_extend_to_u64(
        &mut self,
        esize: usize,
        operand: crate::ir::value::Value,
        index: usize,
    ) -> crate::ir::value::Value {
        let elem = self.ir.ir().vector_get_element(esize, operand, index as u8);
        match esize {
            8 => self.ir.ir().zero_extend_byte_to_long(elem),
            16 => self.ir.ir().zero_extend_half_to_long(elem),
            32 => self.ir.ir().zero_extend_word_to_long(elem),
            _ => unreachable!("esize {} not supported by ADDV", esize),
        }
    }

    fn read_and_extend_to_u64_signedness(
        &mut self,
        esize: usize,
        operand: crate::ir::value::Value,
        index: usize,
        signedness: Signedness,
    ) -> crate::ir::value::Value {
        let elem = self.ir.ir().vector_get_element(esize, operand, index as u8);
        match (esize, signedness) {
            (8, Signedness::Signed) => self.ir.ir().sign_extend_byte_to_long(elem),
            (16, Signedness::Signed) => self.ir.ir().sign_extend_half_to_long(elem),
            (32, Signedness::Signed) => self.ir.ir().sign_extend_word_to_long(elem),
            (8, Signedness::Unsigned) => self.ir.ir().zero_extend_byte_to_long(elem),
            (16, Signedness::Unsigned) => self.ir.ir().zero_extend_half_to_long(elem),
            (32, Signedness::Unsigned) => self.ir.ir().zero_extend_word_to_long(elem),
            _ => unreachable!("esize {} not supported by SADDLV/UADDLV", esize),
        }
    }

    fn read_and_extend_to_u32_signedness(
        &mut self,
        esize: usize,
        operand: Value,
        index: usize,
        signedness: Signedness,
    ) -> Value {
        let element = self.ir.ir().vector_get_element(esize, operand, index as u8);
        match (esize, signedness) {
            (8, Signedness::Signed) => self.ir.ir().sign_extend_byte_to_word(element),
            (16, Signedness::Signed) => self.ir.ir().sign_extend_half_to_word(element),
            (32, Signedness::Signed | Signedness::Unsigned) => element,
            (8, Signedness::Unsigned) => self.ir.ir().zero_extend_byte_to_word(element),
            (16, Signedness::Unsigned) => self.ir.ir().zero_extend_half_to_word(element),
            _ => unreachable!("esize {} not supported by scalar min/max", esize),
        }
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

    #[test]
    fn min_max_across_lanes_family_matches_upstream_opcodes() {
        let cases = [
            (0x6E30_C801, Opcode::FPMaxNumeric32),
            (0x6E30_F822, Opcode::FPMax32),
            (0x6EB0_C801, Opcode::FPMinNumeric32),
            (0x6EB0_F864, Opcode::FPMin32),
            (0x4E30_A885, Opcode::MaxSigned32),
            (0x4E71_A8A6, Opcode::MinSigned32),
            (0x6E30_A8C7, Opcode::MaxUnsigned32),
            (0x6EB1_A8E8, Opcode::MinUnsigned32),
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

        let (block, _) = translate_one(0x6EB0_C801);
        assert_eq!(
            block
                .instructions
                .iter()
                .filter(|instruction| instruction.opcode == Opcode::FPMinNumeric32)
                .count(),
            3
        );
    }

    #[test]
    fn addv_uses_edens_dedicated_vector_reduce_add_opcodes() {
        let cases = [
            (0x0E31_B800, Opcode::VectorReduceAdd8),
            (0x0E71_B800, Opcode::VectorReduceAdd16),
            (0x4EB1_B800, Opcode::VectorReduceAdd32),
        ];

        for (encoding, expected_opcode) in cases {
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert_eq!(
                block
                    .instructions
                    .iter()
                    .filter(|instruction| instruction.opcode == expected_opcode)
                    .count(),
                1,
                "encoding 0x{encoding:08X} did not emit one {expected_opcode:?}"
            );
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }
}
