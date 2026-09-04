//! Port of upstream `dynarmic/frontend/A64/translate/impl/simd_scalar_three_same.cpp`.

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;
use crate::ir::value::Value;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ComparisonType {
    Eq,
    Ge,
    Gt,
    Hi,
    Hs,
    Le,
    Lt,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ComparisonVariant {
    Register,
    Zero,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SignednessSsts {
    Signed,
    Unsigned,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FpComparisonType {
    Eq,
    Ge,
    AbsoluteGe,
    Gt,
    AbsoluteGt,
}

fn rounding_shift_left(
    visitor: &mut TranslatorVisitor<'_>,
    size: u32,
    vm: Vec,
    vn: Vec,
    vd: Vec,
    signedness: SignednessSsts,
) -> bool {
    if size != 0b11 {
        return visitor.reserved_value();
    }

    let operand1 = visitor.v_read(64, vn);
    let operand2 = visitor.v_read(64, vm);
    let result = match signedness {
        SignednessSsts::Signed => visitor
            .ir
            .ir()
            .vector_rounding_shift_left_signed(64, operand1, operand2),
        SignednessSsts::Unsigned => visitor
            .ir
            .ir()
            .vector_rounding_shift_left_unsigned(64, operand1, operand2),
    };

    visitor.v_write(64, vd, result);
    true
}

fn scalar_compare(
    visitor: &mut TranslatorVisitor<'_>,
    size: u32,
    vm: Option<Vec>,
    vn: Vec,
    vd: Vec,
    comparison_type: ComparisonType,
    variant: ComparisonVariant,
) -> bool {
    if size != 0b11 {
        return visitor.reserved_value();
    }

    let esize = 64usize;
    let datasize = 64usize;
    let operand1 = visitor.v_read(datasize, vn);
    let operand2 = match variant {
        ComparisonVariant::Register => visitor.v_read(datasize, vm.expect("register variant")),
        ComparisonVariant::Zero => visitor.ir.ir().zero_vector(),
    };
    let result = match comparison_type {
        ComparisonType::Eq => visitor.ir.ir().vector_equal(esize, operand1, operand2),
        ComparisonType::Ge => visitor
            .ir
            .ir()
            .vector_greater_equal_signed(esize, operand1, operand2),
        ComparisonType::Gt => visitor
            .ir
            .ir()
            .vector_greater_signed(esize, operand1, operand2),
        ComparisonType::Hi => visitor
            .ir
            .ir()
            .vector_greater_unsigned(esize, operand1, operand2),
        ComparisonType::Hs => visitor
            .ir
            .ir()
            .vector_greater_equal_unsigned(esize, operand1, operand2),
        ComparisonType::Le => visitor
            .ir
            .ir()
            .vector_less_equal_signed(esize, operand1, operand2),
        ComparisonType::Lt => visitor
            .ir
            .ir()
            .vector_less_signed(esize, operand1, operand2),
    };

    let element = visitor.ir.ir().vector_get_element(esize, result, 0);
    visitor.v_scalar_write(datasize, vd, element);
    true
}

fn scalar_fp_compare_register(
    visitor: &mut TranslatorVisitor<'_>,
    sz: bool,
    vm: Vec,
    vn: Vec,
    vd: Vec,
    comparison_type: FpComparisonType,
) -> bool {
    let esize = if sz { 64 } else { 32 };
    let datasize = esize;
    let operand1 = visitor.v_read(datasize, vn);
    let operand2 = visitor.v_read(datasize, vm);
    let result = match comparison_type {
        FpComparisonType::Eq => visitor
            .ir
            .ir()
            .fp_vector_equal(esize, operand1, operand2, true),
        FpComparisonType::Ge => visitor
            .ir
            .ir()
            .fp_vector_greater_equal(esize, operand1, operand2, true),
        FpComparisonType::AbsoluteGe => {
            let operand1 = visitor.ir.ir().fp_vector_abs(esize, operand1);
            let operand2 = visitor.ir.ir().fp_vector_abs(esize, operand2);
            visitor
                .ir
                .ir()
                .fp_vector_greater_equal(esize, operand1, operand2, true)
        }
        FpComparisonType::Gt => visitor
            .ir
            .ir()
            .fp_vector_greater(esize, operand1, operand2, true),
        FpComparisonType::AbsoluteGt => {
            let operand1 = visitor.ir.ir().fp_vector_abs(esize, operand1);
            let operand2 = visitor.ir.ir().fp_vector_abs(esize, operand2);
            visitor
                .ir
                .ir()
                .fp_vector_greater(esize, operand1, operand2, true)
        }
    };

    let element = visitor.ir.ir().vector_get_element(esize, result, 0);
    visitor.v_scalar_write(datasize, vd, element);
    true
}

impl<'a> TranslatorVisitor<'a> {
    pub fn sqadd_1(&mut self, inst: &DecodedInst) -> bool {
        let esize = 8usize << inst.bits(23, 22);
        let operand1 = self.v_scalar_read(esize, Vec::from_u32(inst.rn()));
        let operand2 = self.v_scalar_read(esize, Vec::from_u32(inst.rm()));
        let result = self.ir.ir().signed_saturated_add(operand1, operand2);
        self.v_scalar_write(esize, Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn sqdmulh_vec_1(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.bits(23, 22);
        if size == 0 || size == 0b11 {
            return self.reserved_value();
        }
        let esize = 8usize << size;
        let operand1 = self.v_scalar_read(esize, Vec::from_u32(inst.rn()));
        let operand2 = self.v_scalar_read(esize, Vec::from_u32(inst.rm()));
        let result = self
            .ir
            .ir()
            .signed_saturated_doubling_multiply_return_high(operand1, operand2);
        self.v_scalar_write(esize, Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn sqrdmulh_vec_1(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.bits(23, 22);
        if size == 0 || size == 0b11 {
            return self.reserved_value();
        }
        let esize = 8usize << size;
        let vn = self.v_read(128, Vec::from_u32(inst.rn()));
        let operand1 = self.ir.ir().vector_get_element(esize, vn, 0);
        let operand1 = self.ir.ir().zero_extend_to_quad(operand1);
        let vm = self.v_read(128, Vec::from_u32(inst.rm()));
        let operand2 = self.ir.ir().vector_get_element(esize, vm, 0);
        let operand2 = self.ir.ir().zero_extend_to_quad(operand2);
        let result = self
            .ir
            .ir()
            .vector_signed_saturated_doubling_multiply_high_rounding(esize, operand1, operand2);
        let element = self.ir.ir().vector_get_element(esize, result, 0);
        self.v_scalar_write(esize, Vec::from_u32(inst.rd()), element);
        true
    }

    pub fn sqsub_1(&mut self, inst: &DecodedInst) -> bool {
        let esize = 8usize << inst.bits(23, 22);
        let operand1 = self.v_scalar_read(esize, Vec::from_u32(inst.rn()));
        let operand2 = self.v_scalar_read(esize, Vec::from_u32(inst.rm()));
        let result = self.ir.ir().signed_saturated_sub(operand1, operand2);
        self.v_scalar_write(esize, Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn uqadd_1(&mut self, inst: &DecodedInst) -> bool {
        let esize = 8usize << inst.bits(23, 22);
        let operand1 = self.v_scalar_read(esize, Vec::from_u32(inst.rn()));
        let operand2 = self.v_scalar_read(esize, Vec::from_u32(inst.rm()));
        let result = self.ir.ir().unsigned_saturated_add(operand1, operand2);
        self.v_scalar_write(esize, Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn uqsub_1(&mut self, inst: &DecodedInst) -> bool {
        let esize = 8usize << inst.bits(23, 22);
        let operand1 = self.v_scalar_read(esize, Vec::from_u32(inst.rn()));
        let operand2 = self.v_scalar_read(esize, Vec::from_u32(inst.rm()));
        let result = self.ir.ir().unsigned_saturated_sub(operand1, operand2);
        self.v_scalar_write(esize, Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn add_1(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.bits(23, 22);
        if size != 0b11 {
            return self.reserved_value();
        }
        let operand1 = self.v_scalar_read(64, Vec::from_u32(inst.rn()));
        let operand2 = self.v_scalar_read(64, Vec::from_u32(inst.rm()));
        let result = self.ir.ir().add_64(operand1, operand2, Value::ImmU1(false));
        self.v_scalar_write(64, Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn cmeq_reg_1(&mut self, inst: &DecodedInst) -> bool {
        scalar_compare(
            self,
            inst.bits(23, 22),
            Some(Vec::from_u32(inst.rm())),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ComparisonType::Eq,
            ComparisonVariant::Register,
        )
    }

    pub fn cmeq_zero_1(&mut self, inst: &DecodedInst) -> bool {
        scalar_compare(
            self,
            inst.bits(23, 22),
            None,
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ComparisonType::Eq,
            ComparisonVariant::Zero,
        )
    }

    pub fn cmge_reg_1(&mut self, inst: &DecodedInst) -> bool {
        scalar_compare(
            self,
            inst.bits(23, 22),
            Some(Vec::from_u32(inst.rm())),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ComparisonType::Ge,
            ComparisonVariant::Register,
        )
    }

    pub fn cmge_zero_1(&mut self, inst: &DecodedInst) -> bool {
        scalar_compare(
            self,
            inst.bits(23, 22),
            None,
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ComparisonType::Ge,
            ComparisonVariant::Zero,
        )
    }

    pub fn cmgt_reg_1(&mut self, inst: &DecodedInst) -> bool {
        scalar_compare(
            self,
            inst.bits(23, 22),
            Some(Vec::from_u32(inst.rm())),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ComparisonType::Gt,
            ComparisonVariant::Register,
        )
    }

    pub fn cmgt_zero_1(&mut self, inst: &DecodedInst) -> bool {
        scalar_compare(
            self,
            inst.bits(23, 22),
            None,
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ComparisonType::Gt,
            ComparisonVariant::Zero,
        )
    }

    pub fn cmle_1(&mut self, inst: &DecodedInst) -> bool {
        scalar_compare(
            self,
            inst.bits(23, 22),
            None,
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ComparisonType::Le,
            ComparisonVariant::Zero,
        )
    }

    pub fn cmlt_1(&mut self, inst: &DecodedInst) -> bool {
        scalar_compare(
            self,
            inst.bits(23, 22),
            None,
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ComparisonType::Lt,
            ComparisonVariant::Zero,
        )
    }

    pub fn cmhi_1(&mut self, inst: &DecodedInst) -> bool {
        scalar_compare(
            self,
            inst.bits(23, 22),
            Some(Vec::from_u32(inst.rm())),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ComparisonType::Hi,
            ComparisonVariant::Register,
        )
    }

    pub fn cmhs_1(&mut self, inst: &DecodedInst) -> bool {
        scalar_compare(
            self,
            inst.bits(23, 22),
            Some(Vec::from_u32(inst.rm())),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            ComparisonType::Hs,
            ComparisonVariant::Register,
        )
    }

    pub fn cmtst_1(&mut self, inst: &DecodedInst) -> bool {
        if inst.bits(23, 22) != 0b11 {
            return self.reserved_value();
        }
        let operand1 = self.v_read(64, Vec::from_u32(inst.rn()));
        let operand2 = self.v_read(64, Vec::from_u32(inst.rm()));
        let anded = self.ir.ir().vector_and(operand1, operand2);
        let zero = self.ir.ir().zero_vector();
        let equal = self.ir.ir().vector_equal(64, anded, zero);
        let result = self.ir.ir().vector_not(equal);
        self.v_write(64, Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn fabd_2(&mut self, inst: &DecodedInst) -> bool {
        let esize = if inst.bit(22) { 64 } else { 32 };
        let operand1 = self.v_scalar_read(esize, Vec::from_u32(inst.rn()));
        let operand2 = self.v_scalar_read(esize, Vec::from_u32(inst.rm()));
        let difference = self.ir.ir().fp_sub(esize, operand1, operand2);
        let result = self.ir.ir().fp_abs(esize, difference);
        self.v_scalar_write(esize, Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn fmulx_vec_2(&mut self, inst: &DecodedInst) -> bool {
        let esize = if inst.bit(22) { 64 } else { 32 };
        let operand1 = self.v_scalar_read(esize, Vec::from_u32(inst.rn()));
        let operand2 = self.v_scalar_read(esize, Vec::from_u32(inst.rm()));
        let result = self.ir.ir().fp_mulx(esize, operand1, operand2);
        self.v_scalar_write(esize, Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn frecps_1(&mut self, inst: &DecodedInst) -> bool {
        let operand1 = self.v_scalar_read(16, Vec::from_u32(inst.rn()));
        let operand2 = self.v_scalar_read(16, Vec::from_u32(inst.rm()));
        let result = self.ir.ir().fp_recip_step_fused(16, operand1, operand2);
        self.v_scalar_write(16, Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn frecps_2(&mut self, inst: &DecodedInst) -> bool {
        let esize = if inst.bit(22) { 64 } else { 32 };
        let operand1 = self.v_scalar_read(esize, Vec::from_u32(inst.rn()));
        let operand2 = self.v_scalar_read(esize, Vec::from_u32(inst.rm()));
        let result = self.ir.ir().fp_recip_step_fused(esize, operand1, operand2);
        self.v_scalar_write(esize, Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn frsqrts_1(&mut self, inst: &DecodedInst) -> bool {
        let operand1 = self.v_scalar_read(16, Vec::from_u32(inst.rn()));
        let operand2 = self.v_scalar_read(16, Vec::from_u32(inst.rm()));
        let result = self.ir.ir().fp_rsqrt_step_fused(16, operand1, operand2);
        self.v_scalar_write(16, Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn frsqrts_2(&mut self, inst: &DecodedInst) -> bool {
        let esize = if inst.bit(22) { 64 } else { 32 };
        let operand1 = self.v_scalar_read(esize, Vec::from_u32(inst.rn()));
        let operand2 = self.v_scalar_read(esize, Vec::from_u32(inst.rm()));
        let result = self.ir.ir().fp_rsqrt_step_fused(esize, operand1, operand2);
        self.v_scalar_write(esize, Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn facge_2(&mut self, inst: &DecodedInst) -> bool {
        scalar_fp_compare_register(
            self,
            inst.bit(22),
            Vec::from_u32(inst.rm()),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            FpComparisonType::AbsoluteGe,
        )
    }

    pub fn facgt_2(&mut self, inst: &DecodedInst) -> bool {
        scalar_fp_compare_register(
            self,
            inst.bit(22),
            Vec::from_u32(inst.rm()),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            FpComparisonType::AbsoluteGt,
        )
    }

    pub fn fcmeq_reg_1(&mut self, inst: &DecodedInst) -> bool {
        let lhs = self.v_read(128, Vec::from_u32(inst.rn()));
        let rhs = self.v_read(128, Vec::from_u32(inst.rm()));
        let result = self.ir.ir().fp_vector_equal(16, lhs, rhs, true);
        let element = self.ir.ir().vector_get_element(16, result, 0);
        self.v_scalar_write(16, Vec::from_u32(inst.rd()), element);
        true
    }

    pub fn fcmeq_reg_2(&mut self, inst: &DecodedInst) -> bool {
        scalar_fp_compare_register(
            self,
            inst.bit(22),
            Vec::from_u32(inst.rm()),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            FpComparisonType::Eq,
        )
    }

    pub fn fcmge_reg_2(&mut self, inst: &DecodedInst) -> bool {
        scalar_fp_compare_register(
            self,
            inst.bit(22),
            Vec::from_u32(inst.rm()),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            FpComparisonType::Ge,
        )
    }

    pub fn fcmgt_reg_2(&mut self, inst: &DecodedInst) -> bool {
        scalar_fp_compare_register(
            self,
            inst.bit(22),
            Vec::from_u32(inst.rm()),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            FpComparisonType::Gt,
        )
    }

    pub fn sqshl_reg_1(&mut self, inst: &DecodedInst) -> bool {
        let esize = 8usize << inst.bits(23, 22);
        let vn = self.v_read(128, Vec::from_u32(inst.rn()));
        let operand1 = self.ir.ir().vector_get_element(esize, vn, 0);
        let operand1 = self.ir.ir().zero_extend_to_quad(operand1);
        let vm = self.v_read(128, Vec::from_u32(inst.rm()));
        let operand2 = self.ir.ir().vector_get_element(esize, vm, 0);
        let operand2 = self.ir.ir().zero_extend_to_quad(operand2);
        let result = self
            .ir
            .ir()
            .vector_signed_saturated_shift_left(esize, operand1, operand2);
        self.ir.set_q(Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn srshl_1(&mut self, inst: &DecodedInst) -> bool {
        rounding_shift_left(
            self,
            inst.bits(23, 22),
            Vec::from_u32(inst.rm()),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            SignednessSsts::Signed,
        )
    }

    pub fn sshl_1(&mut self, inst: &DecodedInst) -> bool {
        if inst.bits(23, 22) != 0b11 {
            return self.reserved_value();
        }
        let operand1 = self.v_read(64, Vec::from_u32(inst.rn()));
        let operand2 = self.v_read(64, Vec::from_u32(inst.rm()));
        let result = self
            .ir
            .ir()
            .vector_arithmetic_v_shift(64, operand1, operand2);
        self.v_write(64, Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn sub_1(&mut self, inst: &DecodedInst) -> bool {
        if inst.bits(23, 22) != 0b11 {
            return self.reserved_value();
        }
        let operand1 = self.v_scalar_read(64, Vec::from_u32(inst.rn()));
        let operand2 = self.v_scalar_read(64, Vec::from_u32(inst.rm()));
        let result = self.ir.ir().sub_64(operand1, operand2, Value::ImmU1(true));
        self.v_scalar_write(64, Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn uqshl_reg_1(&mut self, inst: &DecodedInst) -> bool {
        let esize = 8usize << inst.bits(23, 22);
        let vn = self.v_read(128, Vec::from_u32(inst.rn()));
        let operand1 = self.ir.ir().vector_get_element(esize, vn, 0);
        let operand1 = self.ir.ir().zero_extend_to_quad(operand1);
        let vm = self.v_read(128, Vec::from_u32(inst.rm()));
        let operand2 = self.ir.ir().vector_get_element(esize, vm, 0);
        let operand2 = self.ir.ir().zero_extend_to_quad(operand2);
        let result = self
            .ir
            .ir()
            .vector_unsigned_saturated_shift_left(esize, operand1, operand2);
        self.ir.set_q(Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn urshl_1(&mut self, inst: &DecodedInst) -> bool {
        rounding_shift_left(
            self,
            inst.bits(23, 22),
            Vec::from_u32(inst.rm()),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            SignednessSsts::Unsigned,
        )
    }

    pub fn ushl_1(&mut self, inst: &DecodedInst) -> bool {
        if inst.bits(23, 22) != 0b11 {
            return self.reserved_value();
        }
        let operand1 = self.v_read(64, Vec::from_u32(inst.rn()));
        let operand2 = self.v_read(64, Vec::from_u32(inst.rm()));
        let result = self.ir.ir().vector_logical_v_shift(64, operand1, operand2);
        self.v_write(64, Vec::from_u32(inst.rd()), result);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a64::decoder::{decode, A64InstructionName};
    use crate::frontend::a64::translate::TranslationOptions;
    use crate::ir::block::Block;
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;

    fn integer_encoding(unsigned: bool, size: u32, opcode: u32) -> u32 {
        (if unsigned { 0x7e20_0000 } else { 0x5e20_0000 })
            | (size << 22)
            | (1 << 16)
            | (opcode << 10)
            | (2 << 5)
            | 3
    }

    fn fp_encoding(base: u32, sz: bool, opcode: u32) -> u32 {
        base | ((sz as u32) << 22) | (1 << 16) | (opcode << 10) | (2 << 5) | 3
    }

    fn translate_one(raw: u32) -> (A64InstructionName, Block, bool) {
        let decoded = decode(raw).expect("scalar three-same instruction must decode");
        let location = A64LocationDescriptor::new(0x1000, 0, false);
        let mut block = Block::new(location.to_location());
        let mut visitor =
            TranslatorVisitor::new(&mut block, location, TranslationOptions::default());
        let should_continue = visitor.dispatch(&decoded);
        drop(visitor);
        (decoded.name, block, should_continue)
    }

    #[test]
    fn missing_scalar_three_same_identities_now_dispatch() {
        let cases = [
            (integer_encoding(false, 2, 3), A64InstructionName::SQADD_1),
            (integer_encoding(false, 2, 11), A64InstructionName::SQSUB_1),
            (
                integer_encoding(false, 1, 45),
                A64InstructionName::SQDMULH_vec_1,
            ),
            (
                integer_encoding(true, 1, 45),
                A64InstructionName::SQRDMULH_vec_1,
            ),
            (integer_encoding(true, 2, 3), A64InstructionName::UQADD_1),
            (integer_encoding(true, 2, 11), A64InstructionName::UQSUB_1),
            (
                integer_encoding(false, 2, 19),
                A64InstructionName::SQSHL_reg_1,
            ),
            (integer_encoding(false, 3, 21), A64InstructionName::SRSHL_1),
            (
                integer_encoding(true, 2, 19),
                A64InstructionName::UQSHL_reg_1,
            ),
            (integer_encoding(true, 3, 21), A64InstructionName::URSHL_1),
            (
                fp_encoding(0x5e20_0000, false, 55),
                A64InstructionName::FMULX_vec_2,
            ),
            (
                fp_encoding(0x7e20_0000, false, 59),
                A64InstructionName::FACGE_2,
            ),
            (
                fp_encoding(0x7ea0_0000, false, 59),
                A64InstructionName::FACGT_2,
            ),
        ];

        for (raw, expected_name) in cases {
            let (name, _block, should_continue) = translate_one(raw);
            assert_eq!(name, expected_name, "encoding 0x{raw:08x}");
            assert!(should_continue, "encoding 0x{raw:08x}");
        }
    }

    #[test]
    fn scalar_saturation_visitors_select_matching_ir_operations() {
        let cases = [
            (integer_encoding(false, 2, 3), Opcode::SignedSaturatedAdd32),
            (integer_encoding(false, 2, 11), Opcode::SignedSaturatedSub32),
            (integer_encoding(true, 2, 3), Opcode::UnsignedSaturatedAdd32),
            (
                integer_encoding(true, 2, 11),
                Opcode::UnsignedSaturatedSub32,
            ),
            (
                integer_encoding(false, 1, 45),
                Opcode::SignedSaturatedDoublingMultiplyReturnHigh16,
            ),
            (
                integer_encoding(true, 1, 45),
                Opcode::VectorSignedSaturatedDoublingMultiplyHighRounding16,
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
    fn existing_scalar_and_vector_paths_use_upstream_operand_shapes() {
        let (_, add, _) = translate_one(integer_encoding(false, 3, 33));
        assert_eq!(
            add.instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::VectorGetElement64)
                .count(),
            2
        );

        let (_, compare, _) = translate_one(integer_encoding(false, 3, 15));
        assert_eq!(
            compare
                .instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::A64GetD)
                .count(),
            2
        );
        assert_eq!(
            compare
                .instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::VectorGetElement64)
                .count(),
            1
        );

        let (_, cmtst, _) = translate_one(integer_encoding(false, 3, 35));
        assert!(!cmtst
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorGetElement64));
    }

    #[test]
    fn invalid_rounding_shift_size_is_reserved_not_interpreted() {
        let (_, block, should_continue) = translate_one(integer_encoding(false, 2, 21));
        assert!(!should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A64ExceptionRaised));
    }
}
