// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `frontend/maxwell/translate/impl/common_funcs.h` and `.cpp`
//!
//! Common helper functions used by multiple instruction translators.

use super::TranslatorVisitor;
use crate::ir::types::FpControl;
use crate::ir::value::Value;

/// Port of upstream `FloatingPointCompare` for F16 operands.
pub fn floating_point_compare_16(
    v: &mut TranslatorVisitor<'_>,
    operand_1: Value,
    operand_2: Value,
    compare_op: u32,
    control: FpControl,
) -> Value {
    match compare_op {
        0 => v.ir.imm_u1(false),
        1 => {
            v.ir.fp_ord_less_than_16_with_control(operand_1, operand_2, control)
        }
        2 => {
            v.ir.fp_ord_equal_16_with_control(operand_1, operand_2, control)
        }
        3 => {
            v.ir.fp_ord_less_than_equal_16_with_control(operand_1, operand_2, control)
        }
        4 => {
            v.ir.fp_ord_greater_than_16_with_control(operand_1, operand_2, control)
        }
        5 => {
            v.ir.fp_ord_not_equal_16_with_control(operand_1, operand_2, control)
        }
        6 => {
            v.ir.fp_ord_greater_than_equal_16_with_control(operand_1, operand_2, control)
        }
        7 => {
            let lhs_nan = v.ir.fp_is_nan_16(operand_1);
            let rhs_nan = v.ir.fp_is_nan_16(operand_2);
            let unordered = v.ir.logical_or(lhs_nan, rhs_nan);
            v.ir.logical_not(unordered)
        }
        8 => {
            let lhs_nan = v.ir.fp_is_nan_16(operand_1);
            let rhs_nan = v.ir.fp_is_nan_16(operand_2);
            v.ir.logical_or(lhs_nan, rhs_nan)
        }
        9 => {
            v.ir.fp_unord_less_than_16_with_control(operand_1, operand_2, control)
        }
        10 => {
            v.ir.fp_unord_equal_16_with_control(operand_1, operand_2, control)
        }
        11 => {
            v.ir.fp_unord_less_than_equal_16_with_control(operand_1, operand_2, control)
        }
        12 => {
            v.ir.fp_unord_greater_than_16_with_control(operand_1, operand_2, control)
        }
        13 => {
            v.ir.fp_unord_not_equal_16_with_control(operand_1, operand_2, control)
        }
        14 => {
            v.ir.fp_unord_greater_than_equal_16_with_control(operand_1, operand_2, control)
        }
        15 => v.ir.imm_u1(true),
        _ => panic!("Invalid FP compare op {compare_op}"),
    }
}

/// Port of upstream `FloatingPointCompare` for F32 operands.
pub fn floating_point_compare_32(
    v: &mut TranslatorVisitor<'_>,
    operand_1: Value,
    operand_2: Value,
    compare_op: u32,
    control: FpControl,
) -> Value {
    match compare_op {
        0 => v.ir.imm_u1(false),
        1 => {
            v.ir.fp_ord_less_than_32_with_control(operand_1, operand_2, control)
        }
        2 => {
            v.ir.fp_ord_equal_32_with_control(operand_1, operand_2, control)
        }
        3 => {
            v.ir.fp_ord_less_than_equal_32_with_control(operand_1, operand_2, control)
        }
        4 => {
            v.ir.fp_ord_greater_than_32_with_control(operand_1, operand_2, control)
        }
        5 => {
            v.ir.fp_ord_not_equal_32_with_control(operand_1, operand_2, control)
        }
        6 => {
            v.ir.fp_ord_greater_than_equal_32_with_control(operand_1, operand_2, control)
        }
        7 => {
            let lhs_nan = v.ir.fp_is_nan_32(operand_1);
            let rhs_nan = v.ir.fp_is_nan_32(operand_2);
            let unordered = v.ir.logical_or(lhs_nan, rhs_nan);
            v.ir.logical_not(unordered)
        }
        8 => {
            let lhs_nan = v.ir.fp_is_nan_32(operand_1);
            let rhs_nan = v.ir.fp_is_nan_32(operand_2);
            v.ir.logical_or(lhs_nan, rhs_nan)
        }
        9 => {
            v.ir.fp_unord_less_than_32_with_control(operand_1, operand_2, control)
        }
        10 => {
            v.ir.fp_unord_equal_32_with_control(operand_1, operand_2, control)
        }
        11 => {
            v.ir.fp_unord_less_than_equal_32_with_control(operand_1, operand_2, control)
        }
        12 => {
            v.ir.fp_unord_greater_than_32_with_control(operand_1, operand_2, control)
        }
        13 => {
            v.ir.fp_unord_not_equal_32_with_control(operand_1, operand_2, control)
        }
        14 => {
            v.ir.fp_unord_greater_than_equal_32_with_control(operand_1, operand_2, control)
        }
        15 => v.ir.imm_u1(true),
        _ => panic!("Invalid FP compare op {compare_op}"),
    }
}

/// Port of upstream `FloatingPointCompare` for F64 operands.
pub fn floating_point_compare_64(
    v: &mut TranslatorVisitor<'_>,
    operand_1: Value,
    operand_2: Value,
    compare_op: u32,
    _control: FpControl,
) -> Value {
    match compare_op {
        0 => v.ir.imm_u1(false),
        1 => v.ir.fp_ord_less_than_64(operand_1, operand_2),
        2 => v.ir.fp_ord_equal_64(operand_1, operand_2),
        3 => v.ir.fp_ord_less_than_equal_64(operand_1, operand_2),
        4 => v.ir.fp_ord_greater_than_64(operand_1, operand_2),
        5 => v.ir.fp_ord_not_equal_64(operand_1, operand_2),
        6 => v.ir.fp_ord_greater_than_equal_64(operand_1, operand_2),
        7 => {
            let lhs_nan = v.ir.fp_is_nan_64(operand_1);
            let rhs_nan = v.ir.fp_is_nan_64(operand_2);
            let unordered = v.ir.logical_or(lhs_nan, rhs_nan);
            v.ir.logical_not(unordered)
        }
        8 => {
            let lhs_nan = v.ir.fp_is_nan_64(operand_1);
            let rhs_nan = v.ir.fp_is_nan_64(operand_2);
            v.ir.logical_or(lhs_nan, rhs_nan)
        }
        9 => v.ir.fp_unord_less_than_64(operand_1, operand_2),
        10 => v.ir.fp_unord_equal_64(operand_1, operand_2),
        11 => v.ir.fp_unord_less_than_equal_64(operand_1, operand_2),
        12 => v.ir.fp_unord_greater_than_64(operand_1, operand_2),
        13 => v.ir.fp_unord_not_equal_64(operand_1, operand_2),
        14 => v.ir.fp_unord_greater_than_equal_64(operand_1, operand_2),
        15 => v.ir.imm_u1(true),
        _ => panic!("Invalid FP compare op {compare_op}"),
    }
}

/// Port of upstream `IntegerCompare`.
pub fn integer_compare(
    v: &mut TranslatorVisitor<'_>,
    operand_1: Value,
    operand_2: Value,
    compare_op: u32,
    is_signed: bool,
) -> Value {
    match compare_op {
        0 => v.ir.imm_u1(false),
        1 if is_signed => v.ir.s_less_than(operand_1, operand_2),
        1 => v.ir.u_less_than(operand_1, operand_2),
        2 => v.ir.i_equal(operand_1, operand_2),
        3 if is_signed => v.ir.s_less_than_equal(operand_1, operand_2),
        3 => v.ir.u_less_than_equal(operand_1, operand_2),
        4 if is_signed => v.ir.s_greater_than(operand_1, operand_2),
        4 => v.ir.u_greater_than(operand_1, operand_2),
        5 => v.ir.i_not_equal(operand_1, operand_2),
        6 if is_signed => v.ir.s_greater_than_equal(operand_1, operand_2),
        6 => v.ir.u_greater_than_equal(operand_1, operand_2),
        7 => v.ir.imm_u1(true),
        _ => panic!("Invalid integer compare op {compare_op}"),
    }
}

/// Port of upstream `ExtendedIntegerCompare`.
pub fn extended_integer_compare(
    v: &mut TranslatorVisitor<'_>,
    operand_1: Value,
    operand_2: Value,
    compare_op: u32,
    is_signed: bool,
) -> Value {
    let zero = v.ir.imm_u32(0);
    let carry_flag = v.ir.get_c_flag();
    let carry = v.ir.select_u32(carry_flag, v.ir.imm_u32(1), zero);
    let not_operand_2 = v.ir.bitwise_not_32(operand_2);
    let difference = v.ir.iadd_32(operand_1, not_operand_2);
    let intermediate = v.ir.iadd_32(difference, carry);
    let z_flag = v.ir.get_z_flag();
    let flip_logic = if is_signed {
        v.ir.imm_u1(false)
    } else {
        let operand_1_negative = v.ir.s_less_than(operand_1, zero);
        let operand_2_negative = v.ir.s_less_than(operand_2, zero);
        v.ir.logical_xor(operand_1_negative, operand_2_negative)
    };

    match compare_op {
        0 => v.ir.imm_u1(false),
        1 => {
            let flipped = v.ir.s_greater_than_equal(intermediate, zero);
            let normal = v.ir.s_less_than(intermediate, zero);
            v.ir.select_u1(flip_logic, flipped, normal)
        }
        2 => {
            let is_zero = v.ir.i_equal(intermediate, zero);
            v.ir.logical_and(is_zero, z_flag)
        }
        3 => {
            let flipped = v.ir.s_greater_than_equal(intermediate, zero);
            let normal = v.ir.s_less_than(intermediate, zero);
            let base_cmp = v.ir.select_u1(flip_logic, flipped, normal);
            let is_zero = v.ir.i_equal(intermediate, zero);
            let equal = v.ir.logical_and(is_zero, z_flag);
            v.ir.logical_or(base_cmp, equal)
        }
        4 => {
            let flipped = v.ir.s_less_than_equal(intermediate, zero);
            let normal = v.ir.s_greater_than(intermediate, zero);
            let base_cmp = v.ir.select_u1(flip_logic, flipped, normal);
            let not_z = v.ir.logical_not(z_flag);
            let is_zero = v.ir.i_equal(intermediate, zero);
            let equal_without_z = v.ir.logical_and(is_zero, not_z);
            v.ir.logical_or(base_cmp, equal_without_z)
        }
        5 => {
            let not_equal = v.ir.i_not_equal(intermediate, zero);
            let is_zero = v.ir.i_equal(intermediate, zero);
            let not_z = v.ir.logical_not(z_flag);
            let equal_without_z = v.ir.logical_and(is_zero, not_z);
            v.ir.logical_or(not_equal, equal_without_z)
        }
        6 => {
            let flipped = v.ir.s_less_than(intermediate, zero);
            let normal = v.ir.s_greater_than_equal(intermediate, zero);
            let base_cmp = v.ir.select_u1(flip_logic, flipped, normal);
            let is_zero = v.ir.i_equal(intermediate, zero);
            let equal = v.ir.logical_and(is_zero, z_flag);
            v.ir.logical_or(base_cmp, equal)
        }
        7 => v.ir.imm_u1(true),
        _ => panic!("Invalid integer compare op {compare_op}"),
    }
}

/// Port of upstream `PredicateCombine`.
pub fn predicate_combine(
    v: &mut TranslatorVisitor<'_>,
    predicate_1: Value,
    predicate_2: Value,
    boolean_op: u32,
) -> Value {
    match boolean_op {
        0 => v.ir.logical_and(predicate_1, predicate_2),
        1 => v.ir.logical_or(predicate_1, predicate_2),
        2 => v.ir.logical_xor(predicate_1, predicate_2),
        // Upstream throws a Shader::NotImplementedException, which pipeline
        // creation catches. An ordinary Rust panic bypasses that typed catch
        // and can abort when propagated through the macro JIT callback.
        _ => std::panic::panic_any(crate::exception::NotImplementedException::new(
            format!("Invalid bop {boolean_op}"),
        )),
    }
}

/// Port of upstream `PredicateOperation`.
pub fn predicate_operation(
    v: &mut TranslatorVisitor<'_>,
    result: Value,
    predicate_op: u32,
) -> Value {
    match predicate_op {
        0 => v.ir.imm_u1(false),
        1 => v.ir.imm_u1(true),
        2 => v.ir.i_equal(result, v.ir.imm_u32(0)),
        3 => v.ir.i_not_equal(result, v.ir.imm_u32(0)),
        _ => panic!("Invalid predicate operation {predicate_op}"),
    }
}

/// Apply FP saturation (clamp to [0.0, 1.0]).
pub fn apply_fp_saturate(v: &mut TranslatorVisitor<'_>, value: Value, saturate: bool) -> Value {
    if saturate {
        v.ir.fp_saturate_32(value)
    } else {
        value
    }
}

/// Apply sign based on a negate flag.
pub fn apply_fp_negate(v: &mut TranslatorVisitor<'_>, value: Value, negate: bool) -> Value {
    if negate {
        v.ir.fp_neg_32(value)
    } else {
        value
    }
}

/// Apply absolute value flag.
pub fn apply_fp_abs(v: &mut TranslatorVisitor<'_>, value: Value, abs: bool) -> Value {
    if abs {
        v.ir.fp_abs_32(value)
    } else {
        value
    }
}

/// Apply absolute value then negate.
pub fn apply_fp_abs_neg(
    v: &mut TranslatorVisitor<'_>,
    value: Value,
    abs: bool,
    neg: bool,
) -> Value {
    let value = apply_fp_abs(v, value, abs);
    apply_fp_negate(v, value, neg)
}

/// Apply integer negate.
pub fn apply_int_negate(v: &mut TranslatorVisitor<'_>, value: Value, negate: bool) -> Value {
    if negate {
        v.ir.ineg_32(value)
    } else {
        value
    }
}

/// Apply bitwise NOT.
pub fn apply_bitwise_not(v: &mut TranslatorVisitor<'_>, value: Value, invert: bool) -> Value {
    if invert {
        v.ir.bitwise_not_32(value)
    } else {
        value
    }
}
