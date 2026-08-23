//! Port of upstream `dynarmic/frontend/A64/translate/impl/simd_scalar_two_register_misc.cpp`.

use crate::common::fp::fpcr::Fpcr;
use crate::common::fp::rounding_mode::RoundingMode;
use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;
use crate::ir::emitter::IREmitter;
use crate::ir::value::Value;

#[derive(Copy, Clone)]
enum ComparisonTypeSstrm {
    Eq,
    Ge,
    Gt,
    Le,
    Lt,
}

#[derive(Copy, Clone)]
enum SignednessSstrm {
    Signed,
    Unsigned,
}

fn scalar_fp_compare_against_zero(
    visitor: &mut TranslatorVisitor<'_>,
    sz: bool,
    vn: Vec,
    vd: Vec,
    comparison_type: ComparisonTypeSstrm,
) -> bool {
    let esize = if sz { 64 } else { 32 };
    let datasize = esize;

    let operand = visitor.v_read(datasize, vn);
    let zero = visitor.ir.ir().zero_vector();
    let result = match comparison_type {
        ComparisonTypeSstrm::Eq => visitor.ir.ir().fp_vector_equal(esize, operand, zero, true),
        ComparisonTypeSstrm::Ge => visitor
            .ir
            .ir()
            .fp_vector_greater_equal(esize, operand, zero, true),
        ComparisonTypeSstrm::Gt => visitor
            .ir
            .ir()
            .fp_vector_greater(esize, operand, zero, true),
        ComparisonTypeSstrm::Le => visitor
            .ir
            .ir()
            .fp_vector_greater_equal(esize, zero, operand, true),
        ComparisonTypeSstrm::Lt => visitor
            .ir
            .ir()
            .fp_vector_greater(esize, zero, operand, true),
    };

    let result = visitor.ir.ir().vector_get_element(esize, result, 0);
    visitor.v_scalar_write(datasize, vd, result);
    true
}

fn scalar_fp_convert_with_round(
    visitor: &mut TranslatorVisitor<'_>,
    sz: bool,
    vn: Vec,
    vd: Vec,
    rmode: RoundingMode,
    sign: SignednessSstrm,
) -> bool {
    let esize = if sz { 64 } else { 32 };

    let operand = visitor.v_scalar_read(esize, vn);
    let result = match (sz, sign) {
        (true, SignednessSstrm::Signed) => {
            visitor.ir.ir().fp_to_fixed_s64(operand, 64, 0, rmode as u8)
        }
        (true, SignednessSstrm::Unsigned) => {
            visitor.ir.ir().fp_to_fixed_u64(operand, 64, 0, rmode as u8)
        }
        (false, SignednessSstrm::Signed) => {
            visitor.ir.ir().fp_to_fixed_s32(operand, 32, 0, rmode as u8)
        }
        (false, SignednessSstrm::Unsigned) => {
            visitor.ir.ir().fp_to_fixed_u32(operand, 32, 0, rmode as u8)
        }
    };

    visitor.v_scalar_write(esize, vd, result);
    true
}

fn saturated_narrow<'a, F>(
    visitor: &mut TranslatorVisitor<'a>,
    size: u32,
    vn: Vec,
    vd: Vec,
    narrowing_fn: F,
) -> bool
where
    F: FnOnce(&mut IREmitter<'a>, usize, Value) -> Value,
{
    if size == 0b11 {
        return visitor.reserved_value();
    }

    let esize = 8usize << size;
    let operand = visitor.v_scalar_read(2 * esize, vn);
    let operand = visitor.ir.ir().zero_extend_to_quad(operand);
    let result = narrowing_fn(visitor.ir.ir(), 2 * esize, operand);

    let result = visitor.ir.ir().vector_get_element(64, result, 0);
    visitor.v_scalar_write(64, vd, result);
    true
}

impl<'a> TranslatorVisitor<'a> {
    /// ABS (scalar). `01011110zz100000101110nnnnnddddd`. Only size==0b11 is valid.
    pub fn abs_1(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.bits(23, 22);
        if size != 0b11 {
            return self.reserved_value();
        }
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let operand1 = self.v_scalar_read(64, vn);
        let shift = self.ir.ir().imm8(63);
        let operand2 = self.ir.ir().arithmetic_shift_right_64(operand1, shift);
        let xored = self.ir.ir().eor_64(operand1, operand2);
        let one = self.ir.ir().imm1(true);
        let result = self.ir.ir().sub_64(xored, operand2, one);
        self.v_scalar_write(64, vd, result);
        true
    }

    /// NEG (scalar). `01111110zz100000101110nnnnnddddd`. size==0b11 only.
    pub fn neg_1(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.bits(23, 22);
        if size != 0b11 {
            return self.reserved_value();
        }
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let operand = self.v_scalar_read(64, vn);
        let zero = self.ir.ir().imm64(0);
        let one = self.ir.ir().imm1(true);
        let result = self.ir.ir().sub_64(zero, operand, one);
        self.v_scalar_write(64, vd, result);
        true
    }

    pub fn sqxtn_1(&mut self, inst: &DecodedInst) -> bool {
        saturated_narrow(
            self,
            inst.bits(23, 22),
            Vec::from_u32(inst.bits(9, 5)),
            Vec::from_u32(inst.rd()),
            IREmitter::vector_signed_saturated_narrow_to_signed,
        )
    }

    pub fn sqxtun_1(&mut self, inst: &DecodedInst) -> bool {
        saturated_narrow(
            self,
            inst.bits(23, 22),
            Vec::from_u32(inst.bits(9, 5)),
            Vec::from_u32(inst.rd()),
            IREmitter::vector_signed_saturated_narrow_to_unsigned,
        )
    }

    pub fn uqxtn_1(&mut self, inst: &DecodedInst) -> bool {
        saturated_narrow(
            self,
            inst.bits(23, 22),
            Vec::from_u32(inst.bits(9, 5)),
            Vec::from_u32(inst.rd()),
            IREmitter::vector_unsigned_saturated_narrow,
        )
    }

    /// FCMEQ (zero, scalar, half-precision).
    /// `0101111011111000110110nnnnnddddd` — esize=16.
    pub fn fcmeq_zero_1(&mut self, inst: &DecodedInst) -> bool {
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let elem = self.v_scalar_read(16, vn);
        let operand = self.ir.ir().zero_extend_to_quad(elem);
        let zero = self.ir.ir().zero_vector();
        let result = self.ir.ir().fp_vector_equal(16, operand, zero, true);
        let r0 = self.ir.ir().vector_get_element(16, result, 0);
        self.v_scalar_write(16, vd, r0);
        true
    }

    /// FCMEQ (zero, scalar, single/double).
    /// `010111101z100000110110nnnnnddddd` — sz at bit 22.
    pub fn fcmeq_zero_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        scalar_fp_compare_against_zero(self, sz, vn, vd, ComparisonTypeSstrm::Eq)
    }

    /// FCMGE (zero, scalar). `011111101z100000110010nnnnnddddd`. sz at bit 22.
    pub fn fcmge_zero_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        scalar_fp_compare_against_zero(self, sz, vn, vd, ComparisonTypeSstrm::Ge)
    }

    /// FCMGT (zero, scalar). `010111101z100000110010nnnnnddddd`. sz at bit 22.
    pub fn fcmgt_zero_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        scalar_fp_compare_against_zero(self, sz, vn, vd, ComparisonTypeSstrm::Gt)
    }

    pub fn fcmle_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        scalar_fp_compare_against_zero(self, sz, vn, vd, ComparisonTypeSstrm::Le)
    }

    pub fn fcmlt_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        scalar_fp_compare_against_zero(self, sz, vn, vd, ComparisonTypeSstrm::Lt)
    }

    /// FCVTAS (vector, scalar). `010111100z100001110010nnnnnddddd`.
    pub fn fcvtas_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        scalar_fp_convert_with_round(
            self,
            sz,
            vn,
            vd,
            RoundingMode::ToNearestTieAwayFromZero,
            SignednessSstrm::Signed,
        )
    }

    /// FCVTAU (vector, scalar). `011111100z100001110010nnnnnddddd`.
    pub fn fcvtau_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        scalar_fp_convert_with_round(
            self,
            sz,
            vn,
            vd,
            RoundingMode::ToNearestTieAwayFromZero,
            SignednessSstrm::Unsigned,
        )
    }

    /// FCVTMS (vector, scalar). `010111100z100001101110nnnnnddddd`.
    pub fn fcvtms_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        scalar_fp_convert_with_round(
            self,
            sz,
            vn,
            vd,
            RoundingMode::TowardsMinusInfinity,
            SignednessSstrm::Signed,
        )
    }

    /// FCVTMU (vector, scalar). `011111100z100001101110nnnnnddddd`.
    pub fn fcvtmu_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        scalar_fp_convert_with_round(
            self,
            sz,
            vn,
            vd,
            RoundingMode::TowardsMinusInfinity,
            SignednessSstrm::Unsigned,
        )
    }

    /// FCVTNS (vector, scalar). `010111100z100001101010nnnnnddddd`.
    pub fn fcvtns_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        scalar_fp_convert_with_round(
            self,
            sz,
            vn,
            vd,
            RoundingMode::ToNearestTieEven,
            SignednessSstrm::Signed,
        )
    }

    /// FCVTNU (vector, scalar). `011111100z100001101010nnnnnddddd`.
    pub fn fcvtnu_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        scalar_fp_convert_with_round(
            self,
            sz,
            vn,
            vd,
            RoundingMode::ToNearestTieEven,
            SignednessSstrm::Unsigned,
        )
    }

    /// FCVTPS (vector, scalar). `010111101z100001101010nnnnnddddd`.
    pub fn fcvtps_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        scalar_fp_convert_with_round(
            self,
            sz,
            vn,
            vd,
            RoundingMode::TowardsPlusInfinity,
            SignednessSstrm::Signed,
        )
    }

    /// FCVTPU (vector, scalar). `011111101z100001101010nnnnnddddd`.
    pub fn fcvtpu_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        scalar_fp_convert_with_round(
            self,
            sz,
            vn,
            vd,
            RoundingMode::TowardsPlusInfinity,
            SignednessSstrm::Unsigned,
        )
    }

    pub fn fcvtxn_1(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        if !sz {
            return self.reserved_value();
        }

        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let element = self.v_scalar_read(64, vn);
        let result = self
            .ir
            .ir()
            .fp_double_to_single(element, RoundingMode::ToOdd as u8);

        self.v_scalar_write(32, vd, result);
        true
    }

    /// FCVTZS (vector, integer, scalar). `010111101z100001101110nnnnnddddd`.
    pub fn fcvtzs_int_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        scalar_fp_convert_with_round(
            self,
            sz,
            vn,
            vd,
            RoundingMode::TowardsZero,
            SignednessSstrm::Signed,
        )
    }

    /// FCVTZU (vector, integer, scalar). `011111101z100001101110nnnnnddddd`.
    pub fn fcvtzu_int_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        scalar_fp_convert_with_round(
            self,
            sz,
            vn,
            vd,
            RoundingMode::TowardsZero,
            SignednessSstrm::Unsigned,
        )
    }

    /// FRECPE (scalar, half-precision). `0101111011111001110110nnnnnddddd`.
    pub fn frecpe_1(&mut self, inst: &DecodedInst) -> bool {
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let operand = self.v_scalar_read(16, vn);
        let result = self.ir.ir().fp_recip_estimate(16, operand);
        self.v_scalar_write(16, vd, result);
        true
    }

    /// FRECPE (scalar, single/double). `010111101z100001110110nnnnnddddd`.
    pub fn frecpe_2(&mut self, inst: &DecodedInst) -> bool {
        let esize = if inst.bit(22) { 64 } else { 32 };
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let operand = self.v_scalar_read(esize, vn);
        let result = self.ir.ir().fp_recip_estimate(esize, operand);
        self.v_scalar_write(esize, vd, result);
        true
    }

    /// FRECPX (scalar, half-precision). `0101111011111001111110nnnnnddddd`.
    pub fn frecpx_1(&mut self, inst: &DecodedInst) -> bool {
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let operand = self.v_scalar_read(16, vn);
        let result = self.ir.ir().fp_recip_exponent(16, operand);
        self.v_scalar_write(16, vd, result);
        true
    }

    /// FRECPX (scalar, single/double). `010111101z100001111110nnnnnddddd`.
    pub fn frecpx_2(&mut self, inst: &DecodedInst) -> bool {
        let esize = if inst.bit(22) { 64 } else { 32 };
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let operand = self.v_scalar_read(esize, vn);
        let result = self.ir.ir().fp_recip_exponent(esize, operand);
        self.v_scalar_write(esize, vd, result);
        true
    }

    /// FRSQRTE (scalar, half-precision). `0111111011111001110110nnnnnddddd`.
    pub fn frsqrte_1(&mut self, inst: &DecodedInst) -> bool {
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let operand = self.v_scalar_read(16, vn);
        let result = self.ir.ir().fp_rsqrt_estimate(16, operand);
        self.v_scalar_write(16, vd, result);
        true
    }

    /// FRSQRTE (scalar, single/double). `011111101z100001110110nnnnnddddd`.
    pub fn frsqrte_2(&mut self, inst: &DecodedInst) -> bool {
        let esize = if inst.bit(22) { 64 } else { 32 };
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let operand = self.v_scalar_read(esize, vn);
        let result = self.ir.ir().fp_rsqrt_estimate(esize, operand);
        self.v_scalar_write(esize, vd, result);
        true
    }

    /// SCVTF (vector, integer, scalar). `010111100z100001110110nnnnnddddd`.
    pub fn scvtf_int_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let esize = if sz { 64 } else { 32 };
        let rmode = Fpcr::new(
            self.ir
                .current_location
                .expect("current_location not set")
                .fpcr(),
        )
        .rmode() as u8;

        let element = self.v_scalar_read(esize, vn);
        let result = if esize == 32 {
            self.ir.ir().fp_fixed_to_single(element, 32, true, 0, rmode)
        } else {
            self.ir.ir().fp_fixed_to_double(element, 64, true, 0, rmode)
        };

        self.v_scalar_write(esize, vd, result);
        true
    }

    pub fn sqabs_1(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.bits(23, 22);
        let esize = 8usize << size;
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());

        let operand = self.v_read(128, vn);
        let operand = self.ir.ir().vector_get_element(esize, operand, 0);
        let operand = self.ir.ir().zero_extend_to_quad(operand);
        let result = self.ir.ir().vector_signed_saturated_abs(esize, operand);

        self.v_write(128, vd, result);
        true
    }

    pub fn sqneg_1(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.bits(23, 22);
        let esize = 8usize << size;
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());

        let operand = self.v_read(128, vn);
        let operand = self.ir.ir().vector_get_element(esize, operand, 0);
        let operand = self.ir.ir().zero_extend_to_quad(operand);
        let result = self.ir.ir().vector_signed_saturated_neg(esize, operand);

        self.v_write(128, vd, result);
        true
    }

    pub fn suqadd_1(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.bits(23, 22);
        let esize = 8usize << size;
        let datasize = 64;
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());

        let operand1 = self.v_read(datasize, vn);
        let operand1 = self.ir.ir().vector_get_element(esize, operand1, 0);
        let operand1 = self.ir.ir().zero_extend_to_quad(operand1);
        let operand2 = self.v_read(datasize, vd);
        let operand2 = self.ir.ir().vector_get_element(esize, operand2, 0);
        let operand2 = self.ir.ir().zero_extend_to_quad(operand2);
        let result = self
            .ir
            .ir()
            .vector_signed_saturated_accumulate_unsigned(esize, operand1, operand2);

        self.v_write(datasize, vd, result);
        true
    }

    /// UCVTF (vector, integer, scalar). `011111100z100001110110nnnnnddddd`.
    pub fn ucvtf_int_2(&mut self, inst: &DecodedInst) -> bool {
        let sz = inst.bit(22);
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let esize = if sz { 64 } else { 32 };
        let rmode = Fpcr::new(
            self.ir
                .current_location
                .expect("current_location not set")
                .fpcr(),
        )
        .rmode() as u8;

        let element = self.v_scalar_read(esize, vn);
        let result = if esize == 32 {
            self.ir
                .ir()
                .fp_fixed_to_single(element, 32, false, 0, rmode)
        } else {
            self.ir
                .ir()
                .fp_fixed_to_double(element, 64, false, 0, rmode)
        };

        self.v_scalar_write(esize, vd, result);
        true
    }

    pub fn usqadd_1(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.bits(23, 22);
        let esize = 8usize << size;
        let datasize = 64;
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());

        let operand1 = self.v_read(datasize, vn);
        let operand1 = self.ir.ir().vector_get_element(esize, operand1, 0);
        let operand1 = self.ir.ir().zero_extend_to_quad(operand1);
        let operand2 = self.v_read(datasize, vd);
        let operand2 = self.ir.ir().vector_get_element(esize, operand2, 0);
        let operand2 = self.ir.ir().zero_extend_to_quad(operand2);
        let result = self
            .ir
            .ir()
            .vector_unsigned_saturated_accumulate_signed(esize, operand1, operand2);

        self.v_write(datasize, vd, result);
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

    fn opcode_count(block: &Block, opcode: Opcode) -> usize {
        block
            .instructions
            .iter()
            .filter(|inst| inst.opcode == opcode)
            .count()
    }

    #[test]
    fn saturated_narrow_1_family_uses_matching_ir_opcodes() {
        let cases = [
            (0x5E21_49F0, Opcode::VectorSignedSaturatedNarrowToSigned16),
            (0x7E21_29F0, Opcode::VectorSignedSaturatedNarrowToUnsigned16),
            (0x7E21_49F0, Opcode::VectorUnsignedSaturatedNarrow16),
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
    fn observed_fcvtns_scalar_encoding_translates_instead_of_interpreting() {
        let (block, should_continue) = translate_one(0x5E21_A800);

        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::FPSingleToFixedS32));
    }

    #[test]
    fn scalar_fcmeq_zero_single_and_double_match_upstream() {
        let cases = [
            (0x5EA0_D800, Opcode::FPVectorEqual32),
            (0x5EE0_D800, Opcode::FPVectorEqual64),
        ];

        for (encoding, expected_opcode) in cases {
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert!(block
                .instructions
                .iter()
                .any(|inst| inst.opcode == expected_opcode));
        }
    }

    #[test]
    fn scalar_fp_to_integer_rounding_family_uses_matching_ir_opcodes() {
        let cases = [
            (0x5E21_A800, Opcode::FPSingleToFixedS32), // FCVTNS S0, S0
            (0x5E21_B800, Opcode::FPSingleToFixedS32), // FCVTMS S0, S0
            (0x5E21_C800, Opcode::FPSingleToFixedS32), // FCVTAS S0, S0
            (0x5EA1_A800, Opcode::FPSingleToFixedS32), // FCVTPS S0, S0
            (0x7E21_A800, Opcode::FPSingleToFixedU32), // FCVTNU S0, S0
            (0x7E21_B800, Opcode::FPSingleToFixedU32), // FCVTMU S0, S0
            (0x7E21_C800, Opcode::FPSingleToFixedU32), // FCVTAU S0, S0
            (0x7EA1_A800, Opcode::FPSingleToFixedU32), // FCVTPU S0, S0
            (0x5E61_A800, Opcode::FPDoubleToFixedS64), // FCVTNS D0, D0
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
    fn observed_frsqrte_scalar_encoding_translates_instead_of_interpreting() {
        let (block, should_continue) = translate_one(0x7EA1_DA11);

        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::FPRSqrtEstimate32));
    }

    #[test]
    fn scalar_fp_estimate_family_uses_matching_ir_opcodes() {
        let cases = [
            (0x5EF9_D800, Opcode::FPRecipEstimate16),
            (0x5EA1_D800, Opcode::FPRecipEstimate32),
            (0x5EE1_D800, Opcode::FPRecipEstimate64),
            (0x5EF9_F800, Opcode::FPRecipExponent16),
            (0x5EA1_F800, Opcode::FPRecipExponent32),
            (0x5EE1_F800, Opcode::FPRecipExponent64),
            (0x7EF9_D800, Opcode::FPRSqrtEstimate16),
            (0x7EA1_D800, Opcode::FPRSqrtEstimate32),
            (0x7EE1_D800, Opcode::FPRSqrtEstimate64),
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
    fn newly_ported_scalar_two_register_misc_visitors_dispatch() {
        let cases = [
            (0x7EA0_D800, Opcode::FPVectorGreaterEqual32), // FCMLE S0, #0.0
            (0x5EA0_E800, Opcode::FPVectorGreater32),      // FCMLT S0, #0.0
            (0x7E61_6800, Opcode::FPDoubleToSingle),       // FCVTXN S0, D0
            (0x5E20_7800, Opcode::VectorSignedSaturatedAbs8),
            (0x7E20_7800, Opcode::VectorSignedSaturatedNeg8),
            (
                0x5E20_3800,
                Opcode::VectorSignedSaturatedAccumulateUnsigned8,
            ),
            (
                0x7E20_3800,
                Opcode::VectorUnsignedSaturatedAccumulateSigned8,
            ),
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
    fn scalar_fp_zero_compare_reads_vector_once_then_extracts_result() {
        let (block, should_continue) = translate_one(0x7EA0_D800);

        assert!(should_continue);
        assert_eq!(opcode_count(&block, Opcode::A64GetS), 1);
        assert_eq!(opcode_count(&block, Opcode::A64GetQ), 0);
        assert_eq!(opcode_count(&block, Opcode::VectorGetElement32), 1);
    }

    #[test]
    fn scalar_abs_and_neg_extract_the_source_once() {
        for encoding in [0x5EE0_B800, 0x7EE0_B800] {
            let (block, should_continue) = translate_one(encoding);

            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert_eq!(opcode_count(&block, Opcode::A64GetQ), 1);
            assert_eq!(opcode_count(&block, Opcode::VectorGetElement64), 1);
        }
    }

    #[test]
    fn fcvtxn_uses_to_odd_and_rejects_the_reserved_sz_value() {
        let (block, should_continue) = translate_one(0x7E61_6800);
        assert!(should_continue);
        let convert = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::FPDoubleToSingle)
            .expect("FCVTXN must emit FPDoubleToSingle");
        assert_eq!(convert.arg(1), Value::ImmU8(RoundingMode::ToOdd as u8));

        let (reserved, should_continue) = translate_one(0x7E21_6800);
        assert!(!should_continue);
        assert!(reserved
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A64ExceptionRaised));
    }

    #[test]
    fn scalar_saturating_accumulate_reads_both_64_bit_vectors() {
        for encoding in [0x5E20_3800, 0x7E20_3800] {
            let (block, should_continue) = translate_one(encoding);

            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert_eq!(opcode_count(&block, Opcode::A64GetD), 2);
            assert_eq!(opcode_count(&block, Opcode::A64GetQ), 0);
            assert_eq!(opcode_count(&block, Opcode::VectorGetElement8), 2);
        }
    }
}
