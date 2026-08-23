//! Port of upstream `dynarmic/frontend/A64/translate/impl/simd_two_register_misc.cpp`.

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;

const ROUND_TO_NEAREST_TIE_EVEN: u8 = 0;
const ROUND_TOWARDS_PLUS_INFINITY: u8 = 1;
const ROUND_TOWARDS_MINUS_INFINITY: u8 = 2;
const ROUND_TOWARDS_ZERO: u8 = 3;
const ROUND_TO_NEAREST_TIE_AWAY_FROM_ZERO: u8 = 4;
const ROUND_TO_ODD: u8 = 5;

#[derive(Clone, Copy)]
enum CmpZero {
    Eq,
    Ge,
    Gt,
    Le,
    Lt,
}

#[derive(Clone, Copy)]
enum SaturatedNarrowKind {
    SignedToSigned,
    SignedToUnsigned,
    Unsigned,
}

#[derive(Clone, Copy)]
enum Signedness {
    Signed,
    Unsigned,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PairedAddLongExtraBehavior {
    None,
    Accumulate,
}

impl<'a> TranslatorVisitor<'a> {
    fn saturated_narrow_2(&mut self, inst: &DecodedInst, kind: SaturatedNarrowKind) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 {
            return self.reserved_value();
        }

        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let original_esize = 16usize << size as usize;
        let operand = self.v_read(128, vn);
        let result = match kind {
            SaturatedNarrowKind::SignedToSigned => self
                .ir
                .ir()
                .vector_signed_saturated_narrow_to_signed(original_esize, operand),
            SaturatedNarrowKind::SignedToUnsigned => self
                .ir
                .ir()
                .vector_signed_saturated_narrow_to_unsigned(original_esize, operand),
            SaturatedNarrowKind::Unsigned => self
                .ir
                .ir()
                .vector_unsigned_saturated_narrow(original_esize, operand),
        };

        self.vpart_write_64(vd, q as usize, result);
        true
    }

    fn two_reg_misc_inputs(&mut self, inst: &DecodedInst) -> Option<(usize, usize, Vec, Vec)> {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 && !q {
            return None;
        }
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };
        Some((esize, datasize, rn, rd))
    }

    fn compare_against_zero(&mut self, inst: &DecodedInst, kind: CmpZero) -> bool {
        let Some((esize, datasize, rn, rd)) = self.two_reg_misc_inputs(inst) else {
            return self.reserved_value();
        };
        let operand = self.v_read(datasize, rn);
        let zero = self.ir.ir().zero_vector();
        let result = match kind {
            CmpZero::Eq => self.ir.ir().vector_equal(esize, operand, zero),
            CmpZero::Ge => self
                .ir
                .ir()
                .vector_greater_equal_signed(esize, operand, zero),
            CmpZero::Gt => self.ir.ir().vector_greater_signed(esize, operand, zero),
            CmpZero::Le => self.ir.ir().vector_less_equal_signed(esize, operand, zero),
            CmpZero::Lt => self.ir.ir().vector_less_signed(esize, operand, zero),
        };
        self.v_write(datasize, rd, result);
        true
    }

    fn paired_add_long(
        &mut self,
        inst: &DecodedInst,
        signedness: Signedness,
        behavior: PairedAddLongExtraBehavior,
    ) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 {
            return self.reserved_value();
        }

        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };
        let operand = self.v_read(datasize, vn);
        let mut result = match signedness {
            Signedness::Signed => self.ir.ir().vector_paired_add_signed_widen(esize, operand),
            Signedness::Unsigned => self
                .ir
                .ir()
                .vector_paired_add_unsigned_widen(esize, operand),
        };

        if behavior == PairedAddLongExtraBehavior::Accumulate {
            let accumulator = self.v_read(datasize, vd);
            result = self.ir.ir().vector_add(esize * 2, accumulator, result);
        }

        self.v_write(datasize, vd, result);
        true
    }

    /// CMEQ Vd.<T>, Vn.<T>, #0
    pub fn cmeq_zero_2(&mut self, inst: &DecodedInst) -> bool {
        self.compare_against_zero(inst, CmpZero::Eq)
    }

    pub fn cmge_zero_2(&mut self, inst: &DecodedInst) -> bool {
        self.compare_against_zero(inst, CmpZero::Ge)
    }

    pub fn cmgt_zero_2(&mut self, inst: &DecodedInst) -> bool {
        self.compare_against_zero(inst, CmpZero::Gt)
    }

    pub fn cmle_2(&mut self, inst: &DecodedInst) -> bool {
        self.compare_against_zero(inst, CmpZero::Le)
    }

    pub fn cmlt_2(&mut self, inst: &DecodedInst) -> bool {
        self.compare_against_zero(inst, CmpZero::Lt)
    }

    pub fn abs_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rn, rd)) = self.two_reg_misc_inputs(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let r = self.ir.ir().vector_abs(esize, n);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn fabs_1(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let datasize = if q { 128 } else { 64 };
        let operand = self.v_read(datasize, rn);
        let result = self.ir.ir().fp_vector_abs(16, operand);
        self.v_write(datasize, rd, result);
        true
    }

    /// NEG (vector): synthesized as `0 - Vn`.
    pub fn neg_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rn, rd)) = self.two_reg_misc_inputs(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let zero = self.ir.ir().zero_vector();
        let r = self.ir.ir().vector_sub(esize, zero, n);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn sqabs_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rn, rd)) = self.two_reg_misc_inputs(inst) else {
            return self.reserved_value();
        };
        let operand = self.v_read(datasize, rn);
        let result = self.ir.ir().vector_signed_saturated_abs(esize, operand);
        self.v_write(datasize, rd, result);
        true
    }

    pub fn sqneg_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rn, rd)) = self.two_reg_misc_inputs(inst) else {
            return self.reserved_value();
        };
        let operand = self.v_read(datasize, rn);
        let result = self.ir.ir().vector_signed_saturated_neg(esize, operand);
        self.v_write(datasize, rd, result);
        true
    }

    pub fn suqadd_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rn, rd)) = self.two_reg_misc_inputs(inst) else {
            return self.reserved_value();
        };
        let operand1 = self.v_read(datasize, rn);
        let operand2 = self.v_read(datasize, rd);
        let result = self
            .ir
            .ir()
            .vector_signed_saturated_accumulate_unsigned(esize, operand1, operand2);
        self.v_write(datasize, rd, result);
        true
    }

    pub fn usqadd_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rn, rd)) = self.two_reg_misc_inputs(inst) else {
            return self.reserved_value();
        };
        let operand1 = self.v_read(datasize, rn);
        let operand2 = self.v_read(datasize, rd);
        let result = self
            .ir
            .ir()
            .vector_unsigned_saturated_accumulate_signed(esize, operand1, operand2);
        self.v_write(datasize, rd, result);
        true
    }

    /// NOT (vector): bitwise complement of all bits, no `size` field.
    pub fn not(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let datasize = if q { 128 } else { 64 };
        let n = self.v_read(datasize, rn);
        let r = self.ir.ir().vector_not(n);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn rbit_asimd(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let datasize = if q { 128 } else { 64 };
        let data = self.v_read(datasize, rn);
        let result = self.ir.ir().vector_reverse_bits(data);
        self.v_write(datasize, rd, result);
        true
    }

    pub fn cls_asimd(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 {
            return self.reserved_value();
        }

        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let operand = self.v_read(datasize, rn);
        let shifted = self
            .ir
            .ir()
            .vector_arithmetic_shift_right(esize, operand, esize as u8);
        let xored = self.ir.ir().vector_eor(operand, shifted);
        let clz = self.ir.ir().vector_count_leading_zeros(esize, xored);
        let one = self.i(esize, 1);
        let one = self.ir.ir().vector_broadcast(esize, one);
        let result = self.ir.ir().vector_sub(esize, clz, one);

        self.v_write(datasize, rd, result);
        true
    }

    /// CNT (vector): population count, byte-wise. `size` must be 0b00.
    pub fn cnt(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size != 0b00 {
            return self.reserved_value();
        }
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let datasize = if q { 128 } else { 64 };
        let n = self.v_read(datasize, rn);
        let r = self.ir.ir().vector_population_count(n);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn clz_asimd(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 {
            return self.reserved_value();
        }

        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let operand = self.v_read(datasize, rn);
        let result = self.ir.ir().vector_count_leading_zeros(esize, operand);

        self.v_write(datasize, rd, result);
        true
    }

    /// REV16 (vector): swap bytes within each 16-bit halfword. size must be 0b00.
    pub fn rev16_asimd(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size > 0 {
            return self.unallocated_encoding();
        }
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let datasize = if q { 128 } else { 64 };
        let n = self.v_read(datasize, rn);
        let r = self.ir.ir().vector_reverse_element_in_groups(16, 8, n);
        self.v_write(datasize, rd, r);
        true
    }

    /// REV32 (vector): reverse byte order within each 32-bit word. size in {0, 1}.
    pub fn rev32_asimd(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size > 1 {
            return self.unallocated_encoding();
        }
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let datasize = if q { 128 } else { 64 };
        let esize = 8usize << size as usize;
        let n = self.v_read(datasize, rn);
        let r = self.ir.ir().vector_reverse_element_in_groups(32, esize, n);
        self.v_write(datasize, rd, r);
        true
    }

    /// REV64 (vector): reverse byte/halfword/word order within each 64-bit
    /// doubleword. size in {0, 1, 2}.
    pub fn rev64_asimd(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size > 2 {
            return self.unallocated_encoding();
        }
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let datasize = if q { 128 } else { 64 };
        let esize = 8usize << size as usize;
        let n = self.v_read(datasize, rn);
        let r = self.ir.ir().vector_reverse_element_in_groups(64, esize, n);
        self.v_write(datasize, rd, r);
        true
    }

    /// XTN (vector narrow): write the low half of Vn (at 2× esize) into the
    /// `Q`-selected half of Vd. size must be 0..2 (since 2*size element must
    /// fit in 64 bits).
    pub fn xtn(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 {
            return self.reserved_value();
        }
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let esize = 8usize << size as usize;
        let operand = self.v_read(128, rn);
        let result = self.ir.ir().vector_narrow(2 * esize, operand);
        self.vpart_write_64(rd, q as usize, result);
        true
    }

    pub fn sqxtun_2(&mut self, inst: &DecodedInst) -> bool {
        self.saturated_narrow_2(inst, SaturatedNarrowKind::SignedToUnsigned)
    }

    pub fn sqxtn_2(&mut self, inst: &DecodedInst) -> bool {
        self.saturated_narrow_2(inst, SaturatedNarrowKind::SignedToSigned)
    }

    pub fn uqxtn_2(&mut self, inst: &DecodedInst) -> bool {
        self.saturated_narrow_2(inst, SaturatedNarrowKind::Unsigned)
    }

    pub fn fcvtl(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let sz = inst.bit(22);

        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let esize = if sz { 32usize } else { 16usize };
        let num_elements = 64 / esize;

        let part = self.vpart_read_64(vn, q as usize);
        let rounding = self.current_fpcr_rounding_mode();
        let mut result = self.ir.ir().zero_vector();

        for i in 0..num_elements {
            let lane = u8::try_from(i).expect("lane index must fit in u8");
            let element = self.ir.ir().vector_get_element(esize, part, lane);
            let converted = if esize == 16 {
                self.ir.ir().fp_half_to_single(element, rounding)
            } else {
                self.ir.ir().fp_single_to_double(element, rounding)
            };
            result = self
                .ir
                .ir()
                .vector_set_element(2 * esize, result, lane, converted);
        }

        self.v_write(128, vd, result);
        true
    }

    pub fn fcvtn(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let sz = inst.bit(22);

        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let datasize = 64usize;
        let esize = if sz { 32usize } else { 16usize };
        let num_elements = datasize / esize;

        let operand = self.v_read(128, vn);
        let rounding = self.current_fpcr_rounding_mode();
        let mut result = self.ir.ir().zero_vector();

        for i in 0..num_elements {
            let lane = u8::try_from(i).expect("lane index must fit in u8");
            let element = self.ir.ir().vector_get_element(2 * esize, operand, lane);
            let converted = if esize == 16 {
                self.ir.ir().fp_single_to_half(element, rounding)
            } else {
                self.ir.ir().fp_double_to_single(element, rounding)
            };
            result = self
                .ir
                .ir()
                .vector_set_element(esize, result, lane, converted);
        }

        self.vpart_write_64(vd, q as usize, result);
        true
    }

    // -----------------------------------------------------------------
    // FP zero compares
    // Encoding: 0Q001110_1z100000_1c0110_nnnnnddddd (where c selects
    // EQ/GE/GT/LE/LT). _4 variants: sz at bit 22 selects 32 vs 64.
    // -----------------------------------------------------------------

    fn fp_two_reg_misc_inputs_4(&mut self, inst: &DecodedInst) -> Option<(usize, usize, Vec, Vec)> {
        let q = inst.bit(30);
        let sz = inst.bit(22);
        if sz && !q {
            return None;
        }
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let esize = if sz { 64 } else { 32 };
        let datasize = if q { 128 } else { 64 };
        Some((esize, datasize, rn, rd))
    }

    fn float_convert_to_integer(
        &mut self,
        inst: &DecodedInst,
        signedness: Signedness,
        rounding: u8,
    ) -> bool {
        let Some((esize, datasize, rn, rd)) = self.fp_two_reg_misc_inputs_4(inst) else {
            return self.reserved_value();
        };
        let operand = self.v_read(datasize, rn);
        let result = match signedness {
            Signedness::Signed => self
                .ir
                .ir()
                .fp_vector_to_signed_fixed(esize, operand, 0, rounding, true),
            Signedness::Unsigned => self
                .ir
                .ir()
                .fp_vector_to_unsigned_fixed(esize, operand, 0, rounding, true),
        };
        self.v_write(datasize, rd, result);
        true
    }

    fn float_round_to_integral(&mut self, inst: &DecodedInst, rounding: u8, exact: bool) -> bool {
        let Some((esize, datasize, rn, rd)) = self.fp_two_reg_misc_inputs_4(inst) else {
            return self.reserved_value();
        };
        let operand = self.v_read(datasize, rn);
        let result = self
            .ir
            .ir()
            .fp_vector_round_int(esize, operand, rounding, exact, true);
        self.v_write(datasize, rd, result);
        true
    }

    fn float_round_to_integral_half_precision(
        &mut self,
        inst: &DecodedInst,
        rounding: u8,
        exact: bool,
    ) -> bool {
        let q = inst.bit(30);
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let datasize = if q { 128 } else { 64 };
        let operand = self.v_read(datasize, rn);
        let result = self
            .ir
            .ir()
            .fp_vector_round_int(16, operand, rounding, exact, true);
        self.v_write(datasize, rd, result);
        true
    }

    pub fn frintn_1(&mut self, inst: &DecodedInst) -> bool {
        self.float_round_to_integral_half_precision(inst, ROUND_TO_NEAREST_TIE_EVEN, false)
    }

    pub fn frintn_2(&mut self, inst: &DecodedInst) -> bool {
        self.float_round_to_integral(inst, ROUND_TO_NEAREST_TIE_EVEN, false)
    }

    pub fn frintm_1(&mut self, inst: &DecodedInst) -> bool {
        self.float_round_to_integral_half_precision(inst, ROUND_TOWARDS_MINUS_INFINITY, false)
    }

    pub fn frintm_2(&mut self, inst: &DecodedInst) -> bool {
        self.float_round_to_integral(inst, ROUND_TOWARDS_MINUS_INFINITY, false)
    }

    pub fn frintp_1(&mut self, inst: &DecodedInst) -> bool {
        self.float_round_to_integral_half_precision(inst, ROUND_TOWARDS_PLUS_INFINITY, false)
    }

    pub fn frintp_2(&mut self, inst: &DecodedInst) -> bool {
        self.float_round_to_integral(inst, ROUND_TOWARDS_PLUS_INFINITY, false)
    }

    pub fn frintz_1(&mut self, inst: &DecodedInst) -> bool {
        self.float_round_to_integral_half_precision(inst, ROUND_TOWARDS_ZERO, false)
    }

    pub fn frintz_2(&mut self, inst: &DecodedInst) -> bool {
        self.float_round_to_integral(inst, ROUND_TOWARDS_ZERO, false)
    }

    pub fn frinta_1(&mut self, inst: &DecodedInst) -> bool {
        self.float_round_to_integral_half_precision(
            inst,
            ROUND_TO_NEAREST_TIE_AWAY_FROM_ZERO,
            false,
        )
    }

    pub fn frinta_2(&mut self, inst: &DecodedInst) -> bool {
        self.float_round_to_integral(inst, ROUND_TO_NEAREST_TIE_AWAY_FROM_ZERO, false)
    }

    pub fn frintx_1(&mut self, inst: &DecodedInst) -> bool {
        let rounding = self.current_fpcr_rounding_mode();
        self.float_round_to_integral_half_precision(inst, rounding, true)
    }

    pub fn frintx_2(&mut self, inst: &DecodedInst) -> bool {
        let rounding = self.current_fpcr_rounding_mode();
        self.float_round_to_integral(inst, rounding, true)
    }

    pub fn frinti_1(&mut self, inst: &DecodedInst) -> bool {
        let rounding = self.current_fpcr_rounding_mode();
        self.float_round_to_integral_half_precision(inst, rounding, false)
    }

    pub fn frinti_2(&mut self, inst: &DecodedInst) -> bool {
        let rounding = self.current_fpcr_rounding_mode();
        self.float_round_to_integral(inst, rounding, false)
    }

    pub fn fcmeq_zero_4(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rn, rd)) = self.fp_two_reg_misc_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let zero = self.ir.ir().zero_vector();
        let r = self.ir.ir().fp_vector_equal(esize, n, zero, true);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn fabs_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rn, rd)) = self.fp_two_reg_misc_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let r = self.ir.ir().fp_vector_abs(esize, n);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn fneg_1(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let datasize = if q { 128 } else { 64 };
        let operand = self.v_read(datasize, rn);
        let mask = self.i(64, 0x8000_8000_8000_8000);
        let mask = self.ir.ir().vector_broadcast(64, mask);
        let result = self.ir.ir().vector_eor(operand, mask);
        self.v_write(datasize, rd, result);
        true
    }

    pub fn fneg_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rn, rd)) = self.fp_two_reg_misc_inputs_4(inst) else {
            return self.reserved_value();
        };
        let operand = self.v_read(datasize, rn);
        let mask_value = if esize == 64 {
            0x8000_0000_0000_0000
        } else {
            0x8000_0000_8000_0000
        };
        let mask = self.i(esize, mask_value);
        let mask = if datasize == 128 {
            self.ir.ir().vector_broadcast(esize, mask)
        } else {
            self.ir.ir().vector_broadcast_lower(esize, mask)
        };
        let result = self.ir.ir().vector_eor(operand, mask);
        self.v_write(datasize, rd, result);
        true
    }

    pub fn fcmge_zero_4(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rn, rd)) = self.fp_two_reg_misc_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let zero = self.ir.ir().zero_vector();
        let r = self.ir.ir().fp_vector_greater_equal(esize, n, zero, true);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn fcmgt_zero_4(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rn, rd)) = self.fp_two_reg_misc_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let zero = self.ir.ir().zero_vector();
        let r = self.ir.ir().fp_vector_greater(esize, n, zero, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FCMLE (vector, zero, single/double). `operand <= 0.0`.
    /// Upstream: `TranslatorVisitor::FCMLE_4`.
    pub fn fcmle_4(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rn, rd)) = self.fp_two_reg_misc_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let zero = self.ir.ir().zero_vector();
        let r = self.ir.ir().fp_vector_greater_equal(esize, zero, n, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FCMLT (vector, zero, single/double). `operand < 0.0`.
    /// Upstream: `TranslatorVisitor::FCMLT_4`.
    pub fn fcmlt_4(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rn, rd)) = self.fp_two_reg_misc_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let zero = self.ir.ir().zero_vector();
        let r = self.ir.ir().fp_vector_greater(esize, zero, n, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FCMEQ_zero_3 (FP16 vector compare-equal-zero).
    pub fn fcmeq_zero_3(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let datasize = if q { 128 } else { 64 };
        let n = self.v_read(datasize, rn);
        let zero = self.ir.ir().zero_vector();
        let r = self.ir.ir().fp_vector_equal(16, n, zero, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FRECPE (vector, FP16) — esize forced to 16.
    pub fn frecpe_3(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let datasize = if q { 128 } else { 64 };
        let n = self.v_read(datasize, rn);
        let r = self.ir.ir().fp_vector_recip_estimate(16, n, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FRECPE (vector, single/double).
    pub fn frecpe_4(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rn, rd)) = self.fp_two_reg_misc_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let r = self.ir.ir().fp_vector_recip_estimate(esize, n, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FRSQRTE (vector, FP16) — esize forced to 16.
    pub fn frsqrte_3(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let datasize = if q { 128 } else { 64 };
        let n = self.v_read(datasize, rn);
        let r = self.ir.ir().fp_vector_rsqrt_estimate(16, n, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FRSQRTE (vector, single/double).
    pub fn frsqrte_4(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rn, rd)) = self.fp_two_reg_misc_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let r = self.ir.ir().fp_vector_rsqrt_estimate(esize, n, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FSQRT (vector, single/double).
    pub fn fsqrt_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rn, rd)) = self.fp_two_reg_misc_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let r = self.ir.ir().fp_vector_sqrt(esize, n, true);
        self.v_write(datasize, rd, r);
        true
    }

    // -----------------------------------------------------------------
    // FP int <-> conversions (vector form, no fbits — fbits=0)
    //   FCVTZS_int_4 / FCVTZU_int_4 — float → signed/unsigned integer
    //                                  with TowardsZero rounding
    //   SCVTF_int_4  / UCVTF_int_4  — signed/unsigned integer → float
    //                                  with FPCR-current rounding
    // -----------------------------------------------------------------

    fn current_fpcr_rounding_mode(&self) -> u8 {
        ((self
            .ir
            .current_location
            .expect("current_location not set")
            .fpcr()
            >> 22)
            & 0x3) as u8
    }

    pub fn fcvtzs_int_4(&mut self, inst: &DecodedInst) -> bool {
        self.float_convert_to_integer(inst, Signedness::Signed, ROUND_TOWARDS_ZERO)
    }

    pub fn fcvtzu_int_4(&mut self, inst: &DecodedInst) -> bool {
        self.float_convert_to_integer(inst, Signedness::Unsigned, ROUND_TOWARDS_ZERO)
    }

    pub fn fcvtns_4(&mut self, inst: &DecodedInst) -> bool {
        self.float_convert_to_integer(inst, Signedness::Signed, ROUND_TO_NEAREST_TIE_EVEN)
    }

    pub fn fcvtms_4(&mut self, inst: &DecodedInst) -> bool {
        self.float_convert_to_integer(inst, Signedness::Signed, ROUND_TOWARDS_MINUS_INFINITY)
    }

    pub fn fcvtas_4(&mut self, inst: &DecodedInst) -> bool {
        self.float_convert_to_integer(
            inst,
            Signedness::Signed,
            ROUND_TO_NEAREST_TIE_AWAY_FROM_ZERO,
        )
    }

    pub fn fcvtps_4(&mut self, inst: &DecodedInst) -> bool {
        self.float_convert_to_integer(inst, Signedness::Signed, ROUND_TOWARDS_PLUS_INFINITY)
    }

    pub fn fcvtnu_4(&mut self, inst: &DecodedInst) -> bool {
        self.float_convert_to_integer(inst, Signedness::Unsigned, ROUND_TO_NEAREST_TIE_EVEN)
    }

    pub fn fcvtmu_4(&mut self, inst: &DecodedInst) -> bool {
        self.float_convert_to_integer(inst, Signedness::Unsigned, ROUND_TOWARDS_MINUS_INFINITY)
    }

    pub fn fcvtau_4(&mut self, inst: &DecodedInst) -> bool {
        self.float_convert_to_integer(
            inst,
            Signedness::Unsigned,
            ROUND_TO_NEAREST_TIE_AWAY_FROM_ZERO,
        )
    }

    pub fn fcvtpu_4(&mut self, inst: &DecodedInst) -> bool {
        self.float_convert_to_integer(inst, Signedness::Unsigned, ROUND_TOWARDS_PLUS_INFINITY)
    }

    pub fn fcvtxn_2(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let sz = inst.bit(22);
        if !sz {
            return self.unallocated_encoding();
        }

        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let operand = self.v_read(128, vn);
        let mut result = self.ir.ir().zero_vector();
        for lane in 0..2 {
            let element = self.ir.ir().vector_get_element(64, operand, lane);
            let converted = self.ir.ir().fp_double_to_single(element, ROUND_TO_ODD);
            result = self.ir.ir().vector_set_element(32, result, lane, converted);
        }
        self.vpart_write_64(vd, q as usize, result);
        true
    }

    pub fn sadalp(&mut self, inst: &DecodedInst) -> bool {
        self.paired_add_long(
            inst,
            Signedness::Signed,
            PairedAddLongExtraBehavior::Accumulate,
        )
    }

    pub fn saddlp(&mut self, inst: &DecodedInst) -> bool {
        self.paired_add_long(inst, Signedness::Signed, PairedAddLongExtraBehavior::None)
    }

    pub fn uadalp(&mut self, inst: &DecodedInst) -> bool {
        self.paired_add_long(
            inst,
            Signedness::Unsigned,
            PairedAddLongExtraBehavior::Accumulate,
        )
    }

    pub fn uaddlp(&mut self, inst: &DecodedInst) -> bool {
        self.paired_add_long(inst, Signedness::Unsigned, PairedAddLongExtraBehavior::None)
    }

    pub fn urecpe(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let sz = inst.bit(22);
        if sz {
            return self.reserved_value();
        }

        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let datasize = if q { 128 } else { 64 };
        let operand = self.v_read(datasize, rn);
        let result = self.ir.ir().vector_unsigned_recip_estimate(operand);
        self.v_write(datasize, rd, result);
        true
    }

    pub fn ursqrte(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let sz = inst.bit(22);
        if sz {
            return self.reserved_value();
        }

        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let datasize = if q { 128 } else { 64 };
        let operand = self.v_read(datasize, rn);
        let result = self.ir.ir().vector_unsigned_recip_sqrt_estimate(operand);
        self.v_write(datasize, rd, result);
        true
    }

    pub fn scvtf_int_4(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rn, rd)) = self.fp_two_reg_misc_inputs_4(inst) else {
            return self.reserved_value();
        };
        let rounding = self.current_fpcr_rounding_mode();
        let n = self.v_read(datasize, rn);
        let r = self
            .ir
            .ir()
            .fp_vector_from_signed_fixed(esize, n, 0, rounding, true);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn ucvtf_int_4(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rn, rd)) = self.fp_two_reg_misc_inputs_4(inst) else {
            return self.reserved_value();
        };
        let rounding = self.current_fpcr_rounding_mode();
        let n = self.v_read(datasize, rn);
        let r = self
            .ir
            .ir()
            .fp_vector_from_unsigned_fixed(esize, n, 0, rounding, true);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn shll(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 {
            return self.reserved_value();
        }

        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let esize = 8usize << size as usize;

        let operand = self.vpart_read_64(vn, q as usize);
        let operand = self.ir.ir().vector_zero_extend(esize, operand);
        let result = self
            .ir
            .ir()
            .vector_logical_shift_left(esize * 2, operand, esize as u8);

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
    use crate::ir::value::Value;

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

    #[test]
    fn cmeq_zero_2_uses_vector_equal_opcode() {
        let (block, should_continue) = translate_one(0x0E209820);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorEqual8));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn cmle_2_uses_edens_greater_then_not_sequence() {
        let (block, should_continue) = translate_one(0x2E209820);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorGreaterS8));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorNot));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn cmlt_2_uses_edens_greater_equal_or_not_sequence() {
        let (block, should_continue) = translate_one(0x0E20A820);
        assert!(should_continue);
        for opcode in [
            Opcode::VectorGreaterS8,
            Opcode::VectorEqual8,
            Opcode::VectorOr,
            Opcode::VectorNot,
        ] {
            assert!(block.instructions.iter().any(|inst| inst.opcode == opcode));
        }
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn clz_asimd_runtime_encoding_uses_vector_clz32() {
        let (block, should_continue) = translate_one(0x2EA0_4800);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorCountLeadingZeros32));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn cls_asimd_uses_upstream_ir_sequence() {
        let (block, should_continue) = translate_one(0x0EA0_4800);
        assert!(should_continue);
        let opcodes: std::vec::Vec<_> = block.instructions.iter().map(|inst| inst.opcode).collect();
        for expected in [
            Opcode::VectorArithmeticShiftRight32,
            Opcode::VectorEor,
            Opcode::VectorCountLeadingZeros32,
            Opcode::VectorBroadcast32,
            Opcode::VectorSub32,
        ] {
            assert!(
                opcodes.contains(&expected),
                "CLS did not emit {expected:?}: {opcodes:?}"
            );
        }
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn fcvtn_encoding_translates_without_interpret_terminal() {
        let (block, should_continue) = translate_one(0x0E616BFF);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::FPDoubleToSingle));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn fcvtl_encoding_translates_without_interpret_terminal() {
        let (block, should_continue) = translate_one(0x0E21_7800);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::FPHalfToSingle));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn shll_runtime_encoding_uses_upstream_ir_sequence() {
        let (block, should_continue) = translate_one(0x2E61_3800);
        assert!(should_continue);
        let opcodes: std::vec::Vec<_> = block.instructions.iter().map(|inst| inst.opcode).collect();
        for expected in [Opcode::VectorZeroExtend16, Opcode::VectorLogicalShiftLeft32] {
            assert!(
                opcodes.contains(&expected),
                "SHLL did not emit {expected:?}: {opcodes:?}"
            );
        }
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn upstream_two_register_misc_instruction_set_is_dispatched() {
        let cases = [
            (0x0E20_2800, Opcode::VectorPairedAddSignedWiden8),
            (
                0x0E20_3800,
                Opcode::VectorSignedSaturatedAccumulateUnsigned8,
            ),
            (0x0E20_6800, Opcode::VectorPairedAddSignedWiden8),
            (0x0E20_7800, Opcode::VectorSignedSaturatedAbs8),
            (0x0E21_A800, Opcode::FPVectorToSignedFixed32),
            (0x0E21_B800, Opcode::FPVectorToSignedFixed32),
            (0x0E21_C800, Opcode::FPVectorToSignedFixed32),
            (0x0EF8_F800, Opcode::FPVectorAbs16),
            (0x0EA1_A800, Opcode::FPVectorToSignedFixed32),
            (0x0EA1_C800, Opcode::VectorUnsignedRecipEstimate),
            (0x2E20_2800, Opcode::VectorPairedAddUnsignedWiden8),
            (
                0x2E20_3800,
                Opcode::VectorUnsignedSaturatedAccumulateSigned8,
            ),
            (0x2E20_6800, Opcode::VectorPairedAddUnsignedWiden8),
            (0x2E20_7800, Opcode::VectorSignedSaturatedNeg8),
            (0x2E61_6800, Opcode::FPDoubleToSingle),
            (0x2E21_A800, Opcode::FPVectorToUnsignedFixed32),
            (0x2E21_B800, Opcode::FPVectorToUnsignedFixed32),
            (0x2E21_C800, Opcode::FPVectorToUnsignedFixed32),
            (0x2E60_5800, Opcode::VectorReverseBits),
            (0x2EF8_F800, Opcode::VectorEor),
            (0x2EA1_A800, Opcode::FPVectorToUnsignedFixed32),
            (0x2EA1_C800, Opcode::VectorUnsignedRecipSqrtEstimate),
        ];

        for (encoding, expected_opcode) in cases {
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert!(
                block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == expected_opcode),
                "encoding 0x{encoding:08X} did not emit {expected_opcode:?}: {:?}",
                block
                    .instructions
                    .iter()
                    .map(|inst| inst.opcode)
                    .collect::<std::vec::Vec<_>>()
            );
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }

    #[test]
    fn saturated_narrow_2_family_uses_matching_ir_opcodes() {
        let cases = [
            (0x0E21_49F0, Opcode::VectorSignedSaturatedNarrowToSigned16),
            (0x2E21_29F0, Opcode::VectorSignedSaturatedNarrowToUnsigned16),
            (0x2E21_49F0, Opcode::VectorUnsignedSaturatedNarrow16),
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
    fn fneg_2_runtime_encoding_uses_upstream_xor_sequence() {
        let (block, should_continue) = translate_one(0x6EE0FBBD);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorEor));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn fcmlt_4_encoding_translates_without_interpret_terminal() {
        let (block, should_continue) = translate_one(0x4EA0_E860);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::FPVectorGreater32));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn fcmle_4_translates_without_interpret_terminal() {
        // FCMLE Vd.4S, Vn.4S, #0.0 — Q=1, sz=0 (esize=32). Emits GreaterEqual(zero, n).
        let (block, should_continue) = translate_one(0x6EA0_D820);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::FPVectorGreaterEqual32));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn frint_vector_family_uses_matching_rounding_ir() {
        let cases = [
            (0x0E79_8820, Opcode::FPVectorRoundInt16, 0, false),
            (0x4E21_8BDE, Opcode::FPVectorRoundInt32, 0, false),
            (0x4E79_98A4, Opcode::FPVectorRoundInt16, 2, false),
            (0x4E61_98E6, Opcode::FPVectorRoundInt64, 2, false),
            (0x0EF9_8928, Opcode::FPVectorRoundInt16, 1, false),
            (0x0EA1_896A, Opcode::FPVectorRoundInt32, 1, false),
            (0x4EF9_99AC, Opcode::FPVectorRoundInt16, 3, false),
            (0x4EE1_99EE, Opcode::FPVectorRoundInt64, 3, false),
            (0x2E79_8A30, Opcode::FPVectorRoundInt16, 4, false),
            (0x6E21_8A72, Opcode::FPVectorRoundInt32, 4, false),
            (0x6E79_9AB4, Opcode::FPVectorRoundInt16, 0, true),
            (0x6E61_9AF6, Opcode::FPVectorRoundInt64, 0, true),
            (0x2EF9_9B38, Opcode::FPVectorRoundInt16, 0, false),
            (0x6EA1_9BDE, Opcode::FPVectorRoundInt32, 0, false),
        ];

        for (encoding, expected_opcode, expected_rounding, expected_exact) in cases {
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            let round = block
                .instructions
                .iter()
                .find(|inst| inst.opcode == expected_opcode)
                .unwrap_or_else(|| {
                    panic!("encoding 0x{encoding:08X} did not emit {expected_opcode:?}")
                });
            assert_eq!(round.args[1], Value::ImmU8(expected_rounding));
            assert_eq!(round.args[2], Value::ImmU1(expected_exact));
            assert_eq!(round.args[3], Value::ImmU1(true));
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }
}
