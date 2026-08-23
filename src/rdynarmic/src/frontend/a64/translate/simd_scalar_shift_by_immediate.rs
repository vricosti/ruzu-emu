//! Port of upstream
//! `dynarmic/frontend/A64/translate/impl/simd_scalar_shift_by_immediate.cpp`.

use crate::common::fp::fpcr::Fpcr;
use crate::common::fp::rounding_mode::RoundingMode;
use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;
use crate::ir::value::Value;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Narrowing {
    // Preserved from Eden's helper enum; the current scalar visitors use only saturating modes.
    #[allow(dead_code)]
    Truncation,
    SaturateToUnsigned,
    SaturateToSigned,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SaturatingShiftLeftType {
    Signed,
    Unsigned,
    SignedWithUnsignedSaturation,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ShiftExtraBehavior {
    None,
    Accumulate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SignednessSsbi {
    Signed,
    Unsigned,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FloatConversionDirection {
    FixedToFloat,
    FloatToFixed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ShiftDirection {
    Left,
    Right,
}

fn highest_set_bit(value: u32) -> usize {
    debug_assert_ne!(value, 0);
    (31 - value.leading_zeros()) as usize
}

fn saturating_shift_left(
    visitor: &mut TranslatorVisitor<'_>,
    immh: u32,
    immb: u32,
    vn: Vec,
    vd: Vec,
    shift_type: SaturatingShiftLeftType,
) -> bool {
    if immh == 0 {
        return visitor.reserved_value();
    }

    let esize = 8usize << highest_set_bit(immh);
    let shift_amount = ((immh << 3) | immb) as usize - esize;

    let operand = visitor.v_scalar_read(esize, vn);
    let operand = visitor.ir.ir().zero_extend_to_quad(operand);
    let shift = visitor.i(esize, shift_amount as u64);
    let shift = visitor.ir.ir().zero_extend_to_quad(shift);
    let result = match shift_type {
        SaturatingShiftLeftType::Signed => visitor
            .ir
            .ir()
            .vector_signed_saturated_shift_left(esize, operand, shift),
        SaturatingShiftLeftType::Unsigned => visitor
            .ir
            .ir()
            .vector_unsigned_saturated_shift_left(esize, operand, shift),
        SaturatingShiftLeftType::SignedWithUnsignedSaturation => visitor
            .ir
            .ir()
            .vector_signed_saturated_shift_left_unsigned(esize, operand, shift_amount as u8),
    };

    visitor.ir.set_q(vd, result);
    true
}

fn shift_right(
    visitor: &mut TranslatorVisitor<'_>,
    immh: u32,
    immb: u32,
    vn: Vec,
    vd: Vec,
    behavior: ShiftExtraBehavior,
    signedness: SignednessSsbi,
) -> bool {
    if immh & 0b1000 == 0 {
        return visitor.reserved_value();
    }

    let esize = 64usize;
    let shift_amount = (esize * 2 - ((immh << 3) | immb) as usize) as u8;
    let operand = visitor.v_scalar_read(esize, vn);
    let shift = visitor.ir.ir().imm8(shift_amount);
    let mut result = match signedness {
        SignednessSsbi::Signed => visitor.ir.ir().arithmetic_shift_right_64(operand, shift),
        SignednessSsbi::Unsigned => visitor.ir.ir().logical_shift_right_64(operand, shift),
    };

    if behavior == ShiftExtraBehavior::Accumulate {
        let addend = visitor.v_scalar_read(esize, vd);
        let carry = visitor.ir.ir().imm1(false);
        result = visitor.ir.ir().add_64(result, addend, carry);
    }

    visitor.v_scalar_write(esize, vd, result);
    true
}

fn rounding_shift_right(
    visitor: &mut TranslatorVisitor<'_>,
    immh: u32,
    immb: u32,
    vn: Vec,
    vd: Vec,
    behavior: ShiftExtraBehavior,
    signedness: SignednessSsbi,
) -> bool {
    if immh & 0b1000 == 0 {
        return visitor.reserved_value();
    }

    let esize = 64usize;
    let shift_amount = (esize * 2 - ((immh << 3) | immb) as usize) as u8;
    let operand = visitor.v_scalar_read(esize, vn);
    let left_shift = visitor.ir.ir().imm8(64 - shift_amount);
    let shifted_round_bit = visitor.ir.ir().logical_shift_left_64(operand, left_shift);
    let round_bit_shift = visitor.ir.ir().imm8(63);
    let round_bit = visitor
        .ir
        .ir()
        .logical_shift_right_64(shifted_round_bit, round_bit_shift);

    let shift = visitor.ir.ir().imm8(shift_amount);
    let shifted = match signedness {
        SignednessSsbi::Signed => visitor.ir.ir().arithmetic_shift_right_64(operand, shift),
        SignednessSsbi::Unsigned => visitor.ir.ir().logical_shift_right_64(operand, shift),
    };
    let carry = visitor.ir.ir().imm1(false);
    let mut result = visitor.ir.ir().add_64(shifted, round_bit, carry);

    if behavior == ShiftExtraBehavior::Accumulate {
        let addend = visitor.v_scalar_read(esize, vd);
        let carry = visitor.ir.ir().imm1(false);
        result = visitor.ir.ir().add_64(result, addend, carry);
    }

    visitor.v_scalar_write(esize, vd, result);
    true
}

fn shift_and_insert(
    visitor: &mut TranslatorVisitor<'_>,
    immh: u32,
    immb: u32,
    vn: Vec,
    vd: Vec,
    direction: ShiftDirection,
) -> bool {
    if immh & 0b1000 == 0 {
        return visitor.reserved_value();
    }

    let esize = 64usize;
    let imm7 = ((immh << 3) | immb) as usize;
    let shift_amount = match direction {
        ShiftDirection::Right => (esize * 2 - imm7) as u8,
        ShiftDirection::Left => (imm7 - esize) as u8,
    };
    let mask = match direction {
        ShiftDirection::Right if shift_amount as usize == esize => 0,
        ShiftDirection::Right => u64::MAX >> shift_amount,
        ShiftDirection::Left => u64::MAX << shift_amount,
    };

    let operand1 = visitor.v_scalar_read(esize, vn);
    let operand2 = visitor.v_scalar_read(esize, vd);
    let shift = visitor.ir.ir().imm8(shift_amount);
    let shifted = match direction {
        ShiftDirection::Right => visitor.ir.ir().logical_shift_right_64(operand1, shift),
        ShiftDirection::Left => visitor.ir.ir().logical_shift_left_64(operand1, shift),
    };
    let preserved = visitor.ir.ir().and_not_64(operand2, Value::ImmU64(mask));
    let result = visitor.ir.ir().or_64(preserved, shifted);

    visitor.v_scalar_write(esize, vd, result);
    true
}

fn shift_right_narrowing(
    visitor: &mut TranslatorVisitor<'_>,
    immh: u32,
    immb: u32,
    vn: Vec,
    vd: Vec,
    narrowing: Narrowing,
    signedness: SignednessSsbi,
) -> bool {
    if immh == 0 || immh & 0b1000 != 0 {
        return visitor.reserved_value();
    }

    let esize = 8usize << highest_set_bit(immh);
    let source_esize = 2 * esize;
    let shift_amount = (source_esize - ((immh << 3) | immb) as usize) as u8;

    let vector = visitor.v_read(128, vn);
    let operand = visitor.ir.ir().vector_get_element(source_esize, vector, 0);
    let operand = visitor.ir.ir().zero_extend_to_quad(operand);
    let wide_result = match signedness {
        SignednessSsbi::Signed => {
            visitor
                .ir
                .ir()
                .vector_arithmetic_shift_right(source_esize, operand, shift_amount)
        }
        SignednessSsbi::Unsigned => {
            visitor
                .ir
                .ir()
                .vector_logical_shift_right(source_esize, operand, shift_amount)
        }
    };

    let result = match narrowing {
        Narrowing::Truncation => visitor.ir.ir().vector_narrow(source_esize, wide_result),
        Narrowing::SaturateToUnsigned if signedness == SignednessSsbi::Signed => visitor
            .ir
            .ir()
            .vector_signed_saturated_narrow_to_unsigned(source_esize, wide_result),
        Narrowing::SaturateToUnsigned => visitor
            .ir
            .ir()
            .vector_unsigned_saturated_narrow(source_esize, wide_result),
        Narrowing::SaturateToSigned => {
            debug_assert_eq!(signedness, SignednessSsbi::Signed);
            visitor
                .ir
                .ir()
                .vector_signed_saturated_narrow_to_signed(source_esize, wide_result)
        }
    };

    let segment = visitor.ir.ir().vector_get_element(esize, result, 0);
    visitor.v_scalar_write(esize, vd, segment);
    true
}

fn scalar_fp_convert_with_round(
    visitor: &mut TranslatorVisitor<'_>,
    immh: u32,
    immb: u32,
    vn: Vec,
    vd: Vec,
    signedness: SignednessSsbi,
    direction: FloatConversionDirection,
    rounding_mode: u8,
) -> bool {
    if immh & 0b1110 == 0 {
        return visitor.reserved_value();
    }

    // FP16 is not implemented, matching Eden's architecturally permitted rejection.
    if immh & 0b1110 == 0b0010 {
        return visitor.reserved_value();
    }

    let esize = if immh & 0b1000 != 0 { 64 } else { 32 };
    let concat = ((immh << 3) | immb) as usize;
    let fbits = (esize * 2 - concat) as u8;
    let operand = visitor.v_scalar_read(esize, vn);

    let result = match (direction, esize, signedness) {
        (FloatConversionDirection::FloatToFixed, 64, SignednessSsbi::Signed) => visitor
            .ir
            .ir()
            .fp_to_fixed_s64(operand, 64, fbits, rounding_mode),
        (FloatConversionDirection::FloatToFixed, 64, SignednessSsbi::Unsigned) => visitor
            .ir
            .ir()
            .fp_to_fixed_u64(operand, 64, fbits, rounding_mode),
        (FloatConversionDirection::FloatToFixed, 32, SignednessSsbi::Signed) => visitor
            .ir
            .ir()
            .fp_to_fixed_s32(operand, 32, fbits, rounding_mode),
        (FloatConversionDirection::FloatToFixed, 32, SignednessSsbi::Unsigned) => visitor
            .ir
            .ir()
            .fp_to_fixed_u32(operand, 32, fbits, rounding_mode),
        (FloatConversionDirection::FixedToFloat, 64, signedness) => {
            visitor.ir.ir().fp_fixed_to_double(
                operand,
                64,
                signedness == SignednessSsbi::Signed,
                fbits,
                rounding_mode,
            )
        }
        (FloatConversionDirection::FixedToFloat, 32, signedness) => {
            visitor.ir.ir().fp_fixed_to_single(
                operand,
                32,
                signedness == SignednessSsbi::Signed,
                fbits,
                rounding_mode,
            )
        }
        _ => unreachable!(),
    };

    visitor.v_scalar_write(esize, vd, result);
    true
}

impl<'a> TranslatorVisitor<'a> {
    pub fn fcvtzs_fix_1(&mut self, inst: &DecodedInst) -> bool {
        scalar_fp_convert_with_round(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            SignednessSsbi::Signed,
            FloatConversionDirection::FloatToFixed,
            RoundingMode::TowardsZero as u8,
        )
    }

    pub fn fcvtzu_fix_1(&mut self, inst: &DecodedInst) -> bool {
        scalar_fp_convert_with_round(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            SignednessSsbi::Unsigned,
            FloatConversionDirection::FloatToFixed,
            RoundingMode::TowardsZero as u8,
        )
    }

    pub fn scvtf_fix_1(&mut self, inst: &DecodedInst) -> bool {
        let fpcr = self
            .ir
            .current_location
            .expect("current_location not set")
            .fpcr();
        scalar_fp_convert_with_round(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            SignednessSsbi::Signed,
            FloatConversionDirection::FixedToFloat,
            Fpcr::new(fpcr).rmode() as u8,
        )
    }

    pub fn ucvtf_fix_1(&mut self, inst: &DecodedInst) -> bool {
        let fpcr = self
            .ir
            .current_location
            .expect("current_location not set")
            .fpcr();
        scalar_fp_convert_with_round(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            SignednessSsbi::Unsigned,
            FloatConversionDirection::FixedToFloat,
            Fpcr::new(fpcr).rmode() as u8,
        )
    }

    pub fn sli_1(&mut self, inst: &DecodedInst) -> bool {
        shift_and_insert(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ShiftDirection::Left,
        )
    }

    pub fn sri_1(&mut self, inst: &DecodedInst) -> bool {
        shift_and_insert(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ShiftDirection::Right,
        )
    }

    pub fn sqshl_imm_1(&mut self, inst: &DecodedInst) -> bool {
        saturating_shift_left(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            SaturatingShiftLeftType::Signed,
        )
    }

    pub fn sqshlu_1(&mut self, inst: &DecodedInst) -> bool {
        saturating_shift_left(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            SaturatingShiftLeftType::SignedWithUnsignedSaturation,
        )
    }

    pub fn sqshrn_1(&mut self, inst: &DecodedInst) -> bool {
        shift_right_narrowing(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            Narrowing::SaturateToSigned,
            SignednessSsbi::Signed,
        )
    }

    pub fn sqshrun_1(&mut self, inst: &DecodedInst) -> bool {
        shift_right_narrowing(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            Narrowing::SaturateToUnsigned,
            SignednessSsbi::Signed,
        )
    }

    pub fn srshr_1(&mut self, inst: &DecodedInst) -> bool {
        rounding_shift_right(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ShiftExtraBehavior::None,
            SignednessSsbi::Signed,
        )
    }

    pub fn srsra_1(&mut self, inst: &DecodedInst) -> bool {
        rounding_shift_right(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ShiftExtraBehavior::Accumulate,
            SignednessSsbi::Signed,
        )
    }

    pub fn sshr_1(&mut self, inst: &DecodedInst) -> bool {
        shift_right(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ShiftExtraBehavior::None,
            SignednessSsbi::Signed,
        )
    }

    pub fn ssra_1(&mut self, inst: &DecodedInst) -> bool {
        shift_right(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ShiftExtraBehavior::Accumulate,
            SignednessSsbi::Signed,
        )
    }

    pub fn shl_1(&mut self, inst: &DecodedInst) -> bool {
        let immh = inst.bits(22, 19);
        let immb = inst.bits(18, 16);
        if immh & 0b1000 == 0 {
            return self.reserved_value();
        }

        let esize = 64usize;
        let shift_amount = (((immh << 3) | immb) as usize - esize) as u8;
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let operand = self.v_scalar_read(esize, vn);
        let shift = self.ir.ir().imm8(shift_amount);
        let result = self.ir.ir().logical_shift_left_64(operand, shift);
        self.v_scalar_write(esize, vd, result);
        true
    }

    pub fn uqshl_imm_1(&mut self, inst: &DecodedInst) -> bool {
        saturating_shift_left(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            SaturatingShiftLeftType::Unsigned,
        )
    }

    pub fn uqshrn_1(&mut self, inst: &DecodedInst) -> bool {
        shift_right_narrowing(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            Narrowing::SaturateToUnsigned,
            SignednessSsbi::Unsigned,
        )
    }

    pub fn urshr_1(&mut self, inst: &DecodedInst) -> bool {
        rounding_shift_right(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ShiftExtraBehavior::None,
            SignednessSsbi::Unsigned,
        )
    }

    pub fn ursra_1(&mut self, inst: &DecodedInst) -> bool {
        rounding_shift_right(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ShiftExtraBehavior::Accumulate,
            SignednessSsbi::Unsigned,
        )
    }

    pub fn ushr_1(&mut self, inst: &DecodedInst) -> bool {
        shift_right(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ShiftExtraBehavior::None,
            SignednessSsbi::Unsigned,
        )
    }

    pub fn usra_1(&mut self, inst: &DecodedInst) -> bool {
        shift_right(
            self,
            inst.bits(22, 19),
            inst.bits(18, 16),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ShiftExtraBehavior::Accumulate,
            SignednessSsbi::Unsigned,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a64::decoder::{A64InstructionName, decode};
    use crate::frontend::a64::translate::TranslationOptions;
    use crate::ir::block::Block;
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::terminal::Terminal;

    fn encoding(unsigned: bool, immh: u32, immb: u32, opcode: u32) -> u32 {
        (if unsigned { 0x7f00_0000 } else { 0x5f00_0000 })
            | (immh << 19)
            | (immb << 16)
            | (opcode << 10)
            | (2 << 5)
            | 3
    }

    fn translate_one(raw: u32) -> (A64InstructionName, Block, bool) {
        let decoded = decode(raw).expect("scalar shift instruction must decode");
        let location = A64LocationDescriptor::new(0x1000, 0, false);
        let mut block = Block::new(location.to_location());
        let mut visitor =
            TranslatorVisitor::new(&mut block, location, TranslationOptions::default());
        let should_continue = visitor.dispatch(&decoded);
        drop(visitor);
        (decoded.name, block, should_continue)
    }

    #[test]
    fn all_scalar_shift_identities_dispatch_to_their_upstream_owner() {
        let cases = [
            (encoding(false, 8, 1, 1), A64InstructionName::SSHR_1),
            (encoding(false, 8, 1, 5), A64InstructionName::SSRA_1),
            (encoding(false, 8, 1, 9), A64InstructionName::SRSHR_1),
            (encoding(false, 8, 1, 13), A64InstructionName::SRSRA_1),
            (encoding(false, 8, 1, 21), A64InstructionName::SHL_1),
            (encoding(false, 4, 1, 29), A64InstructionName::SQSHL_imm_1),
            (encoding(false, 4, 1, 37), A64InstructionName::SQSHRN_1),
            (encoding(false, 8, 1, 57), A64InstructionName::SCVTF_fix_1),
            (encoding(false, 8, 1, 63), A64InstructionName::FCVTZS_fix_1),
            (encoding(true, 8, 1, 1), A64InstructionName::USHR_1),
            (encoding(true, 8, 1, 5), A64InstructionName::USRA_1),
            (encoding(true, 8, 1, 9), A64InstructionName::URSHR_1),
            (encoding(true, 8, 1, 13), A64InstructionName::URSRA_1),
            (encoding(true, 8, 1, 17), A64InstructionName::SRI_1),
            (encoding(true, 8, 1, 21), A64InstructionName::SLI_1),
            (encoding(true, 4, 1, 25), A64InstructionName::SQSHLU_1),
            (encoding(true, 4, 1, 29), A64InstructionName::UQSHL_imm_1),
            (encoding(true, 4, 1, 33), A64InstructionName::SQSHRUN_1),
            (encoding(true, 4, 1, 37), A64InstructionName::UQSHRN_1),
            (encoding(true, 8, 1, 57), A64InstructionName::UCVTF_fix_1),
            (encoding(true, 8, 1, 63), A64InstructionName::FCVTZU_fix_1),
        ];

        for (raw, expected_name) in cases {
            let (name, block, should_continue) = translate_one(raw);
            assert_eq!(name, expected_name, "encoding 0x{raw:08x}");
            assert!(should_continue, "encoding 0x{raw:08x}");
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }

    #[test]
    fn saturating_shift_families_emit_matching_upstream_opcodes() {
        let cases = [
            (
                encoding(false, 4, 1, 29),
                Opcode::VectorSignedSaturatedShiftLeft32,
            ),
            (
                encoding(true, 4, 1, 25),
                Opcode::VectorSignedSaturatedShiftLeftUnsigned32,
            ),
            (
                encoding(true, 4, 1, 29),
                Opcode::VectorUnsignedSaturatedShiftLeft32,
            ),
            (
                encoding(false, 4, 1, 37),
                Opcode::VectorSignedSaturatedNarrowToSigned64,
            ),
            (
                encoding(true, 4, 1, 33),
                Opcode::VectorSignedSaturatedNarrowToUnsigned64,
            ),
            (
                encoding(true, 4, 1, 37),
                Opcode::VectorUnsignedSaturatedNarrow64,
            ),
        ];

        for (raw, expected_opcode) in cases {
            let (_, block, should_continue) = translate_one(raw);
            assert!(should_continue);
            assert!(
                block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == expected_opcode),
                "encoding 0x{raw:08x} did not emit {expected_opcode:?}"
            );
        }
    }

    #[test]
    fn rounding_and_accumulation_preserve_upstream_operation_counts() {
        for (raw, expected_adds) in [
            (encoding(false, 8, 1, 9), 1),
            (encoding(false, 8, 1, 13), 2),
            (encoding(true, 8, 1, 9), 1),
            (encoding(true, 8, 1, 13), 2),
        ] {
            let (_, block, _) = translate_one(raw);
            assert_eq!(
                block
                    .instructions
                    .iter()
                    .filter(|inst| inst.opcode == Opcode::Add64)
                    .count(),
                expected_adds
            );
        }
    }

    #[test]
    fn scalar_fp_conversion_reads_the_source_element_once() {
        for raw in [encoding(false, 8, 1, 63), encoding(true, 8, 1, 57)] {
            let (_, block, should_continue) = translate_one(raw);
            assert!(should_continue);
            assert_eq!(
                block
                    .instructions
                    .iter()
                    .filter(|inst| inst.opcode == Opcode::VectorGetElement64)
                    .count(),
                1
            );
        }
    }

    #[test]
    fn invalid_scalar_right_shift_is_reserved_not_interpreted() {
        let (_, block, should_continue) = translate_one(encoding(false, 7, 0, 1));
        assert!(!should_continue);
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        assert!(
            block
                .instructions
                .iter()
                .any(|inst| inst.opcode == Opcode::A64ExceptionRaised)
        );
    }
}
