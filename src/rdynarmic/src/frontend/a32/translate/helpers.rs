use crate::frontend::a32::types::{Reg, ShiftType};
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::value::Value;

/// Returns whether an instruction which writes `PC` is forbidden at the
/// current position in a Thumb IT block.
///
/// Upstream owner: `frontend/A32/translate/impl/common.h::ITBlockCheck`.
pub(crate) fn it_block_check(ir: &A32IREmitter<'_>) -> bool {
    let it = ir.current_location.expect("current_location not set").it();
    it.is_in_it_block() && !it.is_last_in_it_block()
}

/// Pack the low 16 bits of `lo` and `hi` into one 32-bit value.
/// Upstream: `translate/impl/common.h::Pack2x16To1x32`.
pub fn pack_2x16_to_1x32(ir: &mut A32IREmitter, lo: Value, hi: Value) -> Value {
    let lo = ir.ir().and_32(lo, Value::ImmU32(0xffff));
    let hi = ir
        .ir()
        .logical_shift_left_32(hi, Value::ImmU8(16), Value::ImmU1(false));
    ir.ir().or_32(lo, hi)
}

/// Extract the upper halfword from a 32-bit value.
/// Upstream: `translate/impl/common.h::MostSignificantHalf`.
pub fn most_significant_half(ir: &mut A32IREmitter, value: Value) -> Value {
    let shifted = ir
        .ir()
        .logical_shift_right_32(value, Value::ImmU8(16), Value::ImmU1(false));
    ir.ir().least_significant_half(shifted)
}

/// Rotate the source register by the encoded sign-extension rotation.
/// Upstream: `translate/impl/common.h::Rotate`.
pub fn rotate(ir: &mut A32IREmitter<'_>, m: Reg, rotate: u32) -> Value {
    let rotate_by = (rotate * 8) as u8;
    let reg_m = ir.get_register(m);
    ir.ir()
        .rotate_right_32(reg_m, Value::ImmU8(rotate_by), Value::ImmU1(false))
}

/// Apply an immediate shift to a register value, returning (result, carry_out).
pub fn emit_imm_shift(
    ir: &mut A32IREmitter,
    value: Value,
    shift_type: ShiftType,
    imm5: u32,
    carry_in: Value,
) -> (Value, Value) {
    match shift_type {
        ShiftType::LSL => {
            if imm5 == 0 {
                (value, carry_in)
            } else {
                let result =
                    ir.ir()
                        .logical_shift_left_32(value, Value::ImmU8(imm5 as u8), carry_in);
                let carry = ir.ir().get_carry_from_op(result);
                (result, carry)
            }
        }
        ShiftType::LSR => {
            let shift = if imm5 == 0 { 32 } else { imm5 };
            let result = ir
                .ir()
                .logical_shift_right_32(value, Value::ImmU8(shift as u8), carry_in);
            let carry = ir.ir().get_carry_from_op(result);
            (result, carry)
        }
        ShiftType::ASR => {
            let shift = if imm5 == 0 { 32 } else { imm5 };
            let result =
                ir.ir()
                    .arithmetic_shift_right_32(value, Value::ImmU8(shift as u8), carry_in);
            let carry = ir.ir().get_carry_from_op(result);
            (result, carry)
        }
        ShiftType::ROR => {
            if imm5 == 0 {
                // RRX: rotate right extended
                let result = ir.ir().rotate_right_extended(value, carry_in);
                let carry = ir.ir().get_carry_from_op(result);
                (result, carry)
            } else {
                let result = ir
                    .ir()
                    .rotate_right_32(value, Value::ImmU8(imm5 as u8), carry_in);
                let carry = ir.ir().get_carry_from_op(result);
                (result, carry)
            }
        }
    }
}

/// Apply a register-specified shift to a value, returning (result, carry_out).
pub fn emit_reg_shift(
    ir: &mut A32IREmitter,
    value: Value,
    shift_type: ShiftType,
    amount: Value,
    carry_in: Value,
) -> (Value, Value) {
    let result = match shift_type {
        ShiftType::LSL => ir.ir().logical_shift_left_32(value, amount, carry_in),
        ShiftType::LSR => ir.ir().logical_shift_right_32(value, amount, carry_in),
        ShiftType::ASR => ir.ir().arithmetic_shift_right_32(value, amount, carry_in),
        ShiftType::ROR => ir.ir().rotate_right_32(value, amount, carry_in),
    };
    let carry = ir.ir().get_carry_from_op(result);
    (result, carry)
}

/// Compute load/store address with P/U/W flags.
/// P = pre-index, U = add, W = writeback.
pub fn get_address(
    ir: &mut A32IREmitter,
    p: bool,
    u: bool,
    w: bool,
    base_reg: crate::frontend::a32::types::Reg,
    offset: Value,
) -> Value {
    let base = ir.get_register(base_reg);
    let carry = ir.ir().imm1(false);

    let offset_addr = if u {
        ir.ir().add_32(base, offset, carry)
    } else {
        ir.ir().sub_32(base, offset, Value::ImmU1(true))
    };

    let address = if p { offset_addr } else { base };

    // Writeback: update base register
    let wback = !p || w;
    if wback {
        ir.set_register(base_reg, offset_addr);
    }

    address
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::it_state::ITState;
    use crate::frontend::a32::psr::PSR;
    use crate::ir::block::Block;
    use crate::ir::location::{A32LocationDescriptor, LocationDescriptor};
    use crate::ir::opcode::Opcode;

    #[test]
    fn common_halfword_helpers_preserve_upstream_ir_order() {
        let mut block = Block::new(LocationDescriptor(0));
        let mut ir = A32IREmitter::new(&mut block);

        let packed = pack_2x16_to_1x32(
            &mut ir,
            Value::ImmU32(0xaaaa_5555),
            Value::ImmU32(0xbbbb_6666),
        );
        let upper = most_significant_half(&mut ir, Value::ImmU32(0x1234_5678));

        assert_eq!(block.get(packed.inst_ref()).opcode, Opcode::Or32);
        assert_eq!(
            block.get(upper.inst_ref()).opcode,
            Opcode::LeastSignificantHalf
        );
        assert_eq!(
            block.get(crate::ir::value::InstRef(0)).opcode,
            Opcode::And32
        );
        assert_eq!(
            block.get(crate::ir::value::InstRef(1)).opcode,
            Opcode::LogicalShiftLeft32
        );
        assert_eq!(
            block.get(crate::ir::value::InstRef(3)).opcode,
            Opcode::LogicalShiftRight32
        );
    }

    #[test]
    fn rotate_emits_ror_even_for_zero_rotation() {
        let mut block = Block::new(LocationDescriptor(0));
        {
            let mut ir = A32IREmitter::new(&mut block);
            let _ = rotate(&mut ir, Reg::R3, 0);
        }

        assert_eq!(block.instructions[0].opcode, Opcode::A32GetRegister);
        assert_eq!(block.instructions[0].args[0], Value::ImmA32Reg(Reg::R3));
        assert_eq!(block.instructions[1].opcode, Opcode::BitRotateRight32);
        assert_eq!(block.instructions[1].args[1], Value::ImmU8(0));
        assert_eq!(block.instructions[1].args[2], Value::ImmU1(false));
    }

    #[test]
    fn it_block_check_only_rejects_nonfinal_it_positions() {
        for (state, expected) in [(0x00, false), (0x08, false), (0x0c, true)] {
            let location =
                A32LocationDescriptor::new(0x1000, PSR::default(), FPSCR::default(), false)
                    .set_it(ITState::new(state));
            let mut block = Block::new(location.to_location());
            let ir = A32IREmitter::with_location(&mut block, location);
            assert_eq!(it_block_check(&ir), expected, "IT state {state:02x}");
        }
    }
}
