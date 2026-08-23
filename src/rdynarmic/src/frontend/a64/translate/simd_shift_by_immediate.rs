//! Port of upstream
//! `dynarmic/frontend/A64/translate/impl/simd_shift_by_immediate.cpp`.
//!
//! Vector shift-by-immediate. The 7-bit immediate `immh:immb` encodes
//! both the element size (highest set bit of `immh`) and the shift
//! amount (`immh:immb - esize` for left, `2*esize - immh:immb` for
//! right).

use crate::common::fp::fpcr::Fpcr;
use crate::common::fp::rounding_mode::RoundingMode;
use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;
use crate::ir::value::Value;

#[derive(Clone, Copy)]
enum Rounding {
    None,
    Round,
}

#[derive(Clone, Copy)]
enum Accumulating {
    None,
    Accumulate,
}

#[derive(Clone, Copy)]
enum SignednessSsbi {
    Signed,
    Unsigned,
}

#[derive(Clone, Copy)]
enum NarrowingSsbi {
    Truncation,
    SaturateToUnsigned,
    SaturateToSigned,
}

#[derive(Clone, Copy)]
enum SaturatingShiftLeftTypeSsbi {
    Signed,
    Unsigned,
    SignedWithUnsignedSaturation,
}

#[derive(Clone, Copy)]
enum FloatConversionDirectionSsbi {
    FixedToFloat,
    FloatToFixed,
}

fn highest_set_bit(x: u32) -> usize {
    debug_assert!(x != 0);
    (31 - x.leading_zeros()) as usize
}

fn perform_rounding_correction(
    visitor: &mut TranslatorVisitor<'_>,
    esize: usize,
    round_value: u64,
    original: Value,
    shifted: Value,
) -> Value {
    let round_imm = visitor.i(esize, round_value);
    let round_const = visitor.ir.ir().vector_broadcast(esize, round_imm);
    let masked = visitor.ir.ir().vector_and(original, round_const);
    let round_correction = visitor.ir.ir().vector_equal(esize, masked, round_const);
    visitor.ir.ir().vector_sub(esize, shifted, round_correction)
}

fn shift_right(
    visitor: &mut TranslatorVisitor<'_>,
    inst: &DecodedInst,
    rounding: Rounding,
    accumulating: Accumulating,
    signedness: SignednessSsbi,
) -> bool {
    let q = inst.bit(30);
    let immh = inst.bits(22, 19);
    let immb = inst.bits(18, 16);
    if immh == 0 {
        return visitor.decode_error();
    }
    if (immh & 0b1000) != 0 && !q {
        return visitor.reserved_value();
    }

    let esize = 8usize << highest_set_bit(immh);
    let datasize = if q { 128 } else { 64 };
    let shift_amount = (2 * esize as u8) - ((immh << 3) | immb) as u8;
    let vn = Vec::from_u32(inst.bits(9, 5));
    let vd = Vec::from_u32(inst.rd());

    let operand = visitor.v_read(datasize, vn);
    let mut result = match signedness {
        SignednessSsbi::Signed => {
            visitor
                .ir
                .ir()
                .vector_arithmetic_shift_right(esize, operand, shift_amount)
        }
        SignednessSsbi::Unsigned => {
            visitor
                .ir
                .ir()
                .vector_logical_shift_right(esize, operand, shift_amount)
        }
    };

    if matches!(rounding, Rounding::Round) {
        let round_value = 1u64 << (shift_amount - 1);
        result = perform_rounding_correction(visitor, esize, round_value, operand, result);
    }

    if matches!(accumulating, Accumulating::Accumulate) {
        let accumulator = visitor.v_read(datasize, vd);
        result = visitor.ir.ir().vector_add(esize, result, accumulator);
    }

    visitor.v_write(datasize, vd, result);
    true
}

fn shift_right_narrowing_ssbi(
    visitor: &mut TranslatorVisitor<'_>,
    inst: &DecodedInst,
    rounding: Rounding,
    narrowing: NarrowingSsbi,
    signedness: SignednessSsbi,
) -> bool {
    let q = inst.bit(30);
    let immh = inst.bits(22, 19);
    let immb = inst.bits(18, 16);
    if immh == 0 {
        return visitor.decode_error();
    }
    if (immh & 0b1000) != 0 {
        return visitor.reserved_value();
    }

    let esize = 8usize << highest_set_bit(immh);
    let source_esize = 2 * esize;
    let part = usize::from(q);
    let shift_amount = source_esize as u8 - ((immh << 3) | immb) as u8;
    let vn = Vec::from_u32(inst.bits(9, 5));
    let vd = Vec::from_u32(inst.rd());

    let operand = visitor.v_read(128, vn);
    let mut wide_result = match signedness {
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

    if matches!(rounding, Rounding::Round) {
        let round_value = 1u64 << (shift_amount - 1);
        wide_result =
            perform_rounding_correction(visitor, source_esize, round_value, operand, wide_result);
    }

    let result = match narrowing {
        NarrowingSsbi::Truncation => visitor.ir.ir().vector_narrow(source_esize, wide_result),
        NarrowingSsbi::SaturateToUnsigned => match signedness {
            SignednessSsbi::Signed => visitor
                .ir
                .ir()
                .vector_signed_saturated_narrow_to_unsigned(source_esize, wide_result),
            SignednessSsbi::Unsigned => visitor
                .ir
                .ir()
                .vector_unsigned_saturated_narrow(source_esize, wide_result),
        },
        NarrowingSsbi::SaturateToSigned => {
            debug_assert!(matches!(signedness, SignednessSsbi::Signed));
            visitor
                .ir
                .ir()
                .vector_signed_saturated_narrow_to_signed(source_esize, wide_result)
        }
    };
    visitor.vpart_write_64(vd, part, result);
    true
}

fn shift_left_long(
    visitor: &mut TranslatorVisitor<'_>,
    inst: &DecodedInst,
    signedness: SignednessSsbi,
) -> bool {
    let q = inst.bit(30);
    let immh = inst.bits(22, 19);
    let immb = inst.bits(18, 16);
    if immh == 0 {
        return visitor.decode_error();
    }
    if (immh & 0b1000) != 0 {
        return visitor.reserved_value();
    }

    let esize = 8usize << highest_set_bit(immh);
    let part = usize::from(q);
    let shift_amount = ((immh << 3) | immb) as u8 - esize as u8;
    let vn = Vec::from_u32(inst.bits(9, 5));
    let vd = Vec::from_u32(inst.rd());

    let operand = visitor.vpart_read_64(vn, part);
    let expanded_operand = match signedness {
        SignednessSsbi::Signed => visitor.ir.ir().vector_sign_extend(esize, operand),
        SignednessSsbi::Unsigned => visitor.ir.ir().vector_zero_extend(esize, operand),
    };
    let result =
        visitor
            .ir
            .ir()
            .vector_logical_shift_left(2 * esize, expanded_operand, shift_amount);
    visitor.v_write(128, vd, result);
    true
}

fn saturating_shift_left(
    visitor: &mut TranslatorVisitor<'_>,
    inst: &DecodedInst,
    shift_type: SaturatingShiftLeftTypeSsbi,
) -> bool {
    let q = inst.bit(30);
    let immh = inst.bits(22, 19);
    let immb = inst.bits(18, 16);
    if !q && (immh & 0b1000) != 0 {
        return visitor.reserved_value();
    }

    let esize = 8usize << highest_set_bit(immh);
    let datasize = if q { 128 } else { 64 };
    let shift_amount = ((immh << 3) | immb) as usize - esize;
    let vn = Vec::from_u32(inst.bits(9, 5));
    let vd = Vec::from_u32(inst.rd());
    let operand = visitor.v_read(datasize, vn);
    let shift_imm = visitor.i(esize, shift_amount as u64);
    let shift = visitor.ir.ir().vector_broadcast(esize, shift_imm);
    let result = match shift_type {
        SaturatingShiftLeftTypeSsbi::Signed => visitor
            .ir
            .ir()
            .vector_signed_saturated_shift_left(esize, operand, shift),
        SaturatingShiftLeftTypeSsbi::Unsigned => visitor
            .ir
            .ir()
            .vector_unsigned_saturated_shift_left(esize, operand, shift),
        SaturatingShiftLeftTypeSsbi::SignedWithUnsignedSaturation => visitor
            .ir
            .ir()
            .vector_signed_saturated_shift_left_unsigned(esize, operand, shift_amount as u8),
    };
    visitor.v_write(datasize, vd, result);
    true
}

fn convert_float(
    visitor: &mut TranslatorVisitor<'_>,
    inst: &DecodedInst,
    signedness: SignednessSsbi,
    direction: FloatConversionDirectionSsbi,
    rounding_mode: RoundingMode,
) -> bool {
    let q = inst.bit(30);
    let immh = inst.bits(22, 19);
    let immb = inst.bits(18, 16);
    if immh == 0 {
        return visitor.decode_error();
    }
    if matches!(immh, 0b0001 | 0b0010 | 0b0011) {
        return visitor.reserved_value();
    }
    if (immh & 0b1000) != 0 && !q {
        return visitor.reserved_value();
    }

    let esize = 8usize << highest_set_bit(immh);
    let datasize = if q { 128 } else { 64 };
    let fbits = (esize * 2) as u8 - ((immh << 3) | immb) as u8;
    let vn = Vec::from_u32(inst.bits(9, 5));
    let vd = Vec::from_u32(inst.rd());
    let operand = visitor.v_read(datasize, vn);
    let result = match (direction, signedness) {
        (FloatConversionDirectionSsbi::FixedToFloat, SignednessSsbi::Signed) => visitor
            .ir
            .ir()
            .fp_vector_from_signed_fixed(esize, operand, fbits, rounding_mode as u8, true),
        (FloatConversionDirectionSsbi::FixedToFloat, SignednessSsbi::Unsigned) => visitor
            .ir
            .ir()
            .fp_vector_from_unsigned_fixed(esize, operand, fbits, rounding_mode as u8, true),
        (FloatConversionDirectionSsbi::FloatToFixed, SignednessSsbi::Signed) => visitor
            .ir
            .ir()
            .fp_vector_to_signed_fixed(esize, operand, fbits, rounding_mode as u8, true),
        (FloatConversionDirectionSsbi::FloatToFixed, SignednessSsbi::Unsigned) => visitor
            .ir
            .ir()
            .fp_vector_to_unsigned_fixed(esize, operand, fbits, rounding_mode as u8, true),
    };
    visitor.v_write(datasize, vd, result);
    true
}

impl<'a> TranslatorVisitor<'a> {
    /// SHL (immediate, vector). `0Q0011110IIIIiii010101nnnnnddddd`.
    pub fn shl_2(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let immh = inst.bits(22, 19);
        let immb = inst.bits(18, 16);
        if immh == 0 {
            return self.decode_error();
        }
        if (immh & 0b1000) != 0 && !q {
            return self.reserved_value();
        }
        let esize = 8usize << highest_set_bit(immh);
        let datasize = if q { 128 } else { 64 };
        let shift_amount = ((immh << 3) | immb) as u8 - esize as u8;
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let operand = self.v_read(datasize, vn);
        let result = self
            .ir
            .ir()
            .vector_logical_shift_left(esize, operand, shift_amount);
        self.v_write(datasize, vd, result);
        true
    }

    pub fn sqshl_imm_2(&mut self, inst: &DecodedInst) -> bool {
        saturating_shift_left(self, inst, SaturatingShiftLeftTypeSsbi::Signed)
    }

    pub fn sqshlu_2(&mut self, inst: &DecodedInst) -> bool {
        saturating_shift_left(
            self,
            inst,
            SaturatingShiftLeftTypeSsbi::SignedWithUnsignedSaturation,
        )
    }

    pub fn uqshl_imm_2(&mut self, inst: &DecodedInst) -> bool {
        saturating_shift_left(self, inst, SaturatingShiftLeftTypeSsbi::Unsigned)
    }

    /// USHR (vector). `0Q1011110IIIIiii000001nnnnnddddd`.
    pub fn ushr_2(&mut self, inst: &DecodedInst) -> bool {
        shift_right(
            self,
            inst,
            Rounding::None,
            Accumulating::None,
            SignednessSsbi::Unsigned,
        )
    }

    /// SSHR (vector). `0Q0011110IIIIiii000001nnnnnddddd`.
    pub fn sshr_2(&mut self, inst: &DecodedInst) -> bool {
        shift_right(
            self,
            inst,
            Rounding::None,
            Accumulating::None,
            SignednessSsbi::Signed,
        )
    }

    /// SRSHR (vector). `0Q0011110IIIIiii001001nnnnnddddd`.
    pub fn srshr_2(&mut self, inst: &DecodedInst) -> bool {
        shift_right(
            self,
            inst,
            Rounding::Round,
            Accumulating::None,
            SignednessSsbi::Signed,
        )
    }

    /// SRSRA (vector). `0Q0011110IIIIiii001101nnnnnddddd`.
    pub fn srsra_2(&mut self, inst: &DecodedInst) -> bool {
        shift_right(
            self,
            inst,
            Rounding::Round,
            Accumulating::Accumulate,
            SignednessSsbi::Signed,
        )
    }

    /// SSRA (vector). `0Q0011110IIIIiii000101nnnnnddddd`.
    pub fn ssra_2(&mut self, inst: &DecodedInst) -> bool {
        shift_right(
            self,
            inst,
            Rounding::None,
            Accumulating::Accumulate,
            SignednessSsbi::Signed,
        )
    }

    /// URSHR (vector). `0Q1011110IIIIiii001001nnnnnddddd`.
    pub fn urshr_2(&mut self, inst: &DecodedInst) -> bool {
        shift_right(
            self,
            inst,
            Rounding::Round,
            Accumulating::None,
            SignednessSsbi::Unsigned,
        )
    }

    /// URSRA (vector). `0Q1011110IIIIiii001101nnnnnddddd`.
    pub fn ursra_2(&mut self, inst: &DecodedInst) -> bool {
        shift_right(
            self,
            inst,
            Rounding::Round,
            Accumulating::Accumulate,
            SignednessSsbi::Unsigned,
        )
    }

    /// USRA (vector). `0Q1011110IIIIiii000101nnnnnddddd`.
    pub fn usra_2(&mut self, inst: &DecodedInst) -> bool {
        shift_right(
            self,
            inst,
            Rounding::None,
            Accumulating::Accumulate,
            SignednessSsbi::Unsigned,
        )
    }

    pub fn sri_2(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let immh = inst.bits(22, 19);
        let immb = inst.bits(18, 16);
        if immh == 0 {
            return self.decode_error();
        }
        if !q && (immh & 0b1000) != 0 {
            return self.reserved_value();
        }

        let esize = 8usize << highest_set_bit(immh);
        let datasize = if q { 128 } else { 64 };
        let shift_amount = (esize * 2) as u8 - ((immh << 3) | immb) as u8;
        let mask = if shift_amount as usize == esize {
            0
        } else {
            (u64::MAX >> (64 - esize)) >> shift_amount
        };
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());

        let operand1 = self.v_read(datasize, vn);
        let operand2 = self.v_read(datasize, vd);
        let shifted = self
            .ir
            .ir()
            .vector_logical_shift_right(esize, operand1, shift_amount);
        let mask_imm = self.i(esize, mask);
        let mask_vec = self.ir.ir().vector_broadcast(esize, mask_imm);
        let preserved = self.ir.ir().vector_and_not(operand2, mask_vec);
        let result = self.ir.ir().vector_or(preserved, shifted);

        self.v_write(datasize, vd, result);
        true
    }

    pub fn sli_2(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let immh = inst.bits(22, 19);
        let immb = inst.bits(18, 16);
        if immh == 0 {
            return self.decode_error();
        }
        if !q && (immh & 0b1000) != 0 {
            return self.reserved_value();
        }

        let esize = 8usize << highest_set_bit(immh);
        let datasize = if q { 128 } else { 64 };
        let shift_amount = ((immh << 3) | immb) as u8 - esize as u8;
        let mask = (u64::MAX >> (64 - esize)) << shift_amount;
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());

        let operand1 = self.v_read(datasize, vn);
        let operand2 = self.v_read(datasize, vd);
        let shifted = self
            .ir
            .ir()
            .vector_logical_shift_left(esize, operand1, shift_amount);
        let mask_imm = self.i(esize, mask);
        let mask_vec = self.ir.ir().vector_broadcast(esize, mask_imm);
        let preserved = self.ir.ir().vector_and_not(operand2, mask_vec);
        let result = self.ir.ir().vector_or(preserved, shifted);

        self.v_write(datasize, vd, result);
        true
    }

    pub fn shrn(&mut self, inst: &DecodedInst) -> bool {
        shift_right_narrowing_ssbi(
            self,
            inst,
            Rounding::None,
            NarrowingSsbi::Truncation,
            SignednessSsbi::Unsigned,
        )
    }

    pub fn rshrn(&mut self, inst: &DecodedInst) -> bool {
        shift_right_narrowing_ssbi(
            self,
            inst,
            Rounding::Round,
            NarrowingSsbi::Truncation,
            SignednessSsbi::Unsigned,
        )
    }

    pub fn sqshrn_2(&mut self, inst: &DecodedInst) -> bool {
        shift_right_narrowing_ssbi(
            self,
            inst,
            Rounding::None,
            NarrowingSsbi::SaturateToSigned,
            SignednessSsbi::Signed,
        )
    }

    pub fn sqrshrn_2(&mut self, inst: &DecodedInst) -> bool {
        shift_right_narrowing_ssbi(
            self,
            inst,
            Rounding::Round,
            NarrowingSsbi::SaturateToSigned,
            SignednessSsbi::Signed,
        )
    }

    pub fn sqshrun_2(&mut self, inst: &DecodedInst) -> bool {
        shift_right_narrowing_ssbi(
            self,
            inst,
            Rounding::None,
            NarrowingSsbi::SaturateToUnsigned,
            SignednessSsbi::Signed,
        )
    }

    pub fn sqrshrun_2(&mut self, inst: &DecodedInst) -> bool {
        shift_right_narrowing_ssbi(
            self,
            inst,
            Rounding::Round,
            NarrowingSsbi::SaturateToUnsigned,
            SignednessSsbi::Signed,
        )
    }

    pub fn uqshrn_2(&mut self, inst: &DecodedInst) -> bool {
        shift_right_narrowing_ssbi(
            self,
            inst,
            Rounding::None,
            NarrowingSsbi::SaturateToUnsigned,
            SignednessSsbi::Unsigned,
        )
    }

    pub fn uqrshrn_2(&mut self, inst: &DecodedInst) -> bool {
        shift_right_narrowing_ssbi(
            self,
            inst,
            Rounding::Round,
            NarrowingSsbi::SaturateToUnsigned,
            SignednessSsbi::Unsigned,
        )
    }

    /// SSHLL/SSHLL2. `0Q0011110IIIIiii101001nnnnnddddd`.
    pub fn sshll(&mut self, inst: &DecodedInst) -> bool {
        shift_left_long(self, inst, SignednessSsbi::Signed)
    }

    /// USHLL/USHLL2. `0Q1011110IIIIiii101001nnnnnddddd`.
    pub fn ushll(&mut self, inst: &DecodedInst) -> bool {
        shift_left_long(self, inst, SignednessSsbi::Unsigned)
    }

    pub fn scvtf_fix_2(&mut self, inst: &DecodedInst) -> bool {
        let fpcr = self
            .ir
            .current_location
            .expect("current_location not set")
            .fpcr();
        convert_float(
            self,
            inst,
            SignednessSsbi::Signed,
            FloatConversionDirectionSsbi::FixedToFloat,
            Fpcr::new(fpcr).rmode(),
        )
    }

    pub fn ucvtf_fix_2(&mut self, inst: &DecodedInst) -> bool {
        let fpcr = self
            .ir
            .current_location
            .expect("current_location not set")
            .fpcr();
        convert_float(
            self,
            inst,
            SignednessSsbi::Unsigned,
            FloatConversionDirectionSsbi::FixedToFloat,
            Fpcr::new(fpcr).rmode(),
        )
    }

    pub fn fcvtzs_fix_2(&mut self, inst: &DecodedInst) -> bool {
        convert_float(
            self,
            inst,
            SignednessSsbi::Signed,
            FloatConversionDirectionSsbi::FloatToFixed,
            RoundingMode::TowardsZero,
        )
    }

    pub fn fcvtzu_fix_2(&mut self, inst: &DecodedInst) -> bool {
        convert_float(
            self,
            inst,
            SignednessSsbi::Unsigned,
            FloatConversionDirectionSsbi::FloatToFixed,
            RoundingMode::TowardsZero,
        )
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
    use crate::ir::value::Value;

    fn translate_one(raw: u32) -> (Block, bool) {
        translate_one_with_fpcr(raw, 0)
    }

    fn translate_one_with_fpcr(raw: u32, fpcr: u32) -> (Block, bool) {
        let decoded = decode(raw).expect("instruction should decode");
        let location = A64LocationDescriptor::new(0x1000, fpcr, false);
        let mut block = Block::new(location.to_location());
        let mut visitor = TranslatorVisitor::new(
            &mut block,
            location,
            crate::frontend::a64::translate::TranslationOptions::default(),
        );
        let should_continue = visitor.dispatch(&decoded);
        drop(visitor);
        (block, should_continue)
    }

    fn shift_by_immediate_encoding(
        unsigned: bool,
        q: bool,
        immh: u32,
        immb: u32,
        opcode: u32,
    ) -> u32 {
        0x0F00_0000
            | ((unsigned as u32) << 29)
            | ((q as u32) << 30)
            | (immh << 19)
            | (immb << 16)
            | (opcode << 10)
            | (1 << 5)
    }

    #[test]
    fn shrn_stk_encoding_translates_without_interpret_terminal() {
        let (block, should_continue) = translate_one(0x0F0C8400);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorNarrow16));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorZeroExtend64));
        assert!(!block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::ZeroExtendLongToQuad));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn saturated_shift_right_narrowing_family_uses_matching_ir_opcodes() {
        let cases = [
            (0x0F0E_945C, Opcode::VectorSignedSaturatedNarrowToSigned16),
            (0x0F0E_9C5C, Opcode::VectorSignedSaturatedNarrowToSigned16),
            (0x2F0E_845C, Opcode::VectorSignedSaturatedNarrowToUnsigned16),
            (0x2F0E_8C5C, Opcode::VectorSignedSaturatedNarrowToUnsigned16),
            (0x2F0E_945C, Opcode::VectorUnsignedSaturatedNarrow16),
            (0x2F0E_9C5C, Opcode::VectorUnsignedSaturatedNarrow16),
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
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }

    #[test]
    fn sshll_spacecadet_encoding_translates_without_interpret_terminal() {
        let (block, should_continue) = translate_one(0x0F10A7FF);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorSignExtend16));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorLogicalShiftLeft32));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A64SetQ));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn vector_shift_right_family_matches_upstream_variant_semantics() {
        let cases = [
            (
                0x4F34_07BC,
                Opcode::VectorArithmeticShiftRight32,
                false,
                false,
            ),
            (
                0x4F34_17BC,
                Opcode::VectorArithmeticShiftRight32,
                false,
                true,
            ),
            (
                0x4F34_27BC,
                Opcode::VectorArithmeticShiftRight32,
                true,
                false,
            ),
            (
                0x4F34_37BC,
                Opcode::VectorArithmeticShiftRight32,
                true,
                true,
            ),
            (0x6F34_07BC, Opcode::VectorLogicalShiftRight32, false, false),
            (0x6F34_17BC, Opcode::VectorLogicalShiftRight32, false, true),
            (0x6F34_27BC, Opcode::VectorLogicalShiftRight32, true, false),
            (0x6F34_37BC, Opcode::VectorLogicalShiftRight32, true, true),
        ];

        for (encoding, shift_opcode, rounded, accumulating) in cases {
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert!(
                block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == shift_opcode),
                "encoding 0x{encoding:08X} did not emit {shift_opcode:?}"
            );
            assert_eq!(
                block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == Opcode::VectorEqual32),
                rounded,
                "encoding 0x{encoding:08X} rounding mismatch"
            );
            assert_eq!(
                block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == Opcode::VectorAdd32),
                accumulating,
                "encoding 0x{encoding:08X} accumulation mismatch"
            );
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }

    #[test]
    fn saturating_shift_left_family_uses_matching_ir_operations() {
        let cases = [
            (
                shift_by_immediate_encoding(false, true, 4, 3, 0b011101),
                Opcode::VectorSignedSaturatedShiftLeft32,
            ),
            (
                shift_by_immediate_encoding(true, true, 4, 3, 0b011001),
                Opcode::VectorSignedSaturatedShiftLeftUnsigned32,
            ),
            (
                shift_by_immediate_encoding(true, true, 4, 3, 0b011101),
                Opcode::VectorUnsignedSaturatedShiftLeft32,
            ),
        ];

        for (encoding, expected_opcode) in cases {
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert!(block
                .instructions
                .iter()
                .any(|inst| inst.opcode == expected_opcode));
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }

    #[test]
    fn shift_insert_family_preserves_destination_outside_the_insert_mask() {
        let cases = [
            (
                shift_by_immediate_encoding(true, true, 4, 3, 0b010001),
                Opcode::VectorLogicalShiftRight32,
            ),
            (
                shift_by_immediate_encoding(true, true, 4, 3, 0b010101),
                Opcode::VectorLogicalShiftLeft32,
            ),
        ];

        for (encoding, expected_shift) in cases {
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert!(block
                .instructions
                .iter()
                .any(|inst| inst.opcode == expected_shift));
            assert!(block
                .instructions
                .iter()
                .any(|inst| inst.opcode == Opcode::VectorAndNot));
            assert!(block
                .instructions
                .iter()
                .any(|inst| inst.opcode == Opcode::VectorOr));
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }

    #[test]
    fn vector_fixed_point_conversions_match_signedness_direction_and_rounding() {
        let cases = [
            (
                shift_by_immediate_encoding(false, true, 4, 3, 0b111001),
                Opcode::FPVectorFromSignedFixed32,
                RoundingMode::TowardsMinusInfinity as u8,
            ),
            (
                shift_by_immediate_encoding(true, true, 4, 3, 0b111001),
                Opcode::FPVectorFromUnsignedFixed32,
                RoundingMode::TowardsMinusInfinity as u8,
            ),
            (
                shift_by_immediate_encoding(false, true, 4, 3, 0b111111),
                Opcode::FPVectorToSignedFixed32,
                RoundingMode::TowardsZero as u8,
            ),
            (
                shift_by_immediate_encoding(true, true, 4, 3, 0b111111),
                Opcode::FPVectorToUnsignedFixed32,
                RoundingMode::TowardsZero as u8,
            ),
        ];

        for (encoding, expected_opcode, expected_rounding) in cases {
            let (block, should_continue) = translate_one_with_fpcr(encoding, 2 << 22);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            let conversion = block
                .instructions
                .iter()
                .find(|inst| inst.opcode == expected_opcode)
                .unwrap_or_else(|| {
                    panic!("encoding 0x{encoding:08X} did not emit {expected_opcode:?}")
                });
            assert_eq!(conversion.args[1], Value::ImmU8(29));
            assert_eq!(conversion.args[2], Value::ImmU8(expected_rounding));
            assert_eq!(conversion.args[3], Value::ImmU1(true));
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }
}
