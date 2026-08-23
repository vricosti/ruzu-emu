//! Port of upstream `dynarmic/frontend/A64/translate/impl/simd_three_same.cpp`.

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;

#[derive(Clone, Copy)]
enum MinMax {
    Min,
    Max,
}

#[derive(Clone, Copy)]
enum Sign {
    Signed,
    Unsigned,
}

#[derive(Clone, Copy)]
enum AbsDiffExtraBehavior {
    None,
    Accumulate,
}

#[derive(Clone, Copy)]
enum Operation {
    Add,
    Subtract,
}

#[derive(Clone, Copy)]
enum ExtraBehavior {
    None,
    Round,
}

#[derive(Clone, Copy)]
enum FpPairedMinMaxOperation {
    Max,
    MaxNumeric,
    Min,
    MinNumeric,
}

impl<'a> TranslatorVisitor<'a> {
    fn fp_paired_min_max(
        &mut self,
        inst: &DecodedInst,
        operation: FpPairedMinMaxOperation,
    ) -> bool {
        let q = inst.bit(30);
        let sz = inst.bit(22);
        if sz && !q {
            return self.reserved_value();
        }

        let esize = if sz { 64 } else { 32 };
        let datasize = if q { 128 } else { 64 };
        let elements = datasize / esize;
        let boundary = elements / 2;
        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());

        let operand1 = self.v_read(datasize, vn);
        let operand2 = self.v_read(datasize, vm);
        let mut result = self.ir.ir().zero_vector();

        for (operand, result_start_index) in [(operand1, 0usize), (operand2, boundary)] {
            let mut result_index = result_start_index;
            for index in (0..elements).step_by(2) {
                let elem1 = self.ir.ir().vector_get_element(esize, operand, index as u8);
                let elem2 = self
                    .ir
                    .ir()
                    .vector_get_element(esize, operand, (index + 1) as u8);
                let result_elem = match operation {
                    FpPairedMinMaxOperation::Max => self.ir.ir().fp_max(esize, elem1, elem2),
                    FpPairedMinMaxOperation::MaxNumeric => {
                        self.ir.ir().fp_max_numeric(esize, elem1, elem2)
                    }
                    FpPairedMinMaxOperation::Min => self.ir.ir().fp_min(esize, elem1, elem2),
                    FpPairedMinMaxOperation::MinNumeric => {
                        self.ir.ir().fp_min_numeric(esize, elem1, elem2)
                    }
                };
                result =
                    self.ir
                        .ir()
                        .vector_set_element(esize, result, result_index as u8, result_elem);
                result_index += 1;
            }
        }

        self.v_write(datasize, vd, result);
        true
    }

    fn high_narrowing_operation(
        &mut self,
        inst: &DecodedInst,
        operation: Operation,
        behavior: ExtraBehavior,
    ) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 {
            return self.reserved_value();
        }

        let esize = 8usize << size;
        let doubled_esize = 2 * esize;
        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());

        let operand1 = self.v_read(128, vn);
        let operand2 = self.v_read(128, vm);
        let mut wide = match operation {
            Operation::Add => self.ir.ir().vector_add(doubled_esize, operand1, operand2),
            Operation::Subtract => self.ir.ir().vector_sub(doubled_esize, operand1, operand2),
        };

        if matches!(behavior, ExtraBehavior::Round) {
            let round_imm = self.i(doubled_esize, 1u64 << (esize - 1));
            let round_operand = self.ir.ir().vector_broadcast(doubled_esize, round_imm);
            wide = self.ir.ir().vector_add(doubled_esize, wide, round_operand);
        }

        let shifted = self
            .ir
            .ir()
            .vector_logical_shift_right(doubled_esize, wide, esize as u8);
        let result = self.ir.ir().vector_narrow(doubled_esize, shifted);
        self.vpart_write_64(vd, q as usize, result);
        true
    }

    fn paired_min_max_vector(&mut self, inst: &DecodedInst, op: MinMax, sign: Sign) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 {
            return self.reserved_value();
        }
        let rm = Vec::from_u32(inst.bits(20, 16));
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());

        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };

        let operand1 = self.v_read(datasize, rn);
        let operand2 = self.v_read(datasize, rm);

        let result = match (op, sign) {
            (MinMax::Min, Sign::Unsigned) => match q {
                true => self
                    .ir
                    .ir()
                    .vector_paired_min_unsigned(esize, operand1, operand2),
                false => self
                    .ir
                    .ir()
                    .vector_paired_min_unsigned_lower(esize, operand1, operand2),
            },
            (MinMax::Min, Sign::Signed) => match q {
                true => self
                    .ir
                    .ir()
                    .vector_paired_min_signed(esize, operand1, operand2),
                false => self
                    .ir
                    .ir()
                    .vector_paired_min_signed_lower(esize, operand1, operand2),
            },
            (MinMax::Max, Sign::Unsigned) => match q {
                true => self
                    .ir
                    .ir()
                    .vector_paired_max_unsigned(esize, operand1, operand2),
                false => self
                    .ir
                    .ir()
                    .vector_paired_max_unsigned_lower(esize, operand1, operand2),
            },
            (MinMax::Max, Sign::Signed) => match q {
                true => self
                    .ir
                    .ir()
                    .vector_paired_max_signed(esize, operand1, operand2),
                false => self
                    .ir
                    .ir()
                    .vector_paired_max_signed_lower(esize, operand1, operand2),
            },
        };

        self.v_write(datasize, rd, result);
        true
    }

    pub fn uminp(&mut self, inst: &DecodedInst) -> bool {
        self.paired_min_max_vector(inst, MinMax::Min, Sign::Unsigned)
    }

    pub fn sminp(&mut self, inst: &DecodedInst) -> bool {
        self.paired_min_max_vector(inst, MinMax::Min, Sign::Signed)
    }

    pub fn umaxp(&mut self, inst: &DecodedInst) -> bool {
        self.paired_min_max_vector(inst, MinMax::Max, Sign::Unsigned)
    }

    pub fn smaxp(&mut self, inst: &DecodedInst) -> bool {
        self.paired_min_max_vector(inst, MinMax::Max, Sign::Signed)
    }

    fn vector_min_max(&mut self, inst: &DecodedInst, op: MinMax, sign: Sign) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.three_same_inputs(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = match (op, sign) {
            (MinMax::Max, Sign::Signed) => self.ir.ir().vector_max_signed(esize, n, m),
            (MinMax::Max, Sign::Unsigned) => self.ir.ir().vector_max_unsigned(esize, n, m),
            (MinMax::Min, Sign::Signed) => self.ir.ir().vector_min_signed(esize, n, m),
            (MinMax::Min, Sign::Unsigned) => self.ir.ir().vector_min_unsigned(esize, n, m),
        };
        self.v_write(datasize, rd, r);
        true
    }

    pub fn smax(&mut self, inst: &DecodedInst) -> bool {
        self.vector_min_max(inst, MinMax::Max, Sign::Signed)
    }

    pub fn smin(&mut self, inst: &DecodedInst) -> bool {
        self.vector_min_max(inst, MinMax::Min, Sign::Signed)
    }

    pub fn umax(&mut self, inst: &DecodedInst) -> bool {
        self.vector_min_max(inst, MinMax::Max, Sign::Unsigned)
    }

    pub fn umin(&mut self, inst: &DecodedInst) -> bool {
        self.vector_min_max(inst, MinMax::Min, Sign::Unsigned)
    }

    fn absolute_difference(
        &mut self,
        inst: &DecodedInst,
        behavior: AbsDiffExtraBehavior,
        sign: Sign,
    ) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 {
            return self.reserved_value();
        }
        let rm = Vec::from_u32(inst.bits(20, 16));
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };

        let operand1 = self.v_read(datasize, rn);
        let operand2 = self.v_read(datasize, rm);
        let difference = match sign {
            Sign::Signed => self
                .ir
                .ir()
                .vector_signed_absolute_difference(esize, operand1, operand2),
            Sign::Unsigned => self
                .ir
                .ir()
                .vector_unsigned_absolute_difference(esize, operand1, operand2),
        };
        let result = match behavior {
            AbsDiffExtraBehavior::None => difference,
            AbsDiffExtraBehavior::Accumulate => {
                let initial_dest = self.v_read(datasize, rd);
                self.ir.ir().vector_add(esize, initial_dest, difference)
            }
        };

        self.v_write(datasize, rd, result);
        true
    }

    pub fn saba(&mut self, inst: &DecodedInst) -> bool {
        self.absolute_difference(inst, AbsDiffExtraBehavior::Accumulate, Sign::Signed)
    }

    pub fn sabd(&mut self, inst: &DecodedInst) -> bool {
        self.absolute_difference(inst, AbsDiffExtraBehavior::None, Sign::Signed)
    }

    pub fn uaba(&mut self, inst: &DecodedInst) -> bool {
        self.absolute_difference(inst, AbsDiffExtraBehavior::Accumulate, Sign::Unsigned)
    }

    pub fn uabd(&mut self, inst: &DecodedInst) -> bool {
        self.absolute_difference(inst, AbsDiffExtraBehavior::None, Sign::Unsigned)
    }

    fn rounding_halving_add(&mut self, inst: &DecodedInst, sign: Sign) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 {
            return self.reserved_value();
        }
        let rm = Vec::from_u32(inst.bits(20, 16));
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };

        let operand1 = self.v_read(datasize, rm);
        let operand2 = self.v_read(datasize, rn);
        let result = match sign {
            Sign::Signed => self
                .ir
                .ir()
                .vector_rounding_halving_add_signed(esize, operand1, operand2),
            Sign::Unsigned => self
                .ir
                .ir()
                .vector_rounding_halving_add_unsigned(esize, operand1, operand2),
        };

        self.v_write(datasize, rd, result);
        true
    }

    fn saturating_arithmetic_operation(
        &mut self,
        inst: &DecodedInst,
        operation: Operation,
        sign: Sign,
    ) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 && !q {
            return self.reserved_value();
        }

        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());
        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };
        let operand1 = self.v_read(datasize, vn);
        let operand2 = self.v_read(datasize, vm);

        let result = match (operation, sign) {
            (Operation::Add, Sign::Signed) => self
                .ir
                .ir()
                .vector_signed_saturated_add(esize, operand1, operand2),
            (Operation::Subtract, Sign::Signed) => self
                .ir
                .ir()
                .vector_signed_saturated_sub(esize, operand1, operand2),
            (Operation::Add, Sign::Unsigned) => self
                .ir
                .ir()
                .vector_unsigned_saturated_add(esize, operand1, operand2),
            (Operation::Subtract, Sign::Unsigned) => self
                .ir
                .ir()
                .vector_unsigned_saturated_sub(esize, operand1, operand2),
        };

        self.v_write(datasize, vd, result);
        true
    }

    pub fn shadd(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 {
            return self.reserved_value();
        }
        let rm = Vec::from_u32(inst.bits(20, 16));
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };

        let operand1 = self.v_read(datasize, rn);
        let operand2 = self.v_read(datasize, rm);
        let result = self
            .ir
            .ir()
            .vector_halving_add_signed(esize, operand1, operand2);

        self.v_write(datasize, rd, result);
        true
    }

    pub fn shsub(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 {
            return self.reserved_value();
        }
        let rm = Vec::from_u32(inst.bits(20, 16));
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };

        let operand1 = self.v_read(datasize, rn);
        let operand2 = self.v_read(datasize, rm);
        let result = self
            .ir
            .ir()
            .vector_halving_sub_signed(esize, operand1, operand2);

        self.v_write(datasize, rd, result);
        true
    }

    pub fn sqadd_2(&mut self, inst: &DecodedInst) -> bool {
        self.saturating_arithmetic_operation(inst, Operation::Add, Sign::Signed)
    }

    pub fn sqsub_2(&mut self, inst: &DecodedInst) -> bool {
        self.saturating_arithmetic_operation(inst, Operation::Subtract, Sign::Signed)
    }

    pub fn srhadd(&mut self, inst: &DecodedInst) -> bool {
        self.rounding_halving_add(inst, Sign::Signed)
    }

    pub fn uhadd(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 {
            return self.reserved_value();
        }
        let rm = Vec::from_u32(inst.bits(20, 16));
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };

        let operand1 = self.v_read(datasize, rn);
        let operand2 = self.v_read(datasize, rm);
        let result = self
            .ir
            .ir()
            .vector_halving_add_unsigned(esize, operand1, operand2);

        self.v_write(datasize, rd, result);
        true
    }

    pub fn uhsub(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 {
            return self.reserved_value();
        }
        let rm = Vec::from_u32(inst.bits(20, 16));
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };

        let operand1 = self.v_read(datasize, rn);
        let operand2 = self.v_read(datasize, rm);
        let result = self
            .ir
            .ir()
            .vector_halving_sub_unsigned(esize, operand1, operand2);

        self.v_write(datasize, rd, result);
        true
    }

    pub fn uqadd_2(&mut self, inst: &DecodedInst) -> bool {
        self.saturating_arithmetic_operation(inst, Operation::Add, Sign::Unsigned)
    }

    pub fn uqsub_2(&mut self, inst: &DecodedInst) -> bool {
        self.saturating_arithmetic_operation(inst, Operation::Subtract, Sign::Unsigned)
    }

    pub fn urhadd(&mut self, inst: &DecodedInst) -> bool {
        self.rounding_halving_add(inst, Sign::Unsigned)
    }

    // -----------------------------------------------------------------
    // Sized arithmetic / compare / bitwise. All follow the same shape:
    //   read Vn, Vm at `datasize` (Q? 128 : 64), compute via IR helper,
    //   write Vd. Encoding: 0Q001110_zz1mmmmm_oooooo_nnnnnddddd
    //   (size==0b11 + !Q is reserved for non-64-bit ops).
    // -----------------------------------------------------------------

    fn three_same_inputs(&mut self, inst: &DecodedInst) -> Option<(usize, usize, Vec, Vec, Vec)> {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 && !q {
            return None;
        }
        let rm = Vec::from_u32(inst.bits(20, 16));
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };
        Some((esize, datasize, rm, rn, rd))
    }

    pub fn add_vector(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.three_same_inputs(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().vector_add(esize, n, m);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn sub_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.three_same_inputs(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().vector_sub(esize, n, m);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn cmeq_reg_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.three_same_inputs(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().vector_equal(esize, n, m);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn cmge_reg_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.three_same_inputs(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().vector_greater_equal_signed(esize, n, m);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn cmgt_reg_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.three_same_inputs(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().vector_greater_signed(esize, n, m);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn cmhs_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.three_same_inputs(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().vector_greater_equal_unsigned(esize, n, m);
        self.v_write(datasize, rd, r);
        true
    }

    /// CMHI (vector): a > b unsigned. No direct VectorGreaterUnsigned IR op,
    /// so synthesize as `NOT(VectorGreaterEqualUnsigned(b, a))` — equivalent
    /// to upstream's `VectorGreaterUnsigned(a, b)` lowering on x64.
    pub fn cmhi_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.three_same_inputs(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let ge = self.ir.ir().vector_greater_equal_unsigned(esize, m, n);
        let r = self.ir.ir().vector_not(ge);
        self.v_write(datasize, rd, r);
        true
    }

    /// CMTST (vector): bit-test. result[i] = (n[i] & m[i]) != 0.
    /// Synthesized as `NOT(VectorEqual(VectorAnd(n, m), 0))`.
    pub fn cmtst_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.three_same_inputs(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let anded = self.ir.ir().vector_and(n, m);
        let zero = self.ir.ir().zero_vector();
        let eq = self.ir.ir().vector_equal(esize, anded, zero);
        let r = self.ir.ir().vector_not(eq);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn addp_vec(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.three_same_inputs(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = if datasize == 128 {
            self.ir.ir().vector_paired_add(esize, n, m)
        } else {
            self.ir.ir().vector_paired_add_lower(esize, n, m)
        };
        self.v_write(datasize, rd, r);
        true
    }

    pub fn mul_vec(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 {
            return self.reserved_value();
        }
        let rm = Vec::from_u32(inst.bits(20, 16));
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };

        let operand1 = self.v_read(datasize, rn);
        let operand2 = self.v_read(datasize, rm);
        let result = self.ir.ir().vector_multiply(esize, operand1, operand2);
        self.v_write(datasize, rd, result);
        true
    }

    pub fn addhn(&mut self, inst: &DecodedInst) -> bool {
        self.high_narrowing_operation(inst, Operation::Add, ExtraBehavior::None)
    }

    pub fn raddhn(&mut self, inst: &DecodedInst) -> bool {
        self.high_narrowing_operation(inst, Operation::Add, ExtraBehavior::Round)
    }

    pub fn subhn(&mut self, inst: &DecodedInst) -> bool {
        self.high_narrowing_operation(inst, Operation::Subtract, ExtraBehavior::None)
    }

    pub fn rsubhn(&mut self, inst: &DecodedInst) -> bool {
        self.high_narrowing_operation(inst, Operation::Subtract, ExtraBehavior::Round)
    }

    pub fn fmaxnmp_vec_2(&mut self, inst: &DecodedInst) -> bool {
        self.fp_paired_min_max(inst, FpPairedMinMaxOperation::MaxNumeric)
    }

    pub fn fmaxp_vec_2(&mut self, inst: &DecodedInst) -> bool {
        self.fp_paired_min_max(inst, FpPairedMinMaxOperation::Max)
    }

    pub fn fminnmp_vec_2(&mut self, inst: &DecodedInst) -> bool {
        self.fp_paired_min_max(inst, FpPairedMinMaxOperation::MinNumeric)
    }

    pub fn fminp_vec_2(&mut self, inst: &DecodedInst) -> bool {
        self.fp_paired_min_max(inst, FpPairedMinMaxOperation::Min)
    }

    pub fn mla_vec(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 {
            return self.reserved_value();
        }
        let rm = Vec::from_u32(inst.bits(20, 16));
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };

        let operand1 = self.v_read(datasize, rn);
        let operand2 = self.v_read(datasize, rm);
        let operand3 = self.v_read(datasize, rd);
        let product = self.ir.ir().vector_multiply(esize, operand1, operand2);
        let result = self.ir.ir().vector_add(esize, product, operand3);
        self.v_write(datasize, rd, result);
        true
    }

    pub fn mls_vec(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 {
            return self.reserved_value();
        }
        let rm = Vec::from_u32(inst.bits(20, 16));
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };

        let operand1 = self.v_read(datasize, rn);
        let operand2 = self.v_read(datasize, rm);
        let operand3 = self.v_read(datasize, rd);
        let product = self.ir.ir().vector_multiply(esize, operand1, operand2);
        let result = self.ir.ir().vector_sub(esize, operand3, product);
        self.v_write(datasize, rd, result);
        true
    }

    pub fn sshl_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.three_same_inputs(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().vector_arithmetic_v_shift(esize, n, m);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn ushl_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.three_same_inputs(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().vector_logical_v_shift(esize, n, m);
        self.v_write(datasize, rd, r);
        true
    }

    // -----------------------------------------------------------------
    // Bitwise (no `size` field — always whole-vector).
    // Encoding: 0Q001110_001mmmmm_000111_nnnnnddddd (AND/BIC/ORR/ORN/EOR/BIT/BIF/BSL)
    // -----------------------------------------------------------------

    fn bitwise_inputs(&mut self, inst: &DecodedInst) -> (usize, Vec, Vec, Vec) {
        let q = inst.bit(30);
        let rm = Vec::from_u32(inst.bits(20, 16));
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let datasize = if q { 128 } else { 64 };
        (datasize, rm, rn, rd)
    }

    pub fn and_asimd(&mut self, inst: &DecodedInst) -> bool {
        let (datasize, rm, rn, rd) = self.bitwise_inputs(inst);
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().vector_and(n, m);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn bic_asimd_reg(&mut self, inst: &DecodedInst) -> bool {
        let (datasize, rm, rn, rd) = self.bitwise_inputs(inst);
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().vector_and_not(n, m);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn orr_asimd_reg(&mut self, inst: &DecodedInst) -> bool {
        let (datasize, rm, rn, rd) = self.bitwise_inputs(inst);
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().vector_or(n, m);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn orn_asimd(&mut self, inst: &DecodedInst) -> bool {
        let (datasize, rm, rn, rd) = self.bitwise_inputs(inst);
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let not_m = self.ir.ir().vector_not(m);
        let r = self.ir.ir().vector_or(n, not_m);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn eor_asimd(&mut self, inst: &DecodedInst) -> bool {
        let (datasize, rm, rn, rd) = self.bitwise_inputs(inst);
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().vector_eor(n, m);
        self.v_write(datasize, rd, r);
        true
    }

    /// BSL: Vd(mask) selects bits from Vn, otherwise from Vm.
    /// Upstream: `Vm ^ ((Vm ^ Vn) & Vd)`.
    pub fn bsl(&mut self, inst: &DecodedInst) -> bool {
        let (datasize, rm, rn, rd) = self.bitwise_inputs(inst);
        let op_n = self.v_read(datasize, rn);
        let op_m = self.v_read(datasize, rm);
        let op_d = self.v_read(datasize, rd);
        let xor_mn = self.ir.ir().vector_eor(op_m, op_n);
        let masked = self.ir.ir().vector_and(xor_mn, op_d);
        let r = self.ir.ir().vector_eor(op_m, masked);
        self.v_write(datasize, rd, r);
        true
    }

    /// BIT (Bitwise Insert if True): Vd = Vd ^ ((Vd ^ Vn) & Vm).  Same shape
    /// as BSL but with operand1=Vd, operand4=Vn, operand3=Vm.
    pub fn bit(&mut self, inst: &DecodedInst) -> bool {
        let (datasize, rm, rn, rd) = self.bitwise_inputs(inst);
        let op_n = self.v_read(datasize, rn);
        let op_m = self.v_read(datasize, rm);
        let op_d = self.v_read(datasize, rd);
        let xor_dn = self.ir.ir().vector_eor(op_d, op_n);
        let masked = self.ir.ir().vector_and(xor_dn, op_m);
        let r = self.ir.ir().vector_eor(op_d, masked);
        self.v_write(datasize, rd, r);
        true
    }

    /// BIF (Bitwise Insert if False): Vd_new[bit] = Vm[bit] ? Vd_old[bit] : Vn[bit].
    /// Upstream-faithful XOR-chain form: `r = Vd ^ ((Vd ^ Vn) & ~Vm)`.
    pub fn bif(&mut self, inst: &DecodedInst) -> bool {
        let (datasize, rm, rn, rd) = self.bitwise_inputs(inst);
        let op_n = self.v_read(datasize, rn);
        let op_m = self.v_read(datasize, rm);
        let op_d = self.v_read(datasize, rd);
        let inv_m = self.ir.ir().vector_not(op_m);
        let xor_dn = self.ir.ir().vector_eor(op_d, op_n);
        let masked = self.ir.ir().vector_and(xor_dn, inv_m);
        let r = self.ir.ir().vector_eor(op_d, masked);
        self.v_write(datasize, rd, r);
        true
    }

    // -----------------------------------------------------------------
    // FP three-same: FCMEQ_reg / FCMGE_reg / FCMGT_reg
    // FCMEQ_reg_3 (half), _4 (single/double, 128).
    // -----------------------------------------------------------------

    fn fp_three_same_inputs_4(
        &mut self,
        inst: &DecodedInst,
    ) -> Option<(usize, usize, Vec, Vec, Vec)> {
        let q = inst.bit(30);
        let sz = inst.bit(22);
        if sz && !q {
            return None;
        }
        let rm = Vec::from_u32(inst.bits(20, 16));
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let esize = if sz { 64 } else { 32 };
        let datasize = if q { 128 } else { 64 };
        Some((esize, datasize, rm, rn, rd))
    }

    pub fn fcmeq_reg_3(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let rm = Vec::from_u32(inst.bits(20, 16));
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let esize = 16usize;
        let datasize = if q { 128 } else { 64 };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().fp_vector_equal(esize, n, m, true);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn fcmeq_reg_4(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().fp_vector_equal(esize, n, m, true);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn fcmge_reg_4(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().fp_vector_greater_equal(esize, n, m, true);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn fcmgt_reg_4(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().fp_vector_greater(esize, n, m, true);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn fadd_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().fp_vector_add(esize, n, m, true);
        self.v_write(datasize, rd, r);
        true
    }

    pub fn fsub_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().fp_vector_sub(esize, n, m, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FRECPS (vector, FP16) — esize forced to 16.
    pub fn frecps_3(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let rm = Vec::from_u32(inst.bits(20, 16));
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let datasize = if q { 128 } else { 64 };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().fp_vector_recip_step_fused(16, n, m, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FRECPS (vector, single/double).
    pub fn frecps_4(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().fp_vector_recip_step_fused(esize, n, m, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FRSQRTS (vector, FP16) — esize forced to 16.
    pub fn frsqrts_3(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let rm = Vec::from_u32(inst.bits(20, 16));
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let datasize = if q { 128 } else { 64 };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().fp_vector_rsqrt_step_fused(16, n, m, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FRSQRTS (vector, single/double).
    pub fn frsqrts_4(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().fp_vector_rsqrt_step_fused(esize, n, m, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FMUL (vector, single/double).
    pub fn fmul_vec_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().fp_vector_mul(esize, n, m, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FMULX (vector, single/double).
    pub fn fmulx_vec_4(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().fp_vector_mulx(esize, n, m, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FDIV (vector, single/double).
    pub fn fdiv_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let mut r = self.ir.ir().fp_vector_div(esize, n, m, true);
        if datasize == 64 {
            r = self.ir.ir().vector_zero_upper(r);
        }
        self.v_write(datasize, rd, r);
        true
    }

    /// FMLA (vector, single/double) — result = Vd + Vn * Vm (fused).
    pub fn fmla_vec_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let d = self.v_read(datasize, rd);
        let r = self.ir.ir().fp_vector_mul_add(esize, d, n, m, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FMLS (vector, single/double) — result = Vd + (-Vn) * Vm (fused).
    pub fn fmls_vec_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let d = self.v_read(datasize, rd);
        let neg_n = self.ir.ir().fp_vector_neg(esize, n);
        let r = self.ir.ir().fp_vector_mul_add(esize, d, neg_n, m, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FADDP (vector, single/double).
    /// Upstream: `TranslatorVisitor::FADDP_vec_2`.
    pub fn faddp_vec_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = if inst.q() {
            self.ir.ir().fp_vector_paired_add(esize, n, m, true)
        } else {
            self.ir.ir().fp_vector_paired_add_lower(esize, n, m, true)
        };
        self.v_write(datasize, rd, r);
        true
    }

    /// FMAX (vector, single/double).
    pub fn fmax_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().fp_vector_max(esize, n, m, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FMIN (vector, single/double).
    pub fn fmin_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().fp_vector_min(esize, n, m, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FMAXNM (vector, single/double).
    pub fn fmaxnm_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().fp_vector_max_numeric(esize, n, m, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FMINNM (vector, single/double).
    pub fn fminnm_2(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let r = self.ir.ir().fp_vector_min_numeric(esize, n, m, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FABD (vector, single/double) — abs(Vn - Vm).
    pub fn fabd_4(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let diff = self.ir.ir().fp_vector_sub(esize, n, m, true);
        let r = self.ir.ir().fp_vector_abs(esize, diff);
        self.v_write(datasize, rd, r);
        true
    }

    /// FACGE (vector, single/double) — |Vn| >= |Vm|.
    pub fn facge_4(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let abs_n = self.ir.ir().fp_vector_abs(esize, n);
        let abs_m = self.ir.ir().fp_vector_abs(esize, m);
        let r = self
            .ir
            .ir()
            .fp_vector_greater_equal(esize, abs_n, abs_m, true);
        self.v_write(datasize, rd, r);
        true
    }

    /// FACGT (vector, single/double) — |Vn| > |Vm|.
    pub fn facgt_4(&mut self, inst: &DecodedInst) -> bool {
        let Some((esize, datasize, rm, rn, rd)) = self.fp_three_same_inputs_4(inst) else {
            return self.reserved_value();
        };
        let n = self.v_read(datasize, rn);
        let m = self.v_read(datasize, rm);
        let abs_n = self.ir.ir().fp_vector_abs(esize, n);
        let abs_m = self.ir.ir().fp_vector_abs(esize, m);
        let r = self.ir.ir().fp_vector_greater(esize, abs_n, abs_m, true);
        self.v_write(datasize, rd, r);
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

    #[test]
    fn uminp_q0_uses_lower_opcode() {
        let (block, should_continue) = translate_one(0x2E22AC20);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorPairedMinLowerU8));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn uminp_q1_uses_full_opcode() {
        let (block, should_continue) = translate_one(0x6E22AC20);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorPairedMinU8));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn uminp_size_11_raises_reserved_value() {
        let (block, should_continue) = translate_one(0x2EE2AC20);
        assert!(!should_continue);
        assert!(matches!(
            &block.terminal,
            Terminal::CheckHalt { else_ } if matches!(else_.as_ref(), Terminal::ReturnToDispatch)
        ));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A64ExceptionRaised));
    }

    #[test]
    fn fsub_2_stk_encoding_translates_without_interpret_terminal() {
        let (block, should_continue) = translate_one(0x4EFCD41C);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::FPVectorSub64));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn faddp_vec_2_encoding_translates_without_interpret_terminal() {
        let (block, should_continue) = translate_one(0x2E24_D463);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::FPVectorPairedAddLower32));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn absolute_difference_family_uses_matching_ir_opcodes() {
        let cases = [
            (0x4E66_779C, Opcode::VectorSignedAbsoluteDifference16, false),
            (0x4E66_7F9C, Opcode::VectorSignedAbsoluteDifference16, true),
            (
                0x6E66_779C,
                Opcode::VectorUnsignedAbsoluteDifference16,
                false,
            ),
            (
                0x6E66_7F9C,
                Opcode::VectorUnsignedAbsoluteDifference16,
                true,
            ),
        ];

        for (raw, difference_opcode, accumulates) in cases {
            let (block, should_continue) = translate_one(raw);
            assert!(should_continue, "instruction 0x{raw:08X} should translate");
            assert!(
                block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == difference_opcode),
                "instruction 0x{raw:08X} should emit {difference_opcode:?}"
            );
            assert_eq!(
                block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == Opcode::VectorAdd16),
                accumulates,
                "instruction 0x{raw:08X} accumulation mismatch"
            );
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }

    #[test]
    fn halving_add_family_uses_matching_ir_opcodes() {
        let cases = [
            (0x0E34_0400, Opcode::VectorHalvingAddS8),
            (0x0E34_1400, Opcode::VectorRoundingHalvingAddS8),
            (0x2E34_0400, Opcode::VectorHalvingAddU8),
            (0x2E34_1400, Opcode::VectorRoundingHalvingAddU8),
        ];

        for (raw, expected_opcode) in cases {
            let (block, should_continue) = translate_one(raw);
            assert!(should_continue, "instruction 0x{raw:08X} should translate");
            assert!(
                block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == expected_opcode),
                "instruction 0x{raw:08X} should emit {expected_opcode:?}"
            );
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }

    #[test]
    fn saturating_arithmetic_family_uses_matching_ir_opcodes() {
        let cases = [
            (0x4E61_0CE1, Opcode::VectorSignedSaturatedAdd16),
            (0x4E61_2CE1, Opcode::VectorSignedSaturatedSub16),
            (0x6E61_0CE1, Opcode::VectorUnsignedSaturatedAdd16),
            (0x6E61_2CE1, Opcode::VectorUnsignedSaturatedSub16),
        ];

        for (raw, expected_opcode) in cases {
            let (block, should_continue) = translate_one(raw);
            assert!(should_continue, "instruction 0x{raw:08X} should translate");
            assert!(
                block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == expected_opcode),
                "instruction 0x{raw:08X} should emit {expected_opcode:?}"
            );
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }

    #[test]
    fn saturating_arithmetic_q0_size_11_raises_reserved_value() {
        let (block, should_continue) = translate_one(0x0EE1_0CE1);
        assert!(!should_continue);
        assert!(matches!(
            &block.terminal,
            Terminal::CheckHalt { else_ } if matches!(else_.as_ref(), Terminal::ReturnToDispatch)
        ));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A64ExceptionRaised));
    }

    #[test]
    fn halving_sub_family_uses_matching_ir_opcodes() {
        let cases = [
            (0x4E61_24E1, Opcode::VectorHalvingSubS16),
            (0x6E61_24E1, Opcode::VectorHalvingSubU16),
        ];

        for (raw, expected_opcode) in cases {
            let (block, should_continue) = translate_one(raw);
            assert!(should_continue, "instruction 0x{raw:08X} should translate");
            assert!(
                block
                    .instructions
                    .iter()
                    .any(|inst| inst.opcode == expected_opcode),
                "instruction 0x{raw:08X} should emit {expected_opcode:?}"
            );
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }
}
