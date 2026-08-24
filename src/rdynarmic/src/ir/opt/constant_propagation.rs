use crate::ir::block::Block;
use crate::ir::opcode::Opcode;
use crate::ir::value::{InstRef, Value};

/// Constant propagation pass.
/// Folds instructions with all-immediate arguments into immediate results.
/// Also handles algebraic simplifications (x + 0 = x, x & 0 = 0, etc.).
pub fn constant_propagation(block: &mut Block) {
    let len = block.instructions.len();

    for i in 0..len {
        if block.instructions[i].is_tombstone() {
            continue;
        }

        let opcode = block.instructions[i].opcode;
        let inst_ref = InstRef(i as u32);

        match opcode {
            // --- Truncation / extraction ---
            Opcode::LeastSignificantWord => fold_least_significant_word(block, inst_ref),
            Opcode::LeastSignificantHalf => fold_least_significant_half(block, inst_ref),
            Opcode::LeastSignificantByte => fold_least_significant_byte(block, inst_ref),
            Opcode::MostSignificantBit => fold_most_significant_bit(block, inst_ref),

            // --- Zero tests ---
            Opcode::IsZero32 => {
                if all_args_immediate(block, inst_ref) {
                    let v = block.instructions[i].args[0].get_u32();
                    replace_with(block, inst_ref, Value::ImmU1(v == 0));
                }
            }
            Opcode::IsZero64 => {
                if all_args_immediate(block, inst_ref) {
                    let v = block.instructions[i].args[0].get_u64();
                    replace_with(block, inst_ref, Value::ImmU1(v == 0));
                }
            }

            // --- Shifts (masked) ---
            Opcode::LogicalShiftLeftMasked32 => {
                if all_args_immediate(block, inst_ref) {
                    let a = block.instructions[i].args[0].get_u32();
                    let b = block.instructions[i].args[1].get_u32();
                    replace_with(block, inst_ref, Value::ImmU32(a << (b & 0x1f)));
                }
            }
            Opcode::LogicalShiftLeftMasked64 => {
                if all_args_immediate(block, inst_ref) {
                    let a = block.instructions[i].args[0].get_u64();
                    let b = block.instructions[i].args[1].get_u64();
                    replace_with(block, inst_ref, Value::ImmU64(a << (b & 0x3f)));
                }
            }
            Opcode::LogicalShiftRightMasked32 => {
                if all_args_immediate(block, inst_ref) {
                    let a = block.instructions[i].args[0].get_u32();
                    let b = block.instructions[i].args[1].get_u32();
                    replace_with(block, inst_ref, Value::ImmU32(a >> (b & 0x1f)));
                }
            }
            Opcode::LogicalShiftRightMasked64 => {
                if all_args_immediate(block, inst_ref) {
                    let a = block.instructions[i].args[0].get_u64();
                    let b = block.instructions[i].args[1].get_u64();
                    replace_with(block, inst_ref, Value::ImmU64(a >> (b & 0x3f)));
                }
            }
            Opcode::ArithmeticShiftRightMasked32 => {
                if all_args_immediate(block, inst_ref) {
                    let a = block.instructions[i].args[0].get_u32() as i32;
                    let b = block.instructions[i].args[1].get_u32();
                    replace_with(block, inst_ref, Value::ImmU32((a >> (b & 0x1f)) as u32));
                }
            }
            Opcode::ArithmeticShiftRightMasked64 => {
                if all_args_immediate(block, inst_ref) {
                    let a = block.instructions[i].args[0].get_u64() as i64;
                    let b = block.instructions[i].args[1].get_u64();
                    replace_with(block, inst_ref, Value::ImmU64((a >> (b & 0x3f)) as u64));
                }
            }
            Opcode::RotateRightMasked32 => {
                if all_args_immediate(block, inst_ref) {
                    let a = block.instructions[i].args[0].get_u32();
                    let b = block.instructions[i].args[1].get_u32();
                    replace_with(block, inst_ref, Value::ImmU32(a.rotate_right(b)));
                }
            }
            Opcode::RotateRightMasked64 => {
                if all_args_immediate(block, inst_ref) {
                    let a = block.instructions[i].args[0].get_u64();
                    let b = block.instructions[i].args[1].get_u64();
                    replace_with(block, inst_ref, Value::ImmU64(a.rotate_right(b as u32)));
                }
            }

            // --- Shifts (non-masked, with carry) ---
            Opcode::LogicalShiftLeft32 | Opcode::LogicalShiftLeft64 => {
                if fold_shift_zero(block, inst_ref) && all_args_immediate(block, inst_ref) {
                    let is_32 = opcode == Opcode::LogicalShiftLeft32;
                    let a = block.instructions[i].args[0].get_imm_as_u64();
                    let b = block.instructions[i].args[1].get_u8();
                    let result = safe_lsl(a, b, is_32);
                    replace_with_sized(block, inst_ref, is_32, result);
                }
            }
            Opcode::LogicalShiftRight32 | Opcode::LogicalShiftRight64 => {
                if fold_shift_zero(block, inst_ref) && all_args_immediate(block, inst_ref) {
                    let is_32 = opcode == Opcode::LogicalShiftRight32;
                    let a = block.instructions[i].args[0].get_imm_as_u64();
                    let b = block.instructions[i].args[1].get_u8();
                    let result = safe_lsr(a, b, is_32);
                    replace_with_sized(block, inst_ref, is_32, result);
                }
            }
            Opcode::ArithmeticShiftRight32 | Opcode::ArithmeticShiftRight64 => {
                if fold_shift_zero(block, inst_ref) && all_args_immediate(block, inst_ref) {
                    let is_32 = opcode == Opcode::ArithmeticShiftRight32;
                    let a = block.instructions[i].args[0].get_imm_as_s64();
                    let b = block.instructions[i].args[1].get_u8();
                    let result = safe_asr(a, b, is_32);
                    replace_with_sized(block, inst_ref, is_32, result as u64);
                }
            }
            Opcode::BitRotateRight32 | Opcode::BitRotateRight64 => {
                if fold_shift_zero(block, inst_ref) && all_args_immediate(block, inst_ref) {
                    let is_32 = opcode == Opcode::BitRotateRight32;
                    let a = block.instructions[i].args[0].get_imm_as_u64();
                    let b = block.instructions[i].args[1].get_u8();
                    let result = if is_32 {
                        (a as u32).rotate_right(b as u32) as u64
                    } else {
                        a.rotate_right(b as u32)
                    };
                    replace_with_sized(block, inst_ref, is_32, result);
                }
            }

            // --- Add/Sub ---
            Opcode::Add32 | Opcode::Add64 => fold_add(block, inst_ref, opcode == Opcode::Add32),
            Opcode::Sub32 | Opcode::Sub64 => fold_sub(block, inst_ref, opcode == Opcode::Sub32),

            // --- Multiply ---
            Opcode::Mul32 | Opcode::Mul64 => {
                fold_multiply(block, inst_ref, opcode == Opcode::Mul32)
            }

            // --- Divide ---
            Opcode::SignedDiv32 | Opcode::SignedDiv64 => {
                fold_divide(block, inst_ref, opcode == Opcode::SignedDiv32, true);
            }
            Opcode::UnsignedDiv32 | Opcode::UnsignedDiv64 => {
                fold_divide(block, inst_ref, opcode == Opcode::UnsignedDiv32, false);
            }

            // --- Bitwise ---
            Opcode::And32 | Opcode::And64 => fold_and(block, inst_ref, opcode == Opcode::And32),
            Opcode::Eor32 | Opcode::Eor64 => fold_eor(block, inst_ref, opcode == Opcode::Eor32),
            Opcode::Or32 | Opcode::Or64 => fold_or(block, inst_ref, opcode == Opcode::Or32),
            Opcode::Not32 | Opcode::Not64 => fold_not(block, inst_ref, opcode == Opcode::Not32),

            // --- Sign/Zero Extends ---
            Opcode::SignExtendByteToWord | Opcode::SignExtendHalfToWord => {
                if all_args_immediate(block, inst_ref) {
                    let v = block.instructions[i].args[0].get_imm_as_s64();
                    replace_with(block, inst_ref, Value::ImmU32(v as u32));
                }
            }
            Opcode::SignExtendByteToLong
            | Opcode::SignExtendHalfToLong
            | Opcode::SignExtendWordToLong => {
                if all_args_immediate(block, inst_ref) {
                    let v = block.instructions[i].args[0].get_imm_as_s64();
                    replace_with(block, inst_ref, Value::ImmU64(v as u64));
                }
            }
            Opcode::ZeroExtendByteToWord | Opcode::ZeroExtendHalfToWord => {
                if all_args_immediate(block, inst_ref) {
                    let v = block.instructions[i].args[0].get_imm_as_u64();
                    replace_with(block, inst_ref, Value::ImmU32(v as u32));
                }
            }
            Opcode::ZeroExtendByteToLong
            | Opcode::ZeroExtendHalfToLong
            | Opcode::ZeroExtendWordToLong => {
                if all_args_immediate(block, inst_ref) {
                    let v = block.instructions[i].args[0].get_imm_as_u64();
                    replace_with(block, inst_ref, Value::ImmU64(v));
                }
            }

            // --- Byte reverse / bit reverse ---
            Opcode::ByteReverseWord => {
                if all_args_immediate(block, inst_ref) {
                    let v = block.instructions[i].args[0].get_imm_as_u64() as u32;
                    replace_with(block, inst_ref, Value::ImmU32(v.swap_bytes()));
                }
            }
            Opcode::ByteReverseHalf => {
                if all_args_immediate(block, inst_ref) {
                    let v = block.instructions[i].args[0].get_imm_as_u64() as u16;
                    replace_with(block, inst_ref, Value::ImmU16(v.swap_bytes()));
                }
            }
            Opcode::ByteReverseDual => {
                if all_args_immediate(block, inst_ref) {
                    let v = block.instructions[i].args[0].get_imm_as_u64();
                    replace_with(block, inst_ref, Value::ImmU64(v.swap_bytes()));
                }
            }

            _ => {}
        }
    }
}

// --- Helper functions ---

fn all_args_immediate(block: &Block, inst_ref: InstRef) -> bool {
    let inst = &block.instructions[inst_ref.index()];
    let n = inst.num_args();
    (0..n).all(|i| inst.args[i].is_immediate())
}

fn replace_with(block: &mut Block, inst_ref: InstRef, value: Value) {
    block.replace_uses_with(inst_ref, value);
}

fn replace_with_sized(block: &mut Block, inst_ref: InstRef, is_32: bool, value: u64) {
    let v = if is_32 {
        Value::ImmU32(value as u32)
    } else {
        Value::ImmU64(value)
    };
    replace_with(block, inst_ref, v);
}

/// Safe logical shift left: returns 0 if shift >= bitwidth.
fn safe_lsl(value: u64, shift: u8, is_32: bool) -> u64 {
    let bits = if is_32 { 32 } else { 64 };
    if shift as u32 >= bits {
        0
    } else if is_32 {
        ((value as u32) << shift) as u64
    } else {
        value << shift
    }
}

/// Safe logical shift right: returns 0 if shift >= bitwidth.
fn safe_lsr(value: u64, shift: u8, is_32: bool) -> u64 {
    let bits = if is_32 { 32 } else { 64 };
    if shift as u32 >= bits {
        0
    } else if is_32 {
        ((value as u32) >> shift) as u64
    } else {
        value >> shift
    }
}

/// Safe arithmetic shift right: sign-extends if shift >= bitwidth.
fn safe_asr(value: i64, shift: u8, is_32: bool) -> i64 {
    let bits = if is_32 { 32 } else { 64 };
    if is_32 {
        let v = value as i32;
        if shift as u32 >= bits {
            v >> 31
        } else {
            v >> shift
        }
        .into()
    } else if shift as u32 >= bits {
        value >> 63
    } else {
        value >> shift
    }
}

/// Fold shift-by-zero: if shift amount is zero, replace with the input value.
/// Returns true if the instruction should still be processed for full constant folding.
fn fold_shift_zero(block: &mut Block, inst_ref: InstRef) -> bool {
    let i = inst_ref.index();
    let shift_amount = block.instructions[i].args[1];

    // Clear carry_in if no pseudo-op uses it
    if block.instructions[i].num_args() >= 3 {
        block.set_arg(inst_ref, 2, Value::ImmU1(false));
    }

    if shift_amount.is_zero() {
        let input = block.instructions[i].args[0];
        replace_with(block, inst_ref, input);
        return false;
    }

    !block.instructions[i].is_tombstone() && all_args_immediate(block, inst_ref)
}

fn fold_add(block: &mut Block, inst_ref: InstRef, is_32: bool) {
    let i = inst_ref.index();
    let lhs = block.instructions[i].args[0];
    let rhs = block.instructions[i].args[1];
    let carry = block.instructions[i].args[2];

    // Normalize: if lhs is immediate and rhs isn't, swap
    if lhs.is_immediate() && !rhs.is_immediate() {
        block.set_arg(inst_ref, 0, rhs);
        block.set_arg(inst_ref, 1, lhs);
        fold_add(block, inst_ref, is_32);
        return;
    }

    // x + 0 + 0 = x
    if !lhs.is_immediate() && rhs.is_zero() && carry.is_zero() {
        replace_with(block, inst_ref, lhs);
        return;
    }

    // Fold all immediates
    if lhs.is_immediate() && rhs.is_immediate() && carry.is_immediate() {
        let result = lhs
            .get_imm_as_u64()
            .wrapping_add(rhs.get_imm_as_u64())
            .wrapping_add(carry.get_u1() as u64);
        replace_with_sized(block, inst_ref, is_32, result);
    }
}

fn fold_sub(block: &mut Block, inst_ref: InstRef, is_32: bool) {
    let i = inst_ref.index();
    if !all_args_immediate(block, inst_ref) {
        return;
    }

    let lhs = block.instructions[i].args[0].get_imm_as_u64();
    let rhs = block.instructions[i].args[1].get_imm_as_u64();
    let carry = block.instructions[i].args[2].get_u1();

    let result = lhs.wrapping_add(!rhs).wrapping_add(carry as u64);
    replace_with_sized(block, inst_ref, is_32, result);
}

fn fold_multiply(block: &mut Block, inst_ref: InstRef, is_32: bool) {
    let i = inst_ref.index();
    let lhs = block.instructions[i].args[0];
    let rhs = block.instructions[i].args[1];

    // x * 0 = 0, 0 * x = 0
    if lhs.is_zero() || rhs.is_zero() {
        replace_with_sized(block, inst_ref, is_32, 0);
        return;
    }

    // x * 1 = x
    if rhs.is_unsigned_imm(1) {
        replace_with(block, inst_ref, lhs);
        return;
    }
    if lhs.is_unsigned_imm(1) {
        replace_with(block, inst_ref, rhs);
        return;
    }

    if lhs.is_immediate() && rhs.is_immediate() {
        let result = lhs.get_imm_as_u64().wrapping_mul(rhs.get_imm_as_u64());
        replace_with_sized(block, inst_ref, is_32, result);
    }
}

fn fold_divide(block: &mut Block, inst_ref: InstRef, is_32: bool, is_signed: bool) {
    let i = inst_ref.index();
    let rhs = block.instructions[i].args[1];

    // x / 0 = 0 (ARM-defined behavior)
    if rhs.is_zero() {
        replace_with_sized(block, inst_ref, is_32, 0);
        return;
    }

    let lhs = block.instructions[i].args[0];

    // x / 1 = x
    if rhs.is_unsigned_imm(1) {
        replace_with(block, inst_ref, lhs);
        return;
    }

    if lhs.is_immediate() && rhs.is_immediate() {
        let result = if is_signed {
            let a = lhs.get_imm_as_s64();
            let b = rhs.get_imm_as_s64();
            if b == 0 {
                0
            } else {
                (a / b) as u64
            }
        } else {
            let a = lhs.get_imm_as_u64();
            let b = rhs.get_imm_as_u64();
            if b == 0 {
                0
            } else {
                a / b
            }
        };
        replace_with_sized(block, inst_ref, is_32, result);
    }
}

fn fold_and(block: &mut Block, inst_ref: InstRef, is_32: bool) {
    let i = inst_ref.index();
    let lhs = block.instructions[i].args[0];
    let rhs = block.instructions[i].args[1];

    // x & 0 = 0
    if lhs.is_zero() || rhs.is_zero() {
        replace_with_sized(block, inst_ref, is_32, 0);
        return;
    }

    // x & all_ones = x
    if rhs.has_all_bits_set() {
        replace_with(block, inst_ref, lhs);
        return;
    }
    if lhs.has_all_bits_set() {
        replace_with(block, inst_ref, rhs);
        return;
    }

    if lhs.is_immediate() && rhs.is_immediate() {
        let result = lhs.get_imm_as_u64() & rhs.get_imm_as_u64();
        replace_with_sized(block, inst_ref, is_32, result);
    }
}

fn fold_eor(block: &mut Block, inst_ref: InstRef, is_32: bool) {
    let i = inst_ref.index();
    let lhs = block.instructions[i].args[0];
    let rhs = block.instructions[i].args[1];

    // x ^ 0 = x
    if rhs.is_zero() {
        replace_with(block, inst_ref, lhs);
        return;
    }
    if lhs.is_zero() {
        replace_with(block, inst_ref, rhs);
        return;
    }

    if lhs.is_immediate() && rhs.is_immediate() {
        let result = lhs.get_imm_as_u64() ^ rhs.get_imm_as_u64();
        replace_with_sized(block, inst_ref, is_32, result);
    }
}

fn fold_or(block: &mut Block, inst_ref: InstRef, is_32: bool) {
    let i = inst_ref.index();
    let lhs = block.instructions[i].args[0];
    let rhs = block.instructions[i].args[1];

    // x | 0 = x
    if rhs.is_zero() {
        replace_with(block, inst_ref, lhs);
        return;
    }
    if lhs.is_zero() {
        replace_with(block, inst_ref, rhs);
        return;
    }

    if lhs.is_immediate() && rhs.is_immediate() {
        let result = lhs.get_imm_as_u64() | rhs.get_imm_as_u64();
        replace_with_sized(block, inst_ref, is_32, result);
    }
}

fn fold_not(block: &mut Block, inst_ref: InstRef, is_32: bool) {
    let i = inst_ref.index();
    let operand = block.instructions[i].args[0];
    if operand.is_immediate() {
        let result = !operand.get_imm_as_u64();
        replace_with_sized(block, inst_ref, is_32, result);
    }
}

fn fold_least_significant_word(block: &mut Block, inst_ref: InstRef) {
    if all_args_immediate(block, inst_ref) {
        let v = block.instructions[inst_ref.index()].args[0].get_imm_as_u64();
        replace_with(block, inst_ref, Value::ImmU32(v as u32));
    }
}

fn fold_least_significant_half(block: &mut Block, inst_ref: InstRef) {
    if all_args_immediate(block, inst_ref) {
        let v = block.instructions[inst_ref.index()].args[0].get_imm_as_u64();
        replace_with(block, inst_ref, Value::ImmU16(v as u16));
    }
}

fn fold_least_significant_byte(block: &mut Block, inst_ref: InstRef) {
    if all_args_immediate(block, inst_ref) {
        let v = block.instructions[inst_ref.index()].args[0].get_imm_as_u64();
        replace_with(block, inst_ref, Value::ImmU8(v as u8));
    }
}

fn fold_most_significant_bit(block: &mut Block, inst_ref: InstRef) {
    if all_args_immediate(block, inst_ref) {
        let v = block.instructions[inst_ref.index()].args[0].get_imm_as_u64();
        replace_with(block, inst_ref, Value::ImmU1((v >> 31) != 0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::block::Block;
    use crate::ir::location::LocationDescriptor;
    use crate::ir::opt::identity_removal::identity_removal;

    #[test]
    fn test_fold_add_immediates() {
        let mut block = Block::new(LocationDescriptor(0));
        let add = block.append(
            Opcode::Add32,
            &[Value::ImmU32(5), Value::ImmU32(3), Value::ImmU1(false)],
        );
        // Use the add result so it doesn't get DCE'd prematurely
        block.append(
            Opcode::A64SetX,
            &[
                Value::ImmA64Reg(crate::frontend::a64::types::Reg::R1),
                Value::Inst(add),
            ],
        );

        constant_propagation(&mut block);
        identity_removal(&mut block);

        // The add should have been replaced with ImmU32(8) in the SetX's arg
        let set_inst = &block.instructions[1];
        assert_eq!(set_inst.args[1], Value::ImmU32(8));
    }

    #[test]
    fn test_fold_add_zero() {
        let mut block = Block::new(LocationDescriptor(0));
        let get_x = block.append(
            Opcode::A64GetX,
            &[Value::ImmA64Reg(crate::frontend::a64::types::Reg::R2)],
        );
        let add = block.append(
            Opcode::Add64,
            &[Value::Inst(get_x), Value::ImmU64(0), Value::ImmU1(false)],
        );
        block.append(
            Opcode::A64SetX,
            &[
                Value::ImmA64Reg(crate::frontend::a64::types::Reg::R1),
                Value::Inst(add),
            ],
        );

        constant_propagation(&mut block);
        identity_removal(&mut block);

        // x + 0 = x, so SetX should reference GetX directly
        let set_inst = &block.instructions[2];
        assert_eq!(set_inst.args[1], Value::Inst(get_x));
    }

    #[test]
    fn test_fold_and_zero() {
        let mut block = Block::new(LocationDescriptor(0));
        let get_x = block.append(
            Opcode::A64GetX,
            &[Value::ImmA64Reg(crate::frontend::a64::types::Reg::R2)],
        );
        let and = block.append(Opcode::And32, &[Value::Inst(get_x), Value::ImmU32(0)]);
        block.append(
            Opcode::A64SetX,
            &[
                Value::ImmA64Reg(crate::frontend::a64::types::Reg::R1),
                Value::Inst(and),
            ],
        );

        constant_propagation(&mut block);
        identity_removal(&mut block);

        // x & 0 = 0
        let set_inst = &block.instructions[2];
        assert_eq!(set_inst.args[1], Value::ImmU32(0));
    }

    #[test]
    fn test_fold_mul_zero() {
        let mut block = Block::new(LocationDescriptor(0));
        let get_x = block.append(
            Opcode::A64GetX,
            &[Value::ImmA64Reg(crate::frontend::a64::types::Reg::R2)],
        );
        let mul = block.append(Opcode::Mul32, &[Value::Inst(get_x), Value::ImmU32(0)]);
        block.append(
            Opcode::A64SetX,
            &[
                Value::ImmA64Reg(crate::frontend::a64::types::Reg::R1),
                Value::Inst(mul),
            ],
        );

        constant_propagation(&mut block);
        identity_removal(&mut block);

        let set_inst = &block.instructions[2];
        assert_eq!(set_inst.args[1], Value::ImmU32(0));
    }

    #[test]
    fn test_fold_eor_zero() {
        let mut block = Block::new(LocationDescriptor(0));
        let get_x = block.append(
            Opcode::A64GetX,
            &[Value::ImmA64Reg(crate::frontend::a64::types::Reg::R2)],
        );
        let eor = block.append(Opcode::Eor64, &[Value::Inst(get_x), Value::ImmU64(0)]);
        block.append(
            Opcode::A64SetX,
            &[
                Value::ImmA64Reg(crate::frontend::a64::types::Reg::R1),
                Value::Inst(eor),
            ],
        );

        constant_propagation(&mut block);
        identity_removal(&mut block);

        // x ^ 0 = x
        let set_inst = &block.instructions[2];
        assert_eq!(set_inst.args[1], Value::Inst(get_x));
    }

    #[test]
    fn test_fold_not_immediate() {
        let mut block = Block::new(LocationDescriptor(0));
        let not = block.append(Opcode::Not32, &[Value::ImmU32(0xFF00_00FF)]);
        block.append(
            Opcode::A64SetX,
            &[
                Value::ImmA64Reg(crate::frontend::a64::types::Reg::R1),
                Value::Inst(not),
            ],
        );

        constant_propagation(&mut block);
        identity_removal(&mut block);

        let set_inst = &block.instructions[1];
        assert_eq!(set_inst.args[1], Value::ImmU32(0x00FF_FF00));
    }

    #[test]
    fn test_fold_byte_reverse() {
        let mut block = Block::new(LocationDescriptor(0));
        let rev = block.append(Opcode::ByteReverseWord, &[Value::ImmU32(0x01020304)]);
        block.append(
            Opcode::A64SetX,
            &[
                Value::ImmA64Reg(crate::frontend::a64::types::Reg::R1),
                Value::Inst(rev),
            ],
        );

        constant_propagation(&mut block);
        identity_removal(&mut block);

        let set_inst = &block.instructions[1];
        assert_eq!(set_inst.args[1], Value::ImmU32(0x04030201));
    }

    #[test]
    fn test_fold_div_by_zero() {
        let mut block = Block::new(LocationDescriptor(0));
        let get_x = block.append(
            Opcode::A64GetX,
            &[Value::ImmA64Reg(crate::frontend::a64::types::Reg::R2)],
        );
        let div = block.append(
            Opcode::UnsignedDiv32,
            &[Value::Inst(get_x), Value::ImmU32(0)],
        );
        block.append(
            Opcode::A64SetX,
            &[
                Value::ImmA64Reg(crate::frontend::a64::types::Reg::R1),
                Value::Inst(div),
            ],
        );

        constant_propagation(&mut block);
        identity_removal(&mut block);

        // x / 0 = 0 on ARM
        let set_inst = &block.instructions[2];
        assert_eq!(set_inst.args[1], Value::ImmU32(0));
    }

    #[test]
    fn test_fold_sign_extend() {
        let mut block = Block::new(LocationDescriptor(0));
        let ext = block.append(Opcode::SignExtendByteToWord, &[Value::ImmU8(0x80)]);
        block.append(
            Opcode::A64SetX,
            &[
                Value::ImmA64Reg(crate::frontend::a64::types::Reg::R1),
                Value::Inst(ext),
            ],
        );

        constant_propagation(&mut block);
        identity_removal(&mut block);

        let set_inst = &block.instructions[1];
        // 0x80 as i8 = -128, sign-extended to u32 = 0xFFFFFF80
        assert_eq!(set_inst.args[1], Value::ImmU32(0xFFFF_FF80));
    }
}
