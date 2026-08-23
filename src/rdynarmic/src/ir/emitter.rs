use crate::ir::block::Block;
use crate::ir::location::LocationDescriptor;
use crate::ir::opcode::Opcode;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResultAndOverflow {
    pub result: Value,
    pub overflow: Value,
}

/// Base IR emitter — the builder API for constructing IR blocks.
/// Wraps a Block and appends instructions to it.
pub struct IREmitter<'a> {
    pub block: &'a mut Block,
}

impl<'a> IREmitter<'a> {
    pub fn new(block: &'a mut Block) -> Self {
        Self { block }
    }

    /// Internal: emit an instruction and return its Value as an InstRef.
    fn emit(&mut self, opcode: Opcode, args: &[Value]) -> Value {
        let r = self.block.append(opcode, args);
        Value::Inst(r)
    }

    /// Internal: emit an instruction returning void (no result value).
    fn emit_void(&mut self, opcode: Opcode, args: &[Value]) {
        self.block.append(opcode, args);
    }

    /// Set the block terminal.
    pub fn set_term(&mut self, terminal: Terminal) {
        self.block.set_terminal(terminal);
    }

    // --- Immediates ---

    pub fn imm1(&self, value: bool) -> Value {
        Value::ImmU1(value)
    }
    pub fn imm8(&self, value: u8) -> Value {
        Value::ImmU8(value)
    }
    pub fn imm16(&self, value: u16) -> Value {
        Value::ImmU16(value)
    }
    pub fn imm32(&self, value: u32) -> Value {
        Value::ImmU32(value)
    }
    pub fn imm64(&self, value: u64) -> Value {
        Value::ImmU64(value)
    }

    // --- Pack/Extract ---

    pub fn pack_2x32_to_1x64(&mut self, lo: Value, hi: Value) -> Value {
        self.emit(Opcode::Pack2x32To1x64, &[lo, hi])
    }

    pub fn pack_2x64_to_1x128(&mut self, lo: Value, hi: Value) -> Value {
        self.emit(Opcode::Pack2x64To1x128, &[lo, hi])
    }

    pub fn least_significant_word(&mut self, value: Value) -> Value {
        self.emit(Opcode::LeastSignificantWord, &[value])
    }

    pub fn most_significant_word(&mut self, value: Value) -> Value {
        self.emit(Opcode::MostSignificantWord, &[value])
    }

    pub fn least_significant_half(&mut self, value: Value) -> Value {
        self.emit(Opcode::LeastSignificantHalf, &[value])
    }

    pub fn least_significant_byte(&mut self, value: Value) -> Value {
        self.emit(Opcode::LeastSignificantByte, &[value])
    }

    pub fn most_significant_bit(&mut self, value: Value) -> Value {
        self.emit(Opcode::MostSignificantBit, &[value])
    }

    pub fn is_zero_32(&mut self, value: Value) -> Value {
        self.emit(Opcode::IsZero32, &[value])
    }

    pub fn is_zero_64(&mut self, value: Value) -> Value {
        self.emit(Opcode::IsZero64, &[value])
    }

    pub fn test_bit(&mut self, value: Value, bit: Value) -> Value {
        self.emit(Opcode::TestBit, &[value, bit])
    }

    // --- Conditional select ---

    pub fn conditional_select_32(
        &mut self,
        cond: Value,
        then_val: Value,
        else_val: Value,
    ) -> Value {
        self.emit(Opcode::ConditionalSelect32, &[cond, then_val, else_val])
    }

    pub fn conditional_select_64(
        &mut self,
        cond: Value,
        then_val: Value,
        else_val: Value,
    ) -> Value {
        self.emit(Opcode::ConditionalSelect64, &[cond, then_val, else_val])
    }

    pub fn conditional_select_nzcv(
        &mut self,
        cond: Value,
        then_val: Value,
        else_val: Value,
    ) -> Value {
        self.emit(Opcode::ConditionalSelectNZCV, &[cond, then_val, else_val])
    }

    // --- Shifts (32-bit with carry) ---

    pub fn logical_shift_left_32(&mut self, value: Value, shift: Value, carry_in: Value) -> Value {
        self.emit(Opcode::LogicalShiftLeft32, &[value, shift, carry_in])
    }

    pub fn logical_shift_left_64(&mut self, value: Value, shift: Value) -> Value {
        self.emit(Opcode::LogicalShiftLeft64, &[value, shift])
    }

    pub fn logical_shift_right_32(&mut self, value: Value, shift: Value, carry_in: Value) -> Value {
        self.emit(Opcode::LogicalShiftRight32, &[value, shift, carry_in])
    }

    pub fn logical_shift_right_64(&mut self, value: Value, shift: Value) -> Value {
        self.emit(Opcode::LogicalShiftRight64, &[value, shift])
    }

    pub fn arithmetic_shift_right_32(
        &mut self,
        value: Value,
        shift: Value,
        carry_in: Value,
    ) -> Value {
        self.emit(Opcode::ArithmeticShiftRight32, &[value, shift, carry_in])
    }

    pub fn arithmetic_shift_right_64(&mut self, value: Value, shift: Value) -> Value {
        self.emit(Opcode::ArithmeticShiftRight64, &[value, shift])
    }

    pub fn rotate_right_32(&mut self, value: Value, shift: Value, carry_in: Value) -> Value {
        self.emit(Opcode::BitRotateRight32, &[value, shift, carry_in])
    }

    pub fn rotate_right_64(&mut self, value: Value, shift: Value) -> Value {
        self.emit(Opcode::BitRotateRight64, &[value, shift])
    }

    pub fn rotate_right_extended(&mut self, value: Value, carry_in: Value) -> Value {
        self.emit(Opcode::RotateRightExtended, &[value, carry_in])
    }

    // --- Masked shifts (shift amount taken from register, auto-masked) ---

    pub fn logical_shift_left_masked_32(&mut self, value: Value, shift: Value) -> Value {
        self.emit(Opcode::LogicalShiftLeftMasked32, &[value, shift])
    }

    pub fn logical_shift_left_masked_64(&mut self, value: Value, shift: Value) -> Value {
        self.emit(Opcode::LogicalShiftLeftMasked64, &[value, shift])
    }

    pub fn logical_shift_right_masked_32(&mut self, value: Value, shift: Value) -> Value {
        self.emit(Opcode::LogicalShiftRightMasked32, &[value, shift])
    }

    pub fn logical_shift_right_masked_64(&mut self, value: Value, shift: Value) -> Value {
        self.emit(Opcode::LogicalShiftRightMasked64, &[value, shift])
    }

    pub fn arithmetic_shift_right_masked_32(&mut self, value: Value, shift: Value) -> Value {
        self.emit(Opcode::ArithmeticShiftRightMasked32, &[value, shift])
    }

    pub fn arithmetic_shift_right_masked_64(&mut self, value: Value, shift: Value) -> Value {
        self.emit(Opcode::ArithmeticShiftRightMasked64, &[value, shift])
    }

    pub fn rotate_right_masked_32(&mut self, value: Value, shift: Value) -> Value {
        self.emit(Opcode::RotateRightMasked32, &[value, shift])
    }

    pub fn rotate_right_masked_64(&mut self, value: Value, shift: Value) -> Value {
        self.emit(Opcode::RotateRightMasked64, &[value, shift])
    }

    // --- ALU ---

    pub fn add_32(&mut self, a: Value, b: Value, carry_in: Value) -> Value {
        self.emit(Opcode::Add32, &[a, b, carry_in])
    }

    pub fn add_64(&mut self, a: Value, b: Value, carry_in: Value) -> Value {
        self.emit(Opcode::Add64, &[a, b, carry_in])
    }

    pub fn sub_32(&mut self, a: Value, b: Value, carry_in: Value) -> Value {
        self.emit(Opcode::Sub32, &[a, b, carry_in])
    }

    pub fn sub_64(&mut self, a: Value, b: Value, carry_in: Value) -> Value {
        self.emit(Opcode::Sub64, &[a, b, carry_in])
    }

    pub fn mul_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::Mul32, &[a, b])
    }

    pub fn mul_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::Mul64, &[a, b])
    }

    pub fn signed_multiply_high_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::SignedMultiplyHigh64, &[a, b])
    }

    pub fn unsigned_multiply_high_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::UnsignedMultiplyHigh64, &[a, b])
    }

    pub fn unsigned_div_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::UnsignedDiv32, &[a, b])
    }

    pub fn unsigned_div_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::UnsignedDiv64, &[a, b])
    }

    pub fn signed_div_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::SignedDiv32, &[a, b])
    }

    pub fn signed_div_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::SignedDiv64, &[a, b])
    }

    // --- Logic ---

    pub fn and_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::And32, &[a, b])
    }

    pub fn and_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::And64, &[a, b])
    }

    pub fn and_not_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::AndNot32, &[a, b])
    }

    pub fn and_not_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::AndNot64, &[a, b])
    }

    pub fn eor_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::Eor32, &[a, b])
    }

    pub fn eor_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::Eor64, &[a, b])
    }

    pub fn or_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::Or32, &[a, b])
    }

    pub fn or_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::Or64, &[a, b])
    }

    pub fn not_32(&mut self, a: Value) -> Value {
        self.emit(Opcode::Not32, &[a])
    }

    pub fn not_64(&mut self, a: Value) -> Value {
        self.emit(Opcode::Not64, &[a])
    }

    // --- Extensions ---

    pub fn sign_extend_byte_to_word(&mut self, a: Value) -> Value {
        self.emit(Opcode::SignExtendByteToWord, &[a])
    }

    pub fn sign_extend_half_to_word(&mut self, a: Value) -> Value {
        self.emit(Opcode::SignExtendHalfToWord, &[a])
    }

    pub fn sign_extend_byte_to_long(&mut self, a: Value) -> Value {
        self.emit(Opcode::SignExtendByteToLong, &[a])
    }

    pub fn sign_extend_half_to_long(&mut self, a: Value) -> Value {
        self.emit(Opcode::SignExtendHalfToLong, &[a])
    }

    pub fn sign_extend_word_to_long(&mut self, a: Value) -> Value {
        self.emit(Opcode::SignExtendWordToLong, &[a])
    }

    pub fn zero_extend_byte_to_word(&mut self, a: Value) -> Value {
        self.emit(Opcode::ZeroExtendByteToWord, &[a])
    }

    pub fn zero_extend_half_to_word(&mut self, a: Value) -> Value {
        self.emit(Opcode::ZeroExtendHalfToWord, &[a])
    }

    pub fn zero_extend_byte_to_long(&mut self, a: Value) -> Value {
        self.emit(Opcode::ZeroExtendByteToLong, &[a])
    }

    pub fn zero_extend_half_to_long(&mut self, a: Value) -> Value {
        self.emit(Opcode::ZeroExtendHalfToLong, &[a])
    }

    pub fn zero_extend_word_to_long(&mut self, a: Value) -> Value {
        self.emit(Opcode::ZeroExtendWordToLong, &[a])
    }

    pub fn zero_extend_long_to_quad(&mut self, a: Value) -> Value {
        self.emit(Opcode::ZeroExtendLongToQuad, &[a])
    }

    // --- Byte reverse ---

    pub fn byte_reverse_word(&mut self, a: Value) -> Value {
        self.emit(Opcode::ByteReverseWord, &[a])
    }

    pub fn byte_reverse_half(&mut self, a: Value) -> Value {
        self.emit(Opcode::ByteReverseHalf, &[a])
    }

    pub fn byte_reverse_dual(&mut self, a: Value) -> Value {
        self.emit(Opcode::ByteReverseDual, &[a])
    }

    // --- Count/Extract ---

    pub fn count_leading_zeros_32(&mut self, a: Value) -> Value {
        self.emit(Opcode::CountLeadingZeros32, &[a])
    }

    pub fn count_leading_zeros_64(&mut self, a: Value) -> Value {
        self.emit(Opcode::CountLeadingZeros64, &[a])
    }

    pub fn extract_register_32(&mut self, a: Value, b: Value, lsb: Value) -> Value {
        self.emit(Opcode::ExtractRegister32, &[a, b, lsb])
    }

    pub fn extract_register_64(&mut self, a: Value, b: Value, lsb: Value) -> Value {
        self.emit(Opcode::ExtractRegister64, &[a, b, lsb])
    }

    pub fn replicate_bit_32(&mut self, a: Value, bit: Value) -> Value {
        self.emit(Opcode::ReplicateBit32, &[a, bit])
    }

    pub fn replicate_bit_64(&mut self, a: Value, bit: Value) -> Value {
        self.emit(Opcode::ReplicateBit64, &[a, bit])
    }

    // --- Saturated arithmetic ---

    pub fn signed_saturated_add_with_flag(&mut self, a: Value, b: Value) -> ResultAndOverflow {
        let result = self.emit(Opcode::SignedSaturatedAddWithFlag32, &[a, b]);
        let overflow = self.get_overflow_from_op(result);
        ResultAndOverflow { result, overflow }
    }

    pub fn signed_saturated_sub_with_flag(&mut self, a: Value, b: Value) -> ResultAndOverflow {
        let result = self.emit(Opcode::SignedSaturatedSubWithFlag32, &[a, b]);
        let overflow = self.get_overflow_from_op(result);
        ResultAndOverflow { result, overflow }
    }

    pub fn signed_saturation(
        &mut self,
        a: Value,
        bit_size_to_saturate_to: usize,
    ) -> ResultAndOverflow {
        assert!((1..=32).contains(&bit_size_to_saturate_to));
        let result = self.emit(
            Opcode::SignedSaturation,
            &[a, Value::ImmU8(bit_size_to_saturate_to as u8)],
        );
        let overflow = self.get_overflow_from_op(result);
        ResultAndOverflow { result, overflow }
    }

    pub fn unsigned_saturation(
        &mut self,
        a: Value,
        bit_size_to_saturate_to: usize,
    ) -> ResultAndOverflow {
        assert!(bit_size_to_saturate_to <= 31);
        let result = self.emit(
            Opcode::UnsignedSaturation,
            &[a, Value::ImmU8(bit_size_to_saturate_to as u8)],
        );
        let overflow = self.get_overflow_from_op(result);
        ResultAndOverflow { result, overflow }
    }

    // --- Flags ---

    pub fn get_carry_from_op(&mut self, value: Value) -> Value {
        self.emit_pseudo_op(Opcode::GetCarryFromOp, value)
    }

    pub fn get_overflow_from_op(&mut self, value: Value) -> Value {
        self.emit_pseudo_op(Opcode::GetOverflowFromOp, value)
    }

    pub fn get_nzcv_from_op(&mut self, value: Value) -> Value {
        self.emit_pseudo_op(Opcode::GetNZCVFromOp, value)
    }

    /// Emit a pseudo-op that reads flags from a producing instruction.
    /// Links the pseudo-op to the producing instruction via next_pseudoop,
    /// matching upstream's GetAssociatedPseudoOperation mechanism.
    fn emit_pseudo_op(&mut self, opcode: Opcode, producer: Value) -> Value {
        let pseudo_ref = self.block.append(opcode, &[producer]);
        // Link the pseudo-op to the producing instruction
        if let Value::Inst(producer_ref) = producer {
            let producer_inst = self.block.get_mut(producer_ref);
            // Append to the linked list of pseudo-ops
            if producer_inst.next_pseudoop.is_none() {
                producer_inst.next_pseudoop = Some(pseudo_ref);
            } else {
                // Walk the chain to find the end
                let mut current = producer_inst.next_pseudoop.unwrap();
                loop {
                    let next = self.block.get(current).next_pseudoop;
                    if let Some(next_ref) = next {
                        current = next_ref;
                    } else {
                        break;
                    }
                }
                self.block.get_mut(current).next_pseudoop = Some(pseudo_ref);
            }
        }
        Value::Inst(pseudo_ref)
    }

    pub fn nzcv_from_packed_flags(&mut self, value: Value) -> Value {
        self.emit(Opcode::NZCVFromPackedFlags, &[value])
    }

    // --- CRC32 ---

    pub fn crc32_castagnoli_8(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::CRC32Castagnoli8, &[a, b])
    }

    pub fn crc32_castagnoli_16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::CRC32Castagnoli16, &[a, b])
    }

    pub fn crc32_castagnoli_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::CRC32Castagnoli32, &[a, b])
    }

    pub fn crc32_castagnoli_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::CRC32Castagnoli64, &[a, b])
    }

    pub fn crc32_iso_8(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::CRC32ISO8, &[a, b])
    }

    pub fn crc32_iso_16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::CRC32ISO16, &[a, b])
    }

    pub fn crc32_iso_32(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::CRC32ISO32, &[a, b])
    }

    pub fn crc32_iso_64(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::CRC32ISO64, &[a, b])
    }

    pub fn max_signed(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let opcode = match esize {
            32 => Opcode::MaxSigned32,
            64 => Opcode::MaxSigned64,
            _ => panic!("Invalid esize {} for MaxSigned", esize),
        };
        self.emit(opcode, &[a, b])
    }

    pub fn max_unsigned(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let opcode = match esize {
            32 => Opcode::MaxUnsigned32,
            64 => Opcode::MaxUnsigned64,
            _ => panic!("Invalid esize {} for MaxUnsigned", esize),
        };
        self.emit(opcode, &[a, b])
    }

    pub fn min_signed(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let opcode = match esize {
            32 => Opcode::MinSigned32,
            64 => Opcode::MinSigned64,
            _ => panic!("Invalid esize {} for MinSigned", esize),
        };
        self.emit(opcode, &[a, b])
    }

    pub fn min_unsigned(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let opcode = match esize {
            32 => Opcode::MinUnsigned32,
            64 => Opcode::MinUnsigned64,
            _ => panic!("Invalid esize {} for MinUnsigned", esize),
        };
        self.emit(opcode, &[a, b])
    }

    // --- AES ---

    pub fn aes_decrypt_single_round(&mut self, a: Value) -> Value {
        self.emit(Opcode::AESDecryptSingleRound, &[a])
    }

    pub fn aes_encrypt_single_round(&mut self, a: Value) -> Value {
        self.emit(Opcode::AESEncryptSingleRound, &[a])
    }

    pub fn aes_inverse_mix_columns(&mut self, a: Value) -> Value {
        self.emit(Opcode::AESInverseMixColumns, &[a])
    }

    pub fn aes_mix_columns(&mut self, a: Value) -> Value {
        self.emit(Opcode::AESMixColumns, &[a])
    }

    // --- SHA ---

    pub fn sha256_hash(&mut self, x: Value, y: Value, w: Value, part1: Value) -> Value {
        self.emit(Opcode::SHA256Hash, &[x, y, w, part1])
    }

    pub fn sha256_message_schedule_0(&mut self, x: Value, y: Value) -> Value {
        self.emit(Opcode::SHA256MessageSchedule0, &[x, y])
    }

    pub fn sha256_message_schedule_1(&mut self, x: Value, y: Value, z: Value) -> Value {
        self.emit(Opcode::SHA256MessageSchedule1, &[x, y, z])
    }

    pub fn sm4_access_substitution_box(&mut self, a: Value) -> Value {
        self.emit(Opcode::SM4AccessSubstitutionBox, &[a])
    }

    // --- Vector get/set element ---

    pub fn vector_get_element(&mut self, esize: usize, a: Value, index: u8) -> Value {
        let idx = Value::ImmU8(index);
        match esize {
            8 => self.emit(Opcode::VectorGetElement8, &[a, idx]),
            16 => self.emit(Opcode::VectorGetElement16, &[a, idx]),
            32 => self.emit(Opcode::VectorGetElement32, &[a, idx]),
            64 => self.emit(Opcode::VectorGetElement64, &[a, idx]),
            _ => panic!("Invalid esize {}", esize),
        }
    }

    pub fn vector_set_element(
        &mut self,
        esize: usize,
        vec: Value,
        index: u8,
        elem: Value,
    ) -> Value {
        let idx = Value::ImmU8(index);
        // Upstream arg order: (vec, index, elem)
        match esize {
            8 => self.emit(Opcode::VectorSetElement8, &[vec, idx, elem]),
            16 => self.emit(Opcode::VectorSetElement16, &[vec, idx, elem]),
            32 => self.emit(Opcode::VectorSetElement32, &[vec, idx, elem]),
            64 => self.emit(Opcode::VectorSetElement64, &[vec, idx, elem]),
            _ => panic!("Invalid esize {}", esize),
        }
    }

    // --- Vector ops (size-dispatched) ---

    pub fn vector_add(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorAdd8,
            16 => Opcode::VectorAdd16,
            32 => Opcode::VectorAdd32,
            64 => Opcode::VectorAdd64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_sub(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorSub8,
            16 => Opcode::VectorSub16,
            32 => Opcode::VectorSub32,
            64 => Opcode::VectorSub64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_multiply(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorMultiply8,
            16 => Opcode::VectorMultiply16,
            32 => Opcode::VectorMultiply32,
            64 => Opcode::VectorMultiply64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_and(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::VectorAnd, &[a, b])
    }

    pub fn vector_or(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::VectorOr, &[a, b])
    }

    pub fn vector_eor(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::VectorEor, &[a, b])
    }

    pub fn vector_not(&mut self, a: Value) -> Value {
        self.emit(Opcode::VectorNot, &[a])
    }

    pub fn vector_and_not(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::VectorAndNot, &[a, b])
    }

    pub fn zero_vector(&mut self) -> Value {
        self.emit(Opcode::ZeroVector, &[])
    }

    pub fn vector_zero_upper(&mut self, a: Value) -> Value {
        self.emit(Opcode::VectorZeroUpper, &[a])
    }

    pub fn vector_abs(&mut self, esize: usize, a: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorAbs8,
            16 => Opcode::VectorAbs16,
            32 => Opcode::VectorAbs32,
            64 => Opcode::VectorAbs64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a])
    }

    pub fn vector_signed_absolute_difference(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorSignedAbsoluteDifference8,
            16 => Opcode::VectorSignedAbsoluteDifference16,
            32 => Opcode::VectorSignedAbsoluteDifference32,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_unsigned_absolute_difference(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
    ) -> Value {
        let op = match esize {
            8 => Opcode::VectorUnsignedAbsoluteDifference8,
            16 => Opcode::VectorUnsignedAbsoluteDifference16,
            32 => Opcode::VectorUnsignedAbsoluteDifference32,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_unsigned_recip_estimate(&mut self, a: Value) -> Value {
        self.emit(Opcode::VectorUnsignedRecipEstimate, &[a])
    }

    pub fn vector_unsigned_recip_sqrt_estimate(&mut self, a: Value) -> Value {
        self.emit(Opcode::VectorUnsignedRecipSqrtEstimate, &[a])
    }

    pub fn vector_equal(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorEqual8,
            16 => Opcode::VectorEqual16,
            32 => Opcode::VectorEqual32,
            64 => Opcode::VectorEqual64,
            128 => Opcode::VectorEqual128,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_paired_max_signed(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorPairedMaxS8,
            16 => Opcode::VectorPairedMaxS16,
            32 => Opcode::VectorPairedMaxS32,
            _ => panic!("Invalid esize {} for VectorPairedMaxSigned", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_paired_max_unsigned(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorPairedMaxU8,
            16 => Opcode::VectorPairedMaxU16,
            32 => Opcode::VectorPairedMaxU32,
            _ => panic!("Invalid esize {} for VectorPairedMaxUnsigned", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_paired_min_signed(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorPairedMinS8,
            16 => Opcode::VectorPairedMinS16,
            32 => Opcode::VectorPairedMinS32,
            _ => panic!("Invalid esize {} for VectorPairedMinSigned", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_paired_min_unsigned(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorPairedMinU8,
            16 => Opcode::VectorPairedMinU16,
            32 => Opcode::VectorPairedMinU32,
            _ => panic!("Invalid esize {} for VectorPairedMinUnsigned", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_paired_max_signed_lower(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorPairedMaxLowerS8,
            16 => Opcode::VectorPairedMaxLowerS16,
            32 => Opcode::VectorPairedMaxLowerS32,
            _ => panic!("Invalid esize {} for VectorPairedMaxSignedLower", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_paired_max_unsigned_lower(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorPairedMaxLowerU8,
            16 => Opcode::VectorPairedMaxLowerU16,
            32 => Opcode::VectorPairedMaxLowerU32,
            _ => panic!("Invalid esize {} for VectorPairedMaxUnsignedLower", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_paired_min_signed_lower(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorPairedMinLowerS8,
            16 => Opcode::VectorPairedMinLowerS16,
            32 => Opcode::VectorPairedMinLowerS32,
            _ => panic!("Invalid esize {} for VectorPairedMinSignedLower", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_paired_min_unsigned_lower(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorPairedMinLowerU8,
            16 => Opcode::VectorPairedMinLowerU16,
            32 => Opcode::VectorPairedMinLowerU32,
            _ => panic!("Invalid esize {} for VectorPairedMinUnsignedLower", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_greater_signed(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorGreaterS8,
            16 => Opcode::VectorGreaterS16,
            32 => Opcode::VectorGreaterS32,
            64 => Opcode::VectorGreaterS64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_greater_equal_signed(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorGreaterEqualSigned8,
            16 => Opcode::VectorGreaterEqualSigned16,
            32 => Opcode::VectorGreaterEqualSigned32,
            64 => Opcode::VectorGreaterEqualSigned64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_greater_equal_unsigned(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let max = self.vector_max_unsigned(esize, a, b);
        self.vector_equal(esize, max, a)
    }

    pub fn vector_less_equal_signed(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorLessEqualSigned8,
            16 => Opcode::VectorLessEqualSigned16,
            32 => Opcode::VectorLessEqualSigned32,
            64 => Opcode::VectorLessEqualSigned64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_less_signed(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorLessSigned8,
            16 => Opcode::VectorLessSigned16,
            32 => Opcode::VectorLessSigned32,
            64 => Opcode::VectorLessSigned64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_extract(&mut self, a: Value, b: Value, position: u8) -> Value {
        self.emit(Opcode::VectorExtract, &[a, b, Value::ImmU8(position)])
    }

    pub fn vector_extract_lower(&mut self, a: Value, b: Value, position: u8) -> Value {
        self.emit(Opcode::VectorExtractLower, &[a, b, Value::ImmU8(position)])
    }

    pub fn vector_rotate_whole_vector_right(&mut self, a: Value, amount: u8) -> Value {
        assert_eq!(amount % 32, 0);
        self.emit(
            Opcode::VectorRotateWholeVectorRight,
            &[a, Value::ImmU8(amount)],
        )
    }

    /// Upstream: `VectorDeinterleaveEven(esize, a, b)`.
    pub fn vector_deinterleave_even(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorDeinterleaveEven8,
            16 => Opcode::VectorDeinterleaveEven16,
            32 => Opcode::VectorDeinterleaveEven32,
            64 => Opcode::VectorDeinterleaveEven64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    /// Upstream: `VectorDeinterleaveOdd(esize, a, b)`.
    pub fn vector_deinterleave_odd(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorDeinterleaveOdd8,
            16 => Opcode::VectorDeinterleaveOdd16,
            32 => Opcode::VectorDeinterleaveOdd32,
            64 => Opcode::VectorDeinterleaveOdd64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_logical_shift_left(&mut self, esize: usize, a: Value, shift: u8) -> Value {
        let op = match esize {
            8 => Opcode::VectorLogicalShiftLeft8,
            16 => Opcode::VectorLogicalShiftLeft16,
            32 => Opcode::VectorLogicalShiftLeft32,
            64 => Opcode::VectorLogicalShiftLeft64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, Value::ImmU8(shift)])
    }

    pub fn vector_logical_shift_right(&mut self, esize: usize, a: Value, shift: u8) -> Value {
        let op = match esize {
            8 => Opcode::VectorLogicalShiftRight8,
            16 => Opcode::VectorLogicalShiftRight16,
            32 => Opcode::VectorLogicalShiftRight32,
            64 => Opcode::VectorLogicalShiftRight64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, Value::ImmU8(shift)])
    }

    pub fn vector_rotate_right(&mut self, esize: usize, a: Value, amount: u8) -> Value {
        assert!((amount as usize) < esize);
        if amount == 0 {
            return a;
        }
        let right = self.vector_logical_shift_right(esize, a, amount);
        let left = self.vector_logical_shift_left(esize, a, esize as u8 - amount);
        self.vector_or(right, left)
    }

    pub fn vector_rotate_left(&mut self, esize: usize, a: Value, amount: u8) -> Value {
        assert!((amount as usize) < esize);
        if amount == 0 {
            return a;
        }
        let left = self.vector_logical_shift_left(esize, a, amount);
        let right = self.vector_logical_shift_right(esize, a, esize as u8 - amount);
        self.vector_or(left, right)
    }

    pub fn vector_arithmetic_shift_right(&mut self, esize: usize, a: Value, shift: u8) -> Value {
        let op = match esize {
            8 => Opcode::VectorArithmeticShiftRight8,
            16 => Opcode::VectorArithmeticShiftRight16,
            32 => Opcode::VectorArithmeticShiftRight32,
            64 => Opcode::VectorArithmeticShiftRight64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, Value::ImmU8(shift)])
    }

    pub fn vector_signed_saturated_shift_left(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
    ) -> Value {
        let op = match esize {
            8 => Opcode::VectorSignedSaturatedShiftLeft8,
            16 => Opcode::VectorSignedSaturatedShiftLeft16,
            32 => Opcode::VectorSignedSaturatedShiftLeft32,
            64 => Opcode::VectorSignedSaturatedShiftLeft64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_signed_saturated_shift_left_unsigned(
        &mut self,
        esize: usize,
        a: Value,
        shift: u8,
    ) -> Value {
        let shift_vec = self.vector_broadcast(esize, Value::ImmU64(shift as u64));
        let op = match esize {
            8 => Opcode::VectorSignedSaturatedShiftLeftUnsigned8,
            16 => Opcode::VectorSignedSaturatedShiftLeftUnsigned16,
            32 => Opcode::VectorSignedSaturatedShiftLeftUnsigned32,
            64 => Opcode::VectorSignedSaturatedShiftLeftUnsigned64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, shift_vec])
    }

    pub fn vector_unsigned_saturated_shift_left(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
    ) -> Value {
        let op = match esize {
            8 => Opcode::VectorUnsignedSaturatedShiftLeft8,
            16 => Opcode::VectorUnsignedSaturatedShiftLeft16,
            32 => Opcode::VectorUnsignedSaturatedShiftLeft32,
            64 => Opcode::VectorUnsignedSaturatedShiftLeft64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_narrow(&mut self, original_esize: usize, a: Value) -> Value {
        let op = match original_esize {
            16 => Opcode::VectorNarrow16,
            32 => Opcode::VectorNarrow32,
            64 => Opcode::VectorNarrow64,
            _ => panic!("Invalid esize {}", original_esize),
        };
        self.emit(op, &[a])
    }

    pub fn vector_signed_saturated_narrow_to_signed(
        &mut self,
        original_esize: usize,
        a: Value,
    ) -> Value {
        let op = match original_esize {
            16 => Opcode::VectorSignedSaturatedNarrowToSigned16,
            32 => Opcode::VectorSignedSaturatedNarrowToSigned32,
            64 => Opcode::VectorSignedSaturatedNarrowToSigned64,
            _ => panic!("Invalid esize {}", original_esize),
        };
        self.emit(op, &[a])
    }

    pub fn vector_signed_saturated_narrow_to_unsigned(
        &mut self,
        original_esize: usize,
        a: Value,
    ) -> Value {
        let op = match original_esize {
            16 => Opcode::VectorSignedSaturatedNarrowToUnsigned16,
            32 => Opcode::VectorSignedSaturatedNarrowToUnsigned32,
            64 => Opcode::VectorSignedSaturatedNarrowToUnsigned64,
            _ => panic!("Invalid esize {}", original_esize),
        };
        self.emit(op, &[a])
    }

    pub fn vector_unsigned_saturated_narrow(&mut self, original_esize: usize, a: Value) -> Value {
        let op = match original_esize {
            16 => Opcode::VectorUnsignedSaturatedNarrow16,
            32 => Opcode::VectorUnsignedSaturatedNarrow32,
            64 => Opcode::VectorUnsignedSaturatedNarrow64,
            _ => panic!("Invalid esize {}", original_esize),
        };
        self.emit(op, &[a])
    }

    pub fn vector_sign_extend(&mut self, original_esize: usize, a: Value) -> Value {
        let op = match original_esize {
            8 => Opcode::VectorSignExtend8,
            16 => Opcode::VectorSignExtend16,
            32 => Opcode::VectorSignExtend32,
            64 => Opcode::VectorSignExtend64,
            _ => panic!("Invalid esize {}", original_esize),
        };
        self.emit(op, &[a])
    }

    pub fn vector_zero_extend(&mut self, original_esize: usize, a: Value) -> Value {
        let op = match original_esize {
            8 => Opcode::VectorZeroExtend8,
            16 => Opcode::VectorZeroExtend16,
            32 => Opcode::VectorZeroExtend32,
            64 => Opcode::VectorZeroExtend64,
            _ => panic!("Invalid esize {}", original_esize),
        };
        self.emit(op, &[a])
    }

    pub fn vector_max_signed(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorMaxS8,
            16 => Opcode::VectorMaxS16,
            32 => Opcode::VectorMaxS32,
            64 => Opcode::VectorMaxS64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_max_unsigned(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorMaxU8,
            16 => Opcode::VectorMaxU16,
            32 => Opcode::VectorMaxU32,
            64 => Opcode::VectorMaxU64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_min_signed(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorMinS8,
            16 => Opcode::VectorMinS16,
            32 => Opcode::VectorMinS32,
            64 => Opcode::VectorMinS64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_min_unsigned(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorMinU8,
            16 => Opcode::VectorMinU16,
            32 => Opcode::VectorMinU32,
            64 => Opcode::VectorMinU64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_broadcast(&mut self, esize: usize, a: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorBroadcast8,
            16 => Opcode::VectorBroadcast16,
            32 => Opcode::VectorBroadcast32,
            64 => Opcode::VectorBroadcast64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a])
    }

    pub fn vector_broadcast_lower(&mut self, esize: usize, a: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorBroadcastLower8,
            16 => Opcode::VectorBroadcastLower16,
            32 => Opcode::VectorBroadcastLower32,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a])
    }

    pub fn vector_broadcast_element_lower(&mut self, esize: usize, a: Value, index: u8) -> Value {
        assert!(esize * (index as usize) < 128, "Invalid index");
        let op = match esize {
            8 => Opcode::VectorBroadcastElementLower8,
            16 => Opcode::VectorBroadcastElementLower16,
            32 => Opcode::VectorBroadcastElementLower32,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, Value::ImmU8(index)])
    }

    pub fn vector_count_leading_zeros(&mut self, esize: usize, a: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorCountLeadingZeros8,
            16 => Opcode::VectorCountLeadingZeros16,
            32 => Opcode::VectorCountLeadingZeros32,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a])
    }

    pub fn vector_rounding_halving_add_signed(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
    ) -> Value {
        let op = match esize {
            8 => Opcode::VectorRoundingHalvingAddS8,
            16 => Opcode::VectorRoundingHalvingAddS16,
            32 => Opcode::VectorRoundingHalvingAddS32,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_rounding_halving_add_unsigned(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
    ) -> Value {
        let op = match esize {
            8 => Opcode::VectorRoundingHalvingAddU8,
            16 => Opcode::VectorRoundingHalvingAddU16,
            32 => Opcode::VectorRoundingHalvingAddU32,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_halving_add_signed(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorHalvingAddS8,
            16 => Opcode::VectorHalvingAddS16,
            32 => Opcode::VectorHalvingAddS32,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_halving_add_unsigned(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorHalvingAddU8,
            16 => Opcode::VectorHalvingAddU16,
            32 => Opcode::VectorHalvingAddU32,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_halving_sub_signed(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorHalvingSubS8,
            16 => Opcode::VectorHalvingSubS16,
            32 => Opcode::VectorHalvingSubS32,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_halving_sub_unsigned(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorHalvingSubU8,
            16 => Opcode::VectorHalvingSubU16,
            32 => Opcode::VectorHalvingSubU32,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_signed_saturated_add(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorSignedSaturatedAdd8,
            16 => Opcode::VectorSignedSaturatedAdd16,
            32 => Opcode::VectorSignedSaturatedAdd32,
            64 => Opcode::VectorSignedSaturatedAdd64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_unsigned_saturated_add(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorUnsignedSaturatedAdd8,
            16 => Opcode::VectorUnsignedSaturatedAdd16,
            32 => Opcode::VectorUnsignedSaturatedAdd32,
            64 => Opcode::VectorUnsignedSaturatedAdd64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_signed_saturated_sub(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorSignedSaturatedSub8,
            16 => Opcode::VectorSignedSaturatedSub16,
            32 => Opcode::VectorSignedSaturatedSub32,
            64 => Opcode::VectorSignedSaturatedSub64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_unsigned_saturated_sub(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorUnsignedSaturatedSub8,
            16 => Opcode::VectorUnsignedSaturatedSub16,
            32 => Opcode::VectorUnsignedSaturatedSub32,
            64 => Opcode::VectorUnsignedSaturatedSub64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_signed_saturated_abs(&mut self, esize: usize, a: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorSignedSaturatedAbs8,
            16 => Opcode::VectorSignedSaturatedAbs16,
            32 => Opcode::VectorSignedSaturatedAbs32,
            64 => Opcode::VectorSignedSaturatedAbs64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a])
    }

    pub fn vector_signed_saturated_accumulate_unsigned(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
    ) -> Value {
        let op = match esize {
            8 => Opcode::VectorSignedSaturatedAccumulateUnsigned8,
            16 => Opcode::VectorSignedSaturatedAccumulateUnsigned16,
            32 => Opcode::VectorSignedSaturatedAccumulateUnsigned32,
            64 => Opcode::VectorSignedSaturatedAccumulateUnsigned64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_signed_saturated_neg(&mut self, esize: usize, a: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorSignedSaturatedNeg8,
            16 => Opcode::VectorSignedSaturatedNeg16,
            32 => Opcode::VectorSignedSaturatedNeg32,
            64 => Opcode::VectorSignedSaturatedNeg64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a])
    }

    pub fn vector_unsigned_saturated_accumulate_signed(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
    ) -> Value {
        let op = match esize {
            8 => Opcode::VectorUnsignedSaturatedAccumulateSigned8,
            16 => Opcode::VectorUnsignedSaturatedAccumulateSigned16,
            32 => Opcode::VectorUnsignedSaturatedAccumulateSigned32,
            64 => Opcode::VectorUnsignedSaturatedAccumulateSigned64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_logical_v_shift(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorLogicalVShift8,
            16 => Opcode::VectorLogicalVShift16,
            32 => Opcode::VectorLogicalVShift32,
            64 => Opcode::VectorLogicalVShift64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_arithmetic_v_shift(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorArithmeticVShift8,
            16 => Opcode::VectorArithmeticVShift16,
            32 => Opcode::VectorArithmeticVShift32,
            64 => Opcode::VectorArithmeticVShift64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_rounding_shift_left_signed(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorRoundingShiftLeftS8,
            16 => Opcode::VectorRoundingShiftLeftS16,
            32 => Opcode::VectorRoundingShiftLeftS32,
            64 => Opcode::VectorRoundingShiftLeftS64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_rounding_shift_left_unsigned(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
    ) -> Value {
        let op = match esize {
            8 => Opcode::VectorRoundingShiftLeftU8,
            16 => Opcode::VectorRoundingShiftLeftU16,
            32 => Opcode::VectorRoundingShiftLeftU32,
            64 => Opcode::VectorRoundingShiftLeftU64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_multiply_signed_widen(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorMultiplySignedWiden8,
            16 => Opcode::VectorMultiplySignedWiden16,
            32 => Opcode::VectorMultiplySignedWiden32,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_multiply_unsigned_widen(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorMultiplyUnsignedWiden8,
            16 => Opcode::VectorMultiplyUnsignedWiden16,
            32 => Opcode::VectorMultiplyUnsignedWiden32,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_polynomial_multiply(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::VectorPolynomialMultiply8, &[a, b])
    }

    pub fn vector_polynomial_multiply_long(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorPolynomialMultiplyLong8,
            64 => Opcode::VectorPolynomialMultiplyLong64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_paired_add_lower(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorPairedAddLower8,
            16 => Opcode::VectorPairedAddLower16,
            32 => Opcode::VectorPairedAddLower32,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_paired_add_signed_widen(&mut self, original_esize: usize, a: Value) -> Value {
        let op = match original_esize {
            8 => Opcode::VectorPairedAddSignedWiden8,
            16 => Opcode::VectorPairedAddSignedWiden16,
            32 => Opcode::VectorPairedAddSignedWiden32,
            _ => panic!("Invalid esize {}", original_esize),
        };
        self.emit(op, &[a])
    }

    pub fn vector_paired_add_unsigned_widen(&mut self, original_esize: usize, a: Value) -> Value {
        let op = match original_esize {
            8 => Opcode::VectorPairedAddUnsignedWiden8,
            16 => Opcode::VectorPairedAddUnsignedWiden16,
            32 => Opcode::VectorPairedAddUnsignedWiden32,
            _ => panic!("Invalid esize {}", original_esize),
        };
        self.emit(op, &[a])
    }

    pub fn vector_deinterleave_even_lower(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorDeinterleaveEvenLower8,
            16 => Opcode::VectorDeinterleaveEvenLower16,
            32 => Opcode::VectorDeinterleaveEvenLower32,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_deinterleave_odd_lower(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorDeinterleaveOddLower8,
            16 => Opcode::VectorDeinterleaveOddLower16,
            32 => Opcode::VectorDeinterleaveOddLower32,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_signed_saturated_doubling_multiply_high(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
    ) -> Value {
        let op = match esize {
            16 => Opcode::VectorSignedSaturatedDoublingMultiplyHigh16,
            32 => Opcode::VectorSignedSaturatedDoublingMultiplyHigh32,
            _ => panic!("VQDMULH: invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_signed_saturated_doubling_multiply_long(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
    ) -> Value {
        let op = match esize {
            16 => Opcode::VectorSignedSaturatedDoublingMultiplyLong16,
            32 => Opcode::VectorSignedSaturatedDoublingMultiplyLong32,
            _ => panic!("VQDMULL: invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_signed_saturated_doubling_multiply_high_rounding(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
    ) -> Value {
        let op = match esize {
            16 => Opcode::VectorSignedSaturatedDoublingMultiplyHighRounding16,
            32 => Opcode::VectorSignedSaturatedDoublingMultiplyHighRounding32,
            _ => panic!("VQRDMULH: invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn fp_vector_recip_step_fused(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            16 => Opcode::FPVectorRecipStepFused16,
            32 => Opcode::FPVectorRecipStepFused32,
            64 => Opcode::FPVectorRecipStepFused64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b, Value::ImmU1(fpcr_controlled)])
    }

    pub fn fp_vector_recip_estimate(
        &mut self,
        esize: usize,
        a: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            16 => Opcode::FPVectorRecipEstimate16,
            32 => Opcode::FPVectorRecipEstimate32,
            64 => Opcode::FPVectorRecipEstimate64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, Value::ImmU1(fpcr_controlled)])
    }

    pub fn fp_vector_round_int(
        &mut self,
        esize: usize,
        operand: Value,
        rounding: u8,
        exact: bool,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            16 => Opcode::FPVectorRoundInt16,
            32 => Opcode::FPVectorRoundInt32,
            64 => Opcode::FPVectorRoundInt64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(
            op,
            &[
                operand,
                Value::ImmU8(rounding),
                Value::ImmU1(exact),
                Value::ImmU1(fpcr_controlled),
            ],
        )
    }

    pub fn fp_vector_rsqrt_estimate(
        &mut self,
        esize: usize,
        a: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            16 => Opcode::FPVectorRSqrtEstimate16,
            32 => Opcode::FPVectorRSqrtEstimate32,
            64 => Opcode::FPVectorRSqrtEstimate64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, Value::ImmU1(fpcr_controlled)])
    }

    pub fn zero_extend_to_quad(&mut self, a: Value) -> Value {
        self.zero_extend_long_to_quad(a)
    }

    pub fn vector_interleave_lower(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorInterleaveLower8,
            16 => Opcode::VectorInterleaveLower16,
            32 => Opcode::VectorInterleaveLower32,
            64 => Opcode::VectorInterleaveLower64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_interleave_upper(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorInterleaveUpper8,
            16 => Opcode::VectorInterleaveUpper16,
            32 => Opcode::VectorInterleaveUpper32,
            64 => Opcode::VectorInterleaveUpper64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_paired_add(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorPairedAdd8,
            16 => Opcode::VectorPairedAdd16,
            32 => Opcode::VectorPairedAdd32,
            64 => Opcode::VectorPairedAdd64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn vector_population_count(&mut self, a: Value) -> Value {
        self.emit(Opcode::VectorPopulationCount, &[a])
    }

    pub fn vector_reverse_bits(&mut self, a: Value) -> Value {
        self.emit(Opcode::VectorReverseBits, &[a])
    }

    /// Reverse byte order within each `group_esize`-bit element of a vector
    /// where each element is composed of `byte_esize`-bit lanes. Used by
    /// AArch64 REV16/REV32/REV64 (vector form).
    /// e.g. REV16.16B → group_esize=16, byte_esize=8.
    pub fn vector_reverse_element_in_groups(
        &mut self,
        group_esize: usize,
        byte_esize: usize,
        a: Value,
    ) -> Value {
        let op = match (group_esize, byte_esize) {
            (16, 8) => Opcode::VectorReverseElementsInHalfGroups8,
            (32, 8) => Opcode::VectorReverseElementsInWordGroups8,
            (32, 16) => Opcode::VectorReverseElementsInWordGroups16,
            (64, 8) => Opcode::VectorReverseElementsInLongGroups8,
            (64, 16) => Opcode::VectorReverseElementsInLongGroups16,
            (64, 32) => Opcode::VectorReverseElementsInLongGroups32,
            _ => panic!(
                "Invalid (group, byte) esize combination ({}, {})",
                group_esize, byte_esize
            ),
        };
        self.emit(op, &[a])
    }

    /// Upstream: `VectorTable(std::vector<U64|U128>)`.
    ///
    /// The IR node always has four operands; unused table entries are
    /// `Value::Void`, matching upstream's resized vector of empty IR values.
    pub fn vector_table(&mut self, values: &[Value]) -> Value {
        assert!((1..=4).contains(&values.len()));
        let mut args = [Value::Void; 4];
        args[..values.len()].copy_from_slice(values);
        self.emit(Opcode::VectorTable, &args)
    }

    pub fn vector_table_lookup_64(
        &mut self,
        default: Value,
        table: Value,
        indices: Value,
    ) -> Value {
        self.emit(Opcode::VectorTableLookup64, &[default, table, indices])
    }

    pub fn vector_table_lookup_128(
        &mut self,
        default: Value,
        table: Value,
        indices: Value,
    ) -> Value {
        self.emit(Opcode::VectorTableLookup128, &[default, table, indices])
    }

    /// Upstream: `VectorTranspose(esize, a, b, part)`.
    /// part=false selects even elements, part=true selects odd elements.
    pub fn vector_transpose(&mut self, esize: usize, a: Value, b: Value, part: bool) -> Value {
        let op = match esize {
            8 => Opcode::VectorTranspose8,
            16 => Opcode::VectorTranspose16,
            32 => Opcode::VectorTranspose32,
            64 => Opcode::VectorTranspose64,
            _ => panic!("Invalid esize {}", esize),
        };
        let imm = Value::ImmU1(part);
        self.emit(op, &[a, b, imm])
    }

    // --- FP scalar ---

    pub fn fp_abs(&mut self, esize: usize, a: Value) -> Value {
        let op = match esize {
            16 => Opcode::FPAbs16,
            32 => Opcode::FPAbs32,
            64 => Opcode::FPAbs64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a])
    }

    pub fn fp_neg(&mut self, esize: usize, a: Value) -> Value {
        let op = match esize {
            16 => Opcode::FPNeg16,
            32 => Opcode::FPNeg32,
            64 => Opcode::FPNeg64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a])
    }

    pub fn fp_recip_estimate(&mut self, esize: usize, a: Value) -> Value {
        let op = match esize {
            16 => Opcode::FPRecipEstimate16,
            32 => Opcode::FPRecipEstimate32,
            64 => Opcode::FPRecipEstimate64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a])
    }

    pub fn fp_recip_exponent(&mut self, esize: usize, a: Value) -> Value {
        let op = match esize {
            16 => Opcode::FPRecipExponent16,
            32 => Opcode::FPRecipExponent32,
            64 => Opcode::FPRecipExponent64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a])
    }

    pub fn fp_recip_step_fused(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            16 => Opcode::FPRecipStepFused16,
            32 => Opcode::FPRecipStepFused32,
            64 => Opcode::FPRecipStepFused64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn fp_rsqrt_estimate(&mut self, esize: usize, a: Value) -> Value {
        let op = match esize {
            16 => Opcode::FPRSqrtEstimate16,
            32 => Opcode::FPRSqrtEstimate32,
            64 => Opcode::FPRSqrtEstimate64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a])
    }

    pub fn fp_rsqrt_step_fused(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            16 => Opcode::FPRSqrtStepFused16,
            32 => Opcode::FPRSqrtStepFused32,
            64 => Opcode::FPRSqrtStepFused64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn fp_add(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            32 => Opcode::FPAdd32,
            64 => Opcode::FPAdd64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn fp_sub(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            32 => Opcode::FPSub32,
            64 => Opcode::FPSub64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn fp_mul(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            32 => Opcode::FPMul32,
            64 => Opcode::FPMul64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn fp_mulx(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            32 => Opcode::FPMulX32,
            64 => Opcode::FPMulX64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn fp_div(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            32 => Opcode::FPDiv32,
            64 => Opcode::FPDiv64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn fp_max(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            32 => Opcode::FPMax32,
            64 => Opcode::FPMax64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn fp_max_numeric(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            32 => Opcode::FPMaxNumeric32,
            64 => Opcode::FPMaxNumeric64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn fp_min(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            32 => Opcode::FPMin32,
            64 => Opcode::FPMin64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn fp_min_numeric(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let op = match esize {
            32 => Opcode::FPMinNumeric32,
            64 => Opcode::FPMinNumeric64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b])
    }

    pub fn fp_sqrt(&mut self, esize: usize, a: Value) -> Value {
        let op = match esize {
            32 => Opcode::FPSqrt32,
            64 => Opcode::FPSqrt64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a])
    }

    pub fn fp_compare(&mut self, esize: usize, a: Value, b: Value, exc_on_qnan: Value) -> Value {
        let op = match esize {
            32 => Opcode::FPCompare32,
            64 => Opcode::FPCompare64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b, exc_on_qnan])
    }

    pub fn fp_mul_add(&mut self, esize: usize, addend: Value, op1: Value, op2: Value) -> Value {
        let op = match esize {
            16 => Opcode::FPMulAdd16,
            32 => Opcode::FPMulAdd32,
            64 => Opcode::FPMulAdd64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[addend, op1, op2])
    }

    pub fn fp_mul_sub(&mut self, esize: usize, minuend: Value, op1: Value, op2: Value) -> Value {
        let op = match esize {
            16 => Opcode::FPMulSub16,
            32 => Opcode::FPMulSub32,
            64 => Opcode::FPMulSub64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[minuend, op1, op2])
    }

    pub fn fp_round_int(&mut self, esize: usize, a: Value, rounding: u8, exact: bool) -> Value {
        let op = match esize {
            16 => Opcode::FPRoundInt16,
            32 => Opcode::FPRoundInt32,
            64 => Opcode::FPRoundInt64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, Value::ImmU8(rounding), Value::ImmU1(exact)])
    }

    // --- FP conversions ---

    pub fn fp_half_to_single(&mut self, a: Value, rounding: u8) -> Value {
        self.emit(Opcode::FPHalfToSingle, &[a, Value::ImmU8(rounding)])
    }

    pub fn fp_half_to_double(&mut self, a: Value, rounding: u8) -> Value {
        self.emit(Opcode::FPHalfToDouble, &[a, Value::ImmU8(rounding)])
    }

    pub fn fp_single_to_double(&mut self, a: Value, rounding: u8) -> Value {
        self.emit(Opcode::FPSingleToDouble, &[a, Value::ImmU8(rounding)])
    }

    pub fn fp_single_to_half(&mut self, a: Value, rounding: u8) -> Value {
        self.emit(Opcode::FPSingleToHalf, &[a, Value::ImmU8(rounding)])
    }

    pub fn fp_double_to_single(&mut self, a: Value, rounding: u8) -> Value {
        self.emit(Opcode::FPDoubleToSingle, &[a, Value::ImmU8(rounding)])
    }

    pub fn fp_double_to_half(&mut self, a: Value, rounding: u8) -> Value {
        self.emit(Opcode::FPDoubleToHalf, &[a, Value::ImmU8(rounding)])
    }

    pub fn fp_fixed_to_single(
        &mut self,
        a: Value,
        source_size: usize,
        signed: bool,
        fbits: u8,
        rounding: u8,
    ) -> Value {
        let op = match (source_size, signed) {
            (16, true) => Opcode::FPFixedS16ToSingle,
            (16, false) => Opcode::FPFixedU16ToSingle,
            (32, true) => Opcode::FPFixedS32ToSingle,
            (32, false) => Opcode::FPFixedU32ToSingle,
            (64, true) => Opcode::FPFixedS64ToSingle,
            (64, false) => Opcode::FPFixedU64ToSingle,
            _ => panic!("Invalid FP fixed->single size {}", source_size),
        };
        self.emit(op, &[a, Value::ImmU8(fbits), Value::ImmU8(rounding)])
    }

    pub fn fp_fixed_to_double(
        &mut self,
        a: Value,
        source_size: usize,
        signed: bool,
        fbits: u8,
        rounding: u8,
    ) -> Value {
        let op = match (source_size, signed) {
            (16, true) => Opcode::FPFixedS16ToDouble,
            (16, false) => Opcode::FPFixedU16ToDouble,
            (32, true) => Opcode::FPFixedS32ToDouble,
            (32, false) => Opcode::FPFixedU32ToDouble,
            (64, true) => Opcode::FPFixedS64ToDouble,
            (64, false) => Opcode::FPFixedU64ToDouble,
            _ => panic!("Invalid FP fixed->double size {}", source_size),
        };
        self.emit(op, &[a, Value::ImmU8(fbits), Value::ImmU8(rounding)])
    }

    pub fn fp_to_fixed_s32(
        &mut self,
        a: Value,
        source_size: usize,
        fbits: u8,
        rounding: u8,
    ) -> Value {
        let op = match source_size {
            16 => Opcode::FPHalfToFixedS32,
            32 => Opcode::FPSingleToFixedS32,
            64 => Opcode::FPDoubleToFixedS32,
            _ => panic!("Invalid FP->fixed s32 size {}", source_size),
        };
        self.emit(op, &[a, Value::ImmU8(fbits), Value::ImmU8(rounding)])
    }

    pub fn fp_to_fixed_u32(
        &mut self,
        a: Value,
        source_size: usize,
        fbits: u8,
        rounding: u8,
    ) -> Value {
        let op = match source_size {
            16 => Opcode::FPHalfToFixedU32,
            32 => Opcode::FPSingleToFixedU32,
            64 => Opcode::FPDoubleToFixedU32,
            _ => panic!("Invalid FP->fixed u32 size {}", source_size),
        };
        self.emit(op, &[a, Value::ImmU8(fbits), Value::ImmU8(rounding)])
    }

    pub fn fp_to_fixed_s64(
        &mut self,
        a: Value,
        source_size: usize,
        fbits: u8,
        rounding: u8,
    ) -> Value {
        let op = match source_size {
            16 => Opcode::FPHalfToFixedS64,
            32 => Opcode::FPSingleToFixedS64,
            64 => Opcode::FPDoubleToFixedS64,
            _ => panic!("Invalid FP->fixed s64 size {}", source_size),
        };
        self.emit(op, &[a, Value::ImmU8(fbits), Value::ImmU8(rounding)])
    }

    pub fn fp_to_fixed_u64(
        &mut self,
        a: Value,
        source_size: usize,
        fbits: u8,
        rounding: u8,
    ) -> Value {
        let op = match source_size {
            16 => Opcode::FPHalfToFixedU64,
            32 => Opcode::FPSingleToFixedU64,
            64 => Opcode::FPDoubleToFixedU64,
            _ => panic!("Invalid FP->fixed u64 size {}", source_size),
        };
        self.emit(op, &[a, Value::ImmU8(fbits), Value::ImmU8(rounding)])
    }

    // --- FP vector ops ---

    pub fn fp_vector_add(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            32 => Opcode::FPVectorAdd32,
            64 => Opcode::FPVectorAdd64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b, Value::ImmU1(fpcr_controlled)])
    }

    pub fn fp_vector_paired_add_lower(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            32 => Opcode::FPVectorPairedAddLower32,
            64 => Opcode::FPVectorPairedAddLower64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b, Value::ImmU1(fpcr_controlled)])
    }

    pub fn fp_vector_paired_add(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            32 => Opcode::FPVectorPairedAdd32,
            64 => Opcode::FPVectorPairedAdd64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b, Value::ImmU1(fpcr_controlled)])
    }

    pub fn vector_broadcast_element(&mut self, esize: usize, a: Value, index: u8) -> Value {
        assert!(esize * (index as usize) < 128, "Invalid index");
        let op = match esize {
            8 => Opcode::VectorBroadcastElement8,
            16 => Opcode::VectorBroadcastElement16,
            32 => Opcode::VectorBroadcastElement32,
            64 => Opcode::VectorBroadcastElement64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, Value::ImmU8(index)])
    }

    pub fn fp_vector_rsqrt_step_fused(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            16 => Opcode::FPVectorRSqrtStepFused16,
            32 => Opcode::FPVectorRSqrtStepFused32,
            64 => Opcode::FPVectorRSqrtStepFused64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b, Value::ImmU1(fpcr_controlled)])
    }

    pub fn fp_vector_sub(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            32 => Opcode::FPVectorSub32,
            64 => Opcode::FPVectorSub64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b, Value::ImmU1(fpcr_controlled)])
    }

    pub fn fp_vector_mul(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            32 => Opcode::FPVectorMul32,
            64 => Opcode::FPVectorMul64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b, Value::ImmU1(fpcr_controlled)])
    }

    pub fn fp_vector_max(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            32 => Opcode::FPVectorMax32,
            64 => Opcode::FPVectorMax64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b, Value::ImmU1(fpcr_controlled)])
    }

    pub fn fp_vector_min(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            32 => Opcode::FPVectorMin32,
            64 => Opcode::FPVectorMin64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b, Value::ImmU1(fpcr_controlled)])
    }

    pub fn fp_vector_div(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            32 => Opcode::FPVectorDiv32,
            64 => Opcode::FPVectorDiv64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b, Value::ImmU1(fpcr_controlled)])
    }

    pub fn fp_vector_max_numeric(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            32 => Opcode::FPVectorMaxNumeric32,
            64 => Opcode::FPVectorMaxNumeric64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b, Value::ImmU1(fpcr_controlled)])
    }

    pub fn fp_vector_min_numeric(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            32 => Opcode::FPVectorMinNumeric32,
            64 => Opcode::FPVectorMinNumeric64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b, Value::ImmU1(fpcr_controlled)])
    }

    pub fn fp_vector_mulx(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            32 => Opcode::FPVectorMulX32,
            64 => Opcode::FPVectorMulX64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b, Value::ImmU1(fpcr_controlled)])
    }

    pub fn fp_vector_equal(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            16 => Opcode::FPVectorEqual16,
            32 => Opcode::FPVectorEqual32,
            64 => Opcode::FPVectorEqual64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b, Value::ImmU1(fpcr_controlled)])
    }

    pub fn fp_vector_greater(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            32 => Opcode::FPVectorGreater32,
            64 => Opcode::FPVectorGreater64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b, Value::ImmU1(fpcr_controlled)])
    }

    pub fn fp_vector_greater_equal(
        &mut self,
        esize: usize,
        a: Value,
        b: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            32 => Opcode::FPVectorGreaterEqual32,
            64 => Opcode::FPVectorGreaterEqual64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, b, Value::ImmU1(fpcr_controlled)])
    }

    pub fn fp_vector_abs(&mut self, esize: usize, a: Value) -> Value {
        let op = match esize {
            16 => Opcode::FPVectorAbs16,
            32 => Opcode::FPVectorAbs32,
            64 => Opcode::FPVectorAbs64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a])
    }

    pub fn fp_vector_neg(&mut self, esize: usize, a: Value) -> Value {
        let op = match esize {
            16 => Opcode::FPVectorNeg16,
            32 => Opcode::FPVectorNeg32,
            64 => Opcode::FPVectorNeg64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a])
    }

    pub fn fp_vector_sqrt(&mut self, esize: usize, a: Value, fpcr_controlled: bool) -> Value {
        let op = match esize {
            32 => Opcode::FPVectorSqrt32,
            64 => Opcode::FPVectorSqrt64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, Value::ImmU1(fpcr_controlled)])
    }

    pub fn fp_vector_mul_add(
        &mut self,
        esize: usize,
        addend: Value,
        op1: Value,
        op2: Value,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            16 => Opcode::FPVectorMulAdd16,
            32 => Opcode::FPVectorMulAdd32,
            64 => Opcode::FPVectorMulAdd64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[addend, op1, op2, Value::ImmU1(fpcr_controlled)])
    }

    pub fn fp_vector_from_signed_fixed(
        &mut self,
        esize: usize,
        a: Value,
        fbits: u8,
        rounding: u8,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            32 => Opcode::FPVectorFromSignedFixed32,
            64 => Opcode::FPVectorFromSignedFixed64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(
            op,
            &[
                a,
                Value::ImmU8(fbits),
                Value::ImmU8(rounding),
                Value::ImmU1(fpcr_controlled),
            ],
        )
    }

    pub fn fp_vector_from_unsigned_fixed(
        &mut self,
        esize: usize,
        a: Value,
        fbits: u8,
        rounding: u8,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            32 => Opcode::FPVectorFromUnsignedFixed32,
            64 => Opcode::FPVectorFromUnsignedFixed64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(
            op,
            &[
                a,
                Value::ImmU8(fbits),
                Value::ImmU8(rounding),
                Value::ImmU1(fpcr_controlled),
            ],
        )
    }

    pub fn fp_vector_to_signed_fixed(
        &mut self,
        esize: usize,
        a: Value,
        fbits: u8,
        rounding: u8,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            16 => Opcode::FPVectorToSignedFixed16,
            32 => Opcode::FPVectorToSignedFixed32,
            64 => Opcode::FPVectorToSignedFixed64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(
            op,
            &[
                a,
                Value::ImmU8(fbits),
                Value::ImmU8(rounding),
                Value::ImmU1(fpcr_controlled),
            ],
        )
    }

    pub fn fp_vector_to_unsigned_fixed(
        &mut self,
        esize: usize,
        a: Value,
        fbits: u8,
        rounding: u8,
        fpcr_controlled: bool,
    ) -> Value {
        let op = match esize {
            16 => Opcode::FPVectorToUnsignedFixed16,
            32 => Opcode::FPVectorToUnsignedFixed32,
            64 => Opcode::FPVectorToUnsignedFixed64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(
            op,
            &[
                a,
                Value::ImmU8(fbits),
                Value::ImmU8(rounding),
                Value::ImmU1(fpcr_controlled),
            ],
        )
    }

    // --- Misc ---

    pub fn breakpoint(&mut self) {
        self.emit_void(Opcode::Breakpoint, &[]);
    }

    pub fn push_rsb(&mut self, return_location: LocationDescriptor) {
        self.emit_void(Opcode::PushRSB, &[Value::ImmU64(return_location.value())]);
    }

    pub fn get_nz_from_op(&mut self, value: Value) -> Value {
        self.emit(Opcode::GetNZFromOp, &[value])
    }

    pub fn get_c_flag_from_nzcv(&mut self, nzcv: Value) -> Value {
        self.emit(Opcode::GetCFlagFromNZCV, &[nzcv])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::block::Block;
    use crate::ir::location::LocationDescriptor;
    use crate::ir::value::InstRef;

    #[test]
    fn test_emitter_build_add() {
        let mut block = Block::new(LocationDescriptor(0x1000));
        {
            let mut e = IREmitter::new(&mut block);
            let a = e.imm32(5);
            let b = e.imm32(3);
            let carry = e.imm1(false);
            let _result = e.add_32(a, b, carry);
        }
        assert_eq!(block.inst_count(), 1);
        assert_eq!(block.get(InstRef(0)).opcode, Opcode::Add32);
    }

    #[test]
    fn test_emitter_vector_ops() {
        let mut block = Block::new(LocationDescriptor(0));
        {
            let mut e = IREmitter::new(&mut block);
            let z = e.zero_vector();
            let _add = e.vector_add(32, z, z);
        }
        assert_eq!(block.inst_count(), 2);
        assert_eq!(block.get(InstRef(0)).opcode, Opcode::ZeroVector);
        assert_eq!(block.get(InstRef(1)).opcode, Opcode::VectorAdd32);
    }

    #[test]
    fn vector_count_leading_zeros_selects_upstream_opcodes() {
        let mut block = Block::new(LocationDescriptor(0));
        {
            let mut e = IREmitter::new(&mut block);
            let z = e.zero_vector();
            let _clz8 = e.vector_count_leading_zeros(8, z);
            let _clz16 = e.vector_count_leading_zeros(16, z);
            let _clz32 = e.vector_count_leading_zeros(32, z);
        }

        assert_eq!(
            block.get(InstRef(1)).opcode,
            Opcode::VectorCountLeadingZeros8
        );
        assert_eq!(
            block.get(InstRef(2)).opcode,
            Opcode::VectorCountLeadingZeros16
        );
        assert_eq!(
            block.get(InstRef(3)).opcode,
            Opcode::VectorCountLeadingZeros32
        );
    }

    #[test]
    fn vector_broadcast_element_selects_upstream_opcodes_and_immediate_index() {
        let mut block = Block::new(LocationDescriptor(0));
        {
            let mut e = IREmitter::new(&mut block);
            let z = e.zero_vector();
            let _lower16 = e.vector_broadcast_element_lower(16, z, 5);
            let _full64 = e.vector_broadcast_element(64, z, 1);
        }

        let lower16 = block.get(InstRef(1));
        assert_eq!(lower16.opcode, Opcode::VectorBroadcastElementLower16);
        assert_eq!(lower16.args[1], Value::ImmU8(5));

        let full64 = block.get(InstRef(2));
        assert_eq!(full64.opcode, Opcode::VectorBroadcastElement64);
        assert_eq!(full64.args[1], Value::ImmU8(1));
    }

    #[test]
    #[should_panic(expected = "Invalid index")]
    fn vector_broadcast_element_rejects_an_out_of_range_index() {
        let mut block = Block::new(LocationDescriptor(0));
        let mut e = IREmitter::new(&mut block);
        let z = e.zero_vector();
        let _ = e.vector_broadcast_element(32, z, 4);
    }

    #[test]
    fn scalar_saturation_helpers_link_overflow_pseudo_operations() {
        let mut block = Block::new(LocationDescriptor(0));
        {
            let mut e = IREmitter::new(&mut block);
            let signed = e.signed_saturation(Value::ImmU32(0x8000_0000), 16);
            let unsigned = e.unsigned_saturation(Value::ImmU32(0xffff_ffff), 8);
            let add = e.signed_saturated_add_with_flag(Value::ImmU32(1), Value::ImmU32(2));
            let sub = e.signed_saturated_sub_with_flag(Value::ImmU32(3), Value::ImmU32(4));

            for pair in [signed, unsigned, add, sub] {
                let result = pair.result.inst_ref();
                let overflow = pair.overflow.inst_ref();
                assert_eq!(block.get(overflow).opcode, Opcode::GetOverflowFromOp);
                assert_eq!(
                    block.get_associated_pseudo_operation(result, Opcode::GetOverflowFromOp),
                    Some(overflow)
                );
            }
        }
    }
}
