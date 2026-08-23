use crate::ir::block::Block;
use crate::ir::location::LocationDescriptor;
use crate::ir::opcode::Opcode;
use crate::ir::terminal::Terminal;
use crate::ir::types::Type;
use crate::ir::value::Value;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResultAndOverflow {
    pub result: Value,
    pub overflow: Value,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResultAndCarry {
    pub result: Value,
    pub carry: Value,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResultAndGE {
    pub result: Value,
    pub ge: Value,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UpperAndLower {
    pub upper: Value,
    pub lower: Value,
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

    fn value_type(&self, value: Value) -> Type {
        match value {
            Value::Inst(inst_ref) => self.block.inst_real_return_type(inst_ref),
            value => value.get_type(),
        }
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

    pub fn most_significant_word(&mut self, value: Value) -> ResultAndCarry {
        let result = self.emit(Opcode::MostSignificantWord, &[value]);
        let carry = self.get_carry_from_op(result);
        ResultAndCarry { result, carry }
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

    pub fn packed_abs_diff_sum_u8(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedAbsDiffSumU8, &[a, b])
    }

    pub fn packed_add_u8(&mut self, a: Value, b: Value) -> ResultAndGE {
        let result = self.emit(Opcode::PackedAddU8, &[a, b]);
        let ge = self.get_ge_from_op(result);
        ResultAndGE { result, ge }
    }

    pub fn packed_add_s8(&mut self, a: Value, b: Value) -> ResultAndGE {
        let result = self.emit(Opcode::PackedAddS8, &[a, b]);
        let ge = self.get_ge_from_op(result);
        ResultAndGE { result, ge }
    }

    pub fn packed_add_u16(&mut self, a: Value, b: Value) -> ResultAndGE {
        let result = self.emit(Opcode::PackedAddU16, &[a, b]);
        let ge = self.get_ge_from_op(result);
        ResultAndGE { result, ge }
    }

    pub fn packed_add_s16(&mut self, a: Value, b: Value) -> ResultAndGE {
        let result = self.emit(Opcode::PackedAddS16, &[a, b]);
        let ge = self.get_ge_from_op(result);
        ResultAndGE { result, ge }
    }

    pub fn packed_sub_u8(&mut self, a: Value, b: Value) -> ResultAndGE {
        let result = self.emit(Opcode::PackedSubU8, &[a, b]);
        let ge = self.get_ge_from_op(result);
        ResultAndGE { result, ge }
    }

    pub fn packed_sub_s8(&mut self, a: Value, b: Value) -> ResultAndGE {
        let result = self.emit(Opcode::PackedSubS8, &[a, b]);
        let ge = self.get_ge_from_op(result);
        ResultAndGE { result, ge }
    }

    pub fn packed_sub_u16(&mut self, a: Value, b: Value) -> ResultAndGE {
        let result = self.emit(Opcode::PackedSubU16, &[a, b]);
        let ge = self.get_ge_from_op(result);
        ResultAndGE { result, ge }
    }

    pub fn packed_sub_s16(&mut self, a: Value, b: Value) -> ResultAndGE {
        let result = self.emit(Opcode::PackedSubS16, &[a, b]);
        let ge = self.get_ge_from_op(result);
        ResultAndGE { result, ge }
    }

    pub fn packed_add_sub_u16(&mut self, a: Value, b: Value) -> ResultAndGE {
        let result = self.emit(Opcode::PackedAddSubU16, &[a, b]);
        let ge = self.get_ge_from_op(result);
        ResultAndGE { result, ge }
    }

    pub fn packed_add_sub_s16(&mut self, a: Value, b: Value) -> ResultAndGE {
        let result = self.emit(Opcode::PackedAddSubS16, &[a, b]);
        let ge = self.get_ge_from_op(result);
        ResultAndGE { result, ge }
    }

    pub fn packed_sub_add_u16(&mut self, a: Value, b: Value) -> ResultAndGE {
        let result = self.emit(Opcode::PackedSubAddU16, &[a, b]);
        let ge = self.get_ge_from_op(result);
        ResultAndGE { result, ge }
    }

    pub fn packed_sub_add_s16(&mut self, a: Value, b: Value) -> ResultAndGE {
        let result = self.emit(Opcode::PackedSubAddS16, &[a, b]);
        let ge = self.get_ge_from_op(result);
        ResultAndGE { result, ge }
    }

    pub fn packed_halving_add_u8(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedHalvingAddU8, &[a, b])
    }

    pub fn packed_halving_add_s8(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedHalvingAddS8, &[a, b])
    }

    pub fn packed_halving_add_u16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedHalvingAddU16, &[a, b])
    }

    pub fn packed_halving_add_s16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedHalvingAddS16, &[a, b])
    }

    pub fn packed_halving_sub_u8(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedHalvingSubU8, &[a, b])
    }

    pub fn packed_halving_sub_s8(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedHalvingSubS8, &[a, b])
    }

    pub fn packed_halving_sub_u16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedHalvingSubU16, &[a, b])
    }

    pub fn packed_halving_sub_s16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedHalvingSubS16, &[a, b])
    }

    pub fn packed_halving_add_sub_u16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedHalvingAddSubU16, &[a, b])
    }

    pub fn packed_halving_add_sub_s16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedHalvingAddSubS16, &[a, b])
    }

    pub fn packed_halving_sub_add_u16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedHalvingSubAddU16, &[a, b])
    }

    pub fn packed_halving_sub_add_s16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedHalvingSubAddS16, &[a, b])
    }

    pub fn packed_saturated_add_u8(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedSaturatedAddU8, &[a, b])
    }

    pub fn packed_saturated_add_s8(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedSaturatedAddS8, &[a, b])
    }

    pub fn packed_saturated_add_u16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedSaturatedAddU16, &[a, b])
    }

    pub fn packed_saturated_add_s16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedSaturatedAddS16, &[a, b])
    }

    pub fn packed_saturated_sub_u8(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedSaturatedSubU8, &[a, b])
    }

    pub fn packed_saturated_sub_s8(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedSaturatedSubS8, &[a, b])
    }

    pub fn packed_saturated_sub_u16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedSaturatedSubU16, &[a, b])
    }

    pub fn packed_saturated_sub_s16(&mut self, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedSaturatedSubS16, &[a, b])
    }

    pub fn packed_select(&mut self, ge: Value, a: Value, b: Value) -> Value {
        self.emit(Opcode::PackedSelect, &[ge, a, b])
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

    pub fn sign_extend_to_long(&mut self, a: Value) -> Value {
        match self.value_type(a) {
            Type::U8 => self.sign_extend_byte_to_long(a),
            Type::U16 => self.sign_extend_half_to_long(a),
            Type::U32 => self.sign_extend_word_to_long(a),
            Type::U64 => a,
            ty => panic!("Cannot sign-extend {ty:?} to U64"),
        }
    }

    pub fn sign_extend_to_word(&mut self, a: Value) -> Value {
        match self.value_type(a) {
            Type::U8 => self.sign_extend_byte_to_word(a),
            Type::U16 => self.sign_extend_half_to_word(a),
            Type::U32 => a,
            Type::U64 => self.least_significant_word(a),
            ty => panic!("Cannot sign-extend {ty:?} to U32"),
        }
    }

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

    pub fn zero_extend_to_long(&mut self, a: Value) -> Value {
        match self.value_type(a) {
            Type::U8 => self.zero_extend_byte_to_long(a),
            Type::U16 => self.zero_extend_half_to_long(a),
            Type::U32 => self.zero_extend_word_to_long(a),
            Type::U64 => a,
            ty => panic!("Cannot zero-extend {ty:?} to U64"),
        }
    }

    pub fn zero_extend_to_word(&mut self, a: Value) -> Value {
        match self.value_type(a) {
            Type::U8 => self.zero_extend_byte_to_word(a),
            Type::U16 => self.zero_extend_half_to_word(a),
            Type::U32 => a,
            Type::U64 => self.least_significant_word(a),
            ty => panic!("Cannot zero-extend {ty:?} to U32"),
        }
    }

    pub fn zero_extend_long_to_quad(&mut self, a: Value) -> Value {
        self.emit(Opcode::ZeroExtendLongToQuad, &[a])
    }

    pub fn zero_extend_to_quad(&mut self, a: Value) -> Value {
        let extended = self.zero_extend_to_long(a);
        self.zero_extend_long_to_quad(extended)
    }

    pub fn indeterminate_extend_to_word(&mut self, a: Value) -> Value {
        self.zero_extend_to_word(a)
    }

    pub fn indeterminate_extend_to_long(&mut self, a: Value) -> Value {
        self.zero_extend_to_long(a)
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

    pub fn signed_saturated_add(&mut self, a: Value, b: Value) -> Value {
        let value_type = self.value_type(a);
        assert_eq!(value_type, self.value_type(b));
        let opcode = match value_type {
            Type::U8 => Opcode::SignedSaturatedAdd8,
            Type::U16 => Opcode::SignedSaturatedAdd16,
            Type::U32 => Opcode::SignedSaturatedAdd32,
            Type::U64 => Opcode::SignedSaturatedAdd64,
            ty => panic!("Cannot perform signed saturated add on {ty:?}"),
        };
        self.emit(opcode, &[a, b])
    }

    pub fn signed_saturated_doubling_multiply_return_high(&mut self, a: Value, b: Value) -> Value {
        let value_type = self.value_type(a);
        assert_eq!(value_type, self.value_type(b));
        let opcode = match value_type {
            Type::U16 => Opcode::SignedSaturatedDoublingMultiplyReturnHigh16,
            Type::U32 => Opcode::SignedSaturatedDoublingMultiplyReturnHigh32,
            ty => panic!("Cannot perform saturated doubling multiply-high on {ty:?}"),
        };
        self.emit(opcode, &[a, b])
    }

    pub fn signed_saturated_sub(&mut self, a: Value, b: Value) -> Value {
        let value_type = self.value_type(a);
        assert_eq!(value_type, self.value_type(b));
        let opcode = match value_type {
            Type::U8 => Opcode::SignedSaturatedSub8,
            Type::U16 => Opcode::SignedSaturatedSub16,
            Type::U32 => Opcode::SignedSaturatedSub32,
            Type::U64 => Opcode::SignedSaturatedSub64,
            ty => panic!("Cannot perform signed saturated sub on {ty:?}"),
        };
        self.emit(opcode, &[a, b])
    }

    pub fn unsigned_saturated_add(&mut self, a: Value, b: Value) -> Value {
        let value_type = self.value_type(a);
        assert_eq!(value_type, self.value_type(b));
        let opcode = match value_type {
            Type::U8 => Opcode::UnsignedSaturatedAdd8,
            Type::U16 => Opcode::UnsignedSaturatedAdd16,
            Type::U32 => Opcode::UnsignedSaturatedAdd32,
            Type::U64 => Opcode::UnsignedSaturatedAdd64,
            ty => panic!("Cannot perform unsigned saturated add on {ty:?}"),
        };
        self.emit(opcode, &[a, b])
    }

    pub fn unsigned_saturated_sub(&mut self, a: Value, b: Value) -> Value {
        let value_type = self.value_type(a);
        assert_eq!(value_type, self.value_type(b));
        let opcode = match value_type {
            Type::U8 => Opcode::UnsignedSaturatedSub8,
            Type::U16 => Opcode::UnsignedSaturatedSub16,
            Type::U32 => Opcode::UnsignedSaturatedSub32,
            Type::U64 => Opcode::UnsignedSaturatedSub64,
            ty => panic!("Cannot perform unsigned saturated sub on {ty:?}"),
        };
        self.emit(opcode, &[a, b])
    }

    // --- Flags ---

    pub fn get_carry_from_op(&mut self, value: Value) -> Value {
        self.emit_pseudo_op(Opcode::GetCarryFromOp, value)
    }

    pub fn get_overflow_from_op(&mut self, value: Value) -> Value {
        self.emit_pseudo_op(Opcode::GetOverflowFromOp, value)
    }

    pub fn get_ge_from_op(&mut self, value: Value) -> Value {
        self.emit_pseudo_op(Opcode::GetGEFromOp, value)
    }

    pub fn get_nzcv_from_op(&mut self, value: Value) -> Value {
        self.emit_pseudo_op(Opcode::GetNZCVFromOp, value)
    }

    pub fn get_upper_from_op(&mut self, value: Value) -> Value {
        self.emit_pseudo_op(Opcode::GetUpperFromOp, value)
    }

    pub fn get_lower_from_op(&mut self, value: Value) -> Value {
        self.emit_pseudo_op(Opcode::GetLowerFromOp, value)
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

    pub fn vector_signed_multiply(&mut self, esize: usize, a: Value, b: Value) -> UpperAndLower {
        let opcode = match esize {
            16 => Opcode::VectorSignedMultiply16,
            32 => Opcode::VectorSignedMultiply32,
            _ => panic!("Invalid esize {}", esize),
        };
        let multiply = self.emit(opcode, &[a, b]);
        UpperAndLower {
            upper: self.get_upper_from_op(multiply),
            lower: self.get_lower_from_op(multiply),
        }
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
        let greater = self.vector_greater_signed(esize, a, b);
        let equal = self.vector_equal(esize, a, b);
        self.vector_or(greater, equal)
    }

    pub fn vector_greater_equal_unsigned(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let max = self.vector_max_unsigned(esize, a, b);
        self.vector_equal(esize, max, a)
    }

    pub fn vector_greater_unsigned(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let min = self.vector_min_unsigned(esize, a, b);
        let equal = self.vector_equal(esize, min, a);
        self.vector_not(equal)
    }

    pub fn vector_less_equal_signed(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let greater = self.vector_greater_signed(esize, a, b);
        self.vector_not(greater)
    }

    pub fn vector_less_equal_unsigned(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let min = self.vector_min_unsigned(esize, a, b);
        self.vector_equal(esize, min, a)
    }

    pub fn vector_less_signed(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let greater = self.vector_greater_signed(esize, a, b);
        let equal = self.vector_equal(esize, a, b);
        let greater_or_equal = self.vector_or(greater, equal);
        self.vector_not(greater_or_equal)
    }

    pub fn vector_less_unsigned(&mut self, esize: usize, a: Value, b: Value) -> Value {
        let max = self.vector_max_unsigned(esize, a, b);
        let equal = self.vector_equal(esize, max, a);
        self.vector_not(equal)
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
        let op = match esize {
            8 => Opcode::VectorSignedSaturatedShiftLeftUnsigned8,
            16 => Opcode::VectorSignedSaturatedShiftLeftUnsigned16,
            32 => Opcode::VectorSignedSaturatedShiftLeftUnsigned32,
            64 => Opcode::VectorSignedSaturatedShiftLeftUnsigned64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a, Value::ImmU8(shift)])
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

    pub fn vector_reduce_add(&mut self, esize: usize, a: Value) -> Value {
        let op = match esize {
            8 => Opcode::VectorReduceAdd8,
            16 => Opcode::VectorReduceAdd16,
            32 => Opcode::VectorReduceAdd32,
            64 => Opcode::VectorReduceAdd64,
            _ => panic!("Invalid esize {}", esize),
        };
        self.emit(op, &[a])
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
    fn generic_extension_helpers_select_upstream_opcodes_by_input_type() {
        let mut block = Block::new(LocationDescriptor(0));
        let (
            zero_byte,
            zero_half,
            zero_word,
            zero_long,
            sign_byte,
            sign_half,
            sign_word,
            sign_long,
        );
        {
            let mut e = IREmitter::new(&mut block);
            zero_byte = e.zero_extend_to_long(Value::ImmU8(1));
            zero_half = e.zero_extend_to_long(Value::ImmU16(2));
            zero_word = e.zero_extend_to_long(Value::ImmU32(3));
            zero_long = e.zero_extend_to_long(Value::ImmU64(4));
            sign_byte = e.sign_extend_to_word(Value::ImmU8(5));
            sign_half = e.sign_extend_to_word(Value::ImmU16(6));
            sign_word = e.sign_extend_to_word(Value::ImmU32(7));
            sign_long = e.sign_extend_to_word(Value::ImmU64(8));
        }

        assert_eq!(
            block.get(zero_byte.inst_ref()).opcode,
            Opcode::ZeroExtendByteToLong
        );
        assert_eq!(
            block.get(zero_half.inst_ref()).opcode,
            Opcode::ZeroExtendHalfToLong
        );
        assert_eq!(
            block.get(zero_word.inst_ref()).opcode,
            Opcode::ZeroExtendWordToLong
        );
        assert_eq!(zero_long, Value::ImmU64(4));
        assert_eq!(
            block.get(sign_byte.inst_ref()).opcode,
            Opcode::SignExtendByteToWord
        );
        assert_eq!(
            block.get(sign_half.inst_ref()).opcode,
            Opcode::SignExtendHalfToWord
        );
        assert_eq!(sign_word, Value::ImmU32(7));
        assert_eq!(
            block.get(sign_long.inst_ref()).opcode,
            Opcode::LeastSignificantWord
        );
    }

    #[test]
    fn zero_extend_to_quad_first_extends_narrow_inputs_to_long() {
        let mut block = Block::new(LocationDescriptor(0));
        let (byte, half, word, long);
        {
            let mut e = IREmitter::new(&mut block);
            byte = e.zero_extend_to_quad(Value::ImmU8(1));
            half = e.zero_extend_to_quad(Value::ImmU16(2));
            word = e.zero_extend_to_quad(Value::ImmU32(3));
            long = e.zero_extend_to_quad(Value::ImmU64(4));
        }

        for (result, expected_inner) in [
            (byte, Opcode::ZeroExtendByteToLong),
            (half, Opcode::ZeroExtendHalfToLong),
            (word, Opcode::ZeroExtendWordToLong),
        ] {
            let outer = block.get(result.inst_ref());
            assert_eq!(outer.opcode, Opcode::ZeroExtendLongToQuad);
            let inner = block.get(outer.args[0].inst_ref());
            assert_eq!(inner.opcode, expected_inner);
        }

        let outer = block.get(long.inst_ref());
        assert_eq!(outer.opcode, Opcode::ZeroExtendLongToQuad);
        assert_eq!(outer.args[0], Value::ImmU64(4));
    }

    #[test]
    fn signed_saturating_shift_left_unsigned_uses_upstream_u8_shift_operand() {
        let mut block = Block::new(LocationDescriptor(0));
        let result;
        {
            let mut e = IREmitter::new(&mut block);
            let operand = e.zero_vector();
            result = e.vector_signed_saturated_shift_left_unsigned(16, operand, 7);
        }

        let inst = block.get(result.inst_ref());
        assert_eq!(
            inst.opcode,
            Opcode::VectorSignedSaturatedShiftLeftUnsigned16
        );
        assert_eq!(inst.args[1], Value::ImmU8(7));
        assert_eq!(block.inst_count(), 2);
    }

    #[test]
    fn scalar_saturated_builders_select_upstream_opcodes_from_operand_type() {
        let mut block = Block::new(LocationDescriptor(0));
        {
            let mut e = IREmitter::new(&mut block);
            for (a, b) in [
                (Value::ImmU8(1), Value::ImmU8(2)),
                (Value::ImmU16(1), Value::ImmU16(2)),
                (Value::ImmU32(1), Value::ImmU32(2)),
                (Value::ImmU64(1), Value::ImmU64(2)),
            ] {
                e.signed_saturated_add(a, b);
                e.signed_saturated_sub(a, b);
                e.unsigned_saturated_add(a, b);
                e.unsigned_saturated_sub(a, b);
            }
            e.signed_saturated_doubling_multiply_return_high(Value::ImmU16(3), Value::ImmU16(4));
            e.signed_saturated_doubling_multiply_return_high(Value::ImmU32(5), Value::ImmU32(6));
        }

        let opcodes: std::vec::Vec<_> = block.instructions.iter().map(|inst| inst.opcode).collect();
        assert_eq!(
            opcodes,
            vec![
                Opcode::SignedSaturatedAdd8,
                Opcode::SignedSaturatedSub8,
                Opcode::UnsignedSaturatedAdd8,
                Opcode::UnsignedSaturatedSub8,
                Opcode::SignedSaturatedAdd16,
                Opcode::SignedSaturatedSub16,
                Opcode::UnsignedSaturatedAdd16,
                Opcode::UnsignedSaturatedSub16,
                Opcode::SignedSaturatedAdd32,
                Opcode::SignedSaturatedSub32,
                Opcode::UnsignedSaturatedAdd32,
                Opcode::UnsignedSaturatedSub32,
                Opcode::SignedSaturatedAdd64,
                Opcode::SignedSaturatedSub64,
                Opcode::UnsignedSaturatedAdd64,
                Opcode::UnsignedSaturatedSub64,
                Opcode::SignedSaturatedDoublingMultiplyReturnHigh16,
                Opcode::SignedSaturatedDoublingMultiplyReturnHigh32,
            ]
        );
    }

    #[test]
    fn vector_signed_comparisons_expand_in_edens_exact_order() {
        for (esize, greater, equal) in [
            (8, Opcode::VectorGreaterS8, Opcode::VectorEqual8),
            (16, Opcode::VectorGreaterS16, Opcode::VectorEqual16),
            (32, Opcode::VectorGreaterS32, Opcode::VectorEqual32),
            (64, Opcode::VectorGreaterS64, Opcode::VectorEqual64),
        ] {
            let mut greater_equal = Block::new(LocationDescriptor(0));
            {
                let mut e = IREmitter::new(&mut greater_equal);
                let value = e.zero_vector();
                let _ = e.vector_greater_equal_signed(esize, value, value);
            }
            assert_eq!(greater_equal.inst_count(), 4);
            assert_eq!(greater_equal.get(InstRef(1)).opcode, greater);
            assert_eq!(greater_equal.get(InstRef(2)).opcode, equal);
            assert_eq!(greater_equal.get(InstRef(3)).opcode, Opcode::VectorOr);
            assert_eq!(
                &greater_equal.get(InstRef(3)).args[..2],
                &[Value::Inst(InstRef(1)), Value::Inst(InstRef(2))]
            );

            let mut less_equal = Block::new(LocationDescriptor(0));
            {
                let mut e = IREmitter::new(&mut less_equal);
                let value = e.zero_vector();
                let _ = e.vector_less_equal_signed(esize, value, value);
            }
            assert_eq!(less_equal.inst_count(), 3);
            assert_eq!(less_equal.get(InstRef(1)).opcode, greater);
            assert_eq!(less_equal.get(InstRef(2)).opcode, Opcode::VectorNot);
            assert_eq!(less_equal.get(InstRef(2)).args[0], Value::Inst(InstRef(1)));

            let mut less = Block::new(LocationDescriptor(0));
            {
                let mut e = IREmitter::new(&mut less);
                let value = e.zero_vector();
                let _ = e.vector_less_signed(esize, value, value);
            }
            assert_eq!(less.inst_count(), 5);
            assert_eq!(less.get(InstRef(1)).opcode, greater);
            assert_eq!(less.get(InstRef(2)).opcode, equal);
            assert_eq!(less.get(InstRef(3)).opcode, Opcode::VectorOr);
            assert_eq!(less.get(InstRef(4)).opcode, Opcode::VectorNot);
            assert_eq!(less.get(InstRef(4)).args[0], Value::Inst(InstRef(3)));
        }
    }

    #[test]
    fn vector_unsigned_comparisons_expand_in_edens_exact_order() {
        for (esize, min, max, equal) in [
            (
                8,
                Opcode::VectorMinU8,
                Opcode::VectorMaxU8,
                Opcode::VectorEqual8,
            ),
            (
                16,
                Opcode::VectorMinU16,
                Opcode::VectorMaxU16,
                Opcode::VectorEqual16,
            ),
            (
                32,
                Opcode::VectorMinU32,
                Opcode::VectorMaxU32,
                Opcode::VectorEqual32,
            ),
            (
                64,
                Opcode::VectorMinU64,
                Opcode::VectorMaxU64,
                Opcode::VectorEqual64,
            ),
        ] {
            for (greater_equal, first) in [(true, max), (false, min)] {
                let mut block = Block::new(LocationDescriptor(0));
                {
                    let mut e = IREmitter::new(&mut block);
                    let value = e.zero_vector();
                    if greater_equal {
                        let _ = e.vector_greater_equal_unsigned(esize, value, value);
                    } else {
                        let _ = e.vector_less_equal_unsigned(esize, value, value);
                    }
                }
                assert_eq!(block.inst_count(), 3);
                assert_eq!(block.get(InstRef(1)).opcode, first);
                assert_eq!(block.get(InstRef(2)).opcode, equal);
                assert_eq!(
                    &block.get(InstRef(2)).args[..2],
                    &[Value::Inst(InstRef(1)), Value::Inst(InstRef(0))]
                );
            }

            for (greater, first) in [(true, min), (false, max)] {
                let mut block = Block::new(LocationDescriptor(0));
                {
                    let mut e = IREmitter::new(&mut block);
                    let value = e.zero_vector();
                    if greater {
                        let _ = e.vector_greater_unsigned(esize, value, value);
                    } else {
                        let _ = e.vector_less_unsigned(esize, value, value);
                    }
                }
                assert_eq!(block.inst_count(), 4);
                assert_eq!(block.get(InstRef(1)).opcode, first);
                assert_eq!(block.get(InstRef(2)).opcode, equal);
                assert_eq!(block.get(InstRef(3)).opcode, Opcode::VectorNot);
                assert_eq!(block.get(InstRef(3)).args[0], Value::Inst(InstRef(2)));
            }
        }
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
    fn vector_reduce_add_selects_the_four_upstream_opcodes() {
        let mut block = Block::new(LocationDescriptor(0));
        {
            let mut e = IREmitter::new(&mut block);
            let z = e.zero_vector();
            for esize in [8, 16, 32, 64] {
                let _ = e.vector_reduce_add(esize, z);
            }
        }

        let expected = [
            Opcode::VectorReduceAdd8,
            Opcode::VectorReduceAdd16,
            Opcode::VectorReduceAdd32,
            Opcode::VectorReduceAdd64,
        ];
        for (index, opcode) in expected.into_iter().enumerate() {
            assert_eq!(block.get(InstRef(index as u32 + 1)).opcode, opcode);
        }
    }

    #[test]
    fn vector_signed_multiply_builds_edens_upper_and_lower_results() {
        let mut block = Block::new(LocationDescriptor(0));
        let (result16, result32);
        {
            let mut e = IREmitter::new(&mut block);
            let a = e.zero_vector();
            let b = e.zero_vector();
            result16 = e.vector_signed_multiply(16, a, b);
            result32 = e.vector_signed_multiply(32, a, b);
        }
        let multiply16 = block.get(result16.upper.inst_ref()).args[0].inst_ref();
        let multiply32 = block.get(result32.upper.inst_ref()).args[0].inst_ref();

        for (multiply, result, opcode) in [
            (multiply16, result16, Opcode::VectorSignedMultiply16),
            (multiply32, result32, Opcode::VectorSignedMultiply32),
        ] {
            assert_eq!(block.get(multiply).opcode, opcode);
            assert_eq!(
                block.get_associated_pseudo_operation(multiply, Opcode::GetUpperFromOp),
                Some(result.upper.inst_ref())
            );
            assert_eq!(
                block.get_associated_pseudo_operation(multiply, Opcode::GetLowerFromOp),
                Some(result.lower.inst_ref())
            );
        }
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

    #[test]
    fn packed_abs_diff_sum_u8_emits_upstream_opcode_and_operand_order() {
        let mut block = Block::new(LocationDescriptor(0));
        let result;
        {
            let mut e = IREmitter::new(&mut block);
            result =
                e.packed_abs_diff_sum_u8(Value::ImmU32(0x0102_0304), Value::ImmU32(0x0506_0708));
        }

        let inst = block.get(result.inst_ref());
        assert_eq!(inst.opcode, Opcode::PackedAbsDiffSumU8);
        assert_eq!(inst.args[0], Value::ImmU32(0x0102_0304));
        assert_eq!(inst.args[1], Value::ImmU32(0x0506_0708));
    }

    #[test]
    fn packed_add_u16_returns_and_links_upstream_ge_pseudo_result() {
        let mut block = Block::new(LocationDescriptor(0));
        let pair;
        {
            let mut e = IREmitter::new(&mut block);
            pair = e.packed_add_u16(Value::ImmU32(0x0001_0002), Value::ImmU32(0x0003_0004));
        }

        let result = pair.result.inst_ref();
        let ge = pair.ge.inst_ref();
        assert_eq!(block.get(result).opcode, Opcode::PackedAddU16);
        assert_eq!(block.get(result).args[0], Value::ImmU32(0x0001_0002));
        assert_eq!(block.get(result).args[1], Value::ImmU32(0x0003_0004));
        assert_eq!(block.get(ge).opcode, Opcode::GetGEFromOp);
        assert_eq!(
            block.get_associated_pseudo_operation(result, Opcode::GetGEFromOp),
            Some(ge)
        );
    }

    #[test]
    fn packed_parallel_builder_surface_matches_upstream_opcodes_and_ge_results() {
        let mut block = Block::new(LocationDescriptor(0));
        let (ge_pairs, plain_values);
        {
            let mut e = IREmitter::new(&mut block);
            let a = Value::ImmU32(0x0102_0304);
            let b = Value::ImmU32(0x0506_0708);
            ge_pairs = vec![
                (e.packed_add_u8(a, b), Opcode::PackedAddU8),
                (e.packed_add_s8(a, b), Opcode::PackedAddS8),
                (e.packed_add_u16(a, b), Opcode::PackedAddU16),
                (e.packed_add_s16(a, b), Opcode::PackedAddS16),
                (e.packed_sub_u8(a, b), Opcode::PackedSubU8),
                (e.packed_sub_s8(a, b), Opcode::PackedSubS8),
                (e.packed_sub_u16(a, b), Opcode::PackedSubU16),
                (e.packed_sub_s16(a, b), Opcode::PackedSubS16),
                (e.packed_add_sub_u16(a, b), Opcode::PackedAddSubU16),
                (e.packed_add_sub_s16(a, b), Opcode::PackedAddSubS16),
                (e.packed_sub_add_u16(a, b), Opcode::PackedSubAddU16),
                (e.packed_sub_add_s16(a, b), Opcode::PackedSubAddS16),
            ];
            plain_values = vec![
                (e.packed_halving_add_u8(a, b), Opcode::PackedHalvingAddU8),
                (e.packed_halving_add_s8(a, b), Opcode::PackedHalvingAddS8),
                (e.packed_halving_add_u16(a, b), Opcode::PackedHalvingAddU16),
                (e.packed_halving_add_s16(a, b), Opcode::PackedHalvingAddS16),
                (e.packed_halving_sub_u8(a, b), Opcode::PackedHalvingSubU8),
                (e.packed_halving_sub_s8(a, b), Opcode::PackedHalvingSubS8),
                (e.packed_halving_sub_u16(a, b), Opcode::PackedHalvingSubU16),
                (e.packed_halving_sub_s16(a, b), Opcode::PackedHalvingSubS16),
                (
                    e.packed_halving_add_sub_u16(a, b),
                    Opcode::PackedHalvingAddSubU16,
                ),
                (
                    e.packed_halving_add_sub_s16(a, b),
                    Opcode::PackedHalvingAddSubS16,
                ),
                (
                    e.packed_halving_sub_add_u16(a, b),
                    Opcode::PackedHalvingSubAddU16,
                ),
                (
                    e.packed_halving_sub_add_s16(a, b),
                    Opcode::PackedHalvingSubAddS16,
                ),
                (
                    e.packed_saturated_add_u8(a, b),
                    Opcode::PackedSaturatedAddU8,
                ),
                (
                    e.packed_saturated_add_s8(a, b),
                    Opcode::PackedSaturatedAddS8,
                ),
                (
                    e.packed_saturated_add_u16(a, b),
                    Opcode::PackedSaturatedAddU16,
                ),
                (
                    e.packed_saturated_add_s16(a, b),
                    Opcode::PackedSaturatedAddS16,
                ),
                (
                    e.packed_saturated_sub_u8(a, b),
                    Opcode::PackedSaturatedSubU8,
                ),
                (
                    e.packed_saturated_sub_s8(a, b),
                    Opcode::PackedSaturatedSubS8,
                ),
                (
                    e.packed_saturated_sub_u16(a, b),
                    Opcode::PackedSaturatedSubU16,
                ),
                (
                    e.packed_saturated_sub_s16(a, b),
                    Opcode::PackedSaturatedSubS16,
                ),
            ];
        }

        for (pair, opcode) in ge_pairs {
            let producer = pair.result.inst_ref();
            let ge = pair.ge.inst_ref();
            assert_eq!(block.get(producer).opcode, opcode);
            assert_eq!(block.get(producer).args[0], Value::ImmU32(0x0102_0304));
            assert_eq!(block.get(producer).args[1], Value::ImmU32(0x0506_0708));
            assert_eq!(block.get(ge).opcode, Opcode::GetGEFromOp);
            assert_eq!(
                block.get_associated_pseudo_operation(producer, Opcode::GetGEFromOp),
                Some(ge)
            );
        }
        for (value, opcode) in plain_values {
            let inst = block.get(value.inst_ref());
            assert_eq!(inst.opcode, opcode);
            assert_eq!(inst.args[0], Value::ImmU32(0x0102_0304));
            assert_eq!(inst.args[1], Value::ImmU32(0x0506_0708));
        }
    }

    #[test]
    fn packed_select_emits_upstream_operand_order() {
        let mut block = Block::new(LocationDescriptor(0));
        let result;
        {
            let mut e = IREmitter::new(&mut block);
            result = e.packed_select(
                Value::ImmU32(0x00ff_00ff),
                Value::ImmU32(0x1111_1111),
                Value::ImmU32(0x2222_2222),
            );
        }

        let inst = block.get(result.inst_ref());
        assert_eq!(inst.opcode, Opcode::PackedSelect);
        assert_eq!(inst.args[0], Value::ImmU32(0x00ff_00ff));
        assert_eq!(inst.args[1], Value::ImmU32(0x1111_1111));
        assert_eq!(inst.args[2], Value::ImmU32(0x2222_2222));
    }

    #[test]
    fn most_significant_word_returns_and_links_upstream_carry_pseudo_result() {
        let mut block = Block::new(LocationDescriptor(0));
        let pair;
        {
            let mut e = IREmitter::new(&mut block);
            pair = e.most_significant_word(Value::ImmU64(0x1234_5678_9abc_def0));
        }

        let result = pair.result.inst_ref();
        let carry = pair.carry.inst_ref();
        assert_eq!(block.get(result).opcode, Opcode::MostSignificantWord);
        assert_eq!(block.get(carry).opcode, Opcode::GetCarryFromOp);
        assert_eq!(
            block.get_associated_pseudo_operation(result, Opcode::GetCarryFromOp),
            Some(carry)
        );
    }

    #[test]
    fn upper_and_lower_pseudo_operations_link_to_their_producer() {
        let mut block = Block::new(LocationDescriptor(0));
        let (producer, upper, lower);
        {
            let mut e = IREmitter::new(&mut block);
            producer = e.emit(Opcode::Void, &[]);
            upper = e.get_upper_from_op(producer);
            lower = e.get_lower_from_op(producer);
        }

        let producer = producer.inst_ref();
        let upper = upper.inst_ref();
        let lower = lower.inst_ref();
        assert_eq!(block.get(upper).opcode, Opcode::GetUpperFromOp);
        assert_eq!(block.get(lower).opcode, Opcode::GetLowerFromOp);
        assert_eq!(
            block.get_associated_pseudo_operation(producer, Opcode::GetUpperFromOp),
            Some(upper)
        );
        assert_eq!(
            block.get_associated_pseudo_operation(producer, Opcode::GetLowerFromOp),
            Some(lower)
        );
        assert_eq!(block.get(upper).next_pseudoop, Some(lower));
    }
}
