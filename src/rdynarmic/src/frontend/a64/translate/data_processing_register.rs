use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Reg;
use crate::ir::{IREmitter, Value};

fn rbit32(ir: &mut IREmitter<'_>, operand: Value) -> Value {
    let first_and = ir.and_32(operand, Value::ImmU32(0x5555_5555));
    let first_lsl = ir.logical_shift_left_32(first_and, Value::ImmU8(1), Value::ImmU1(false));
    let first_shift = ir.logical_shift_right_32(operand, Value::ImmU8(1), Value::ImmU1(false));
    let first_lsr = ir.and_32(first_shift, Value::ImmU32(0x5555_5555));
    let first = ir.or_32(first_lsl, first_lsr);

    let second_and = ir.and_32(first, Value::ImmU32(0x3333_3333));
    let second_lsl = ir.logical_shift_left_32(second_and, Value::ImmU8(2), Value::ImmU1(false));
    let second_shift = ir.logical_shift_right_32(first, Value::ImmU8(2), Value::ImmU1(false));
    let second_lsr = ir.and_32(second_shift, Value::ImmU32(0x3333_3333));
    let second = ir.or_32(second_lsl, second_lsr);

    let third_and = ir.and_32(second, Value::ImmU32(0x0F0F_0F0F));
    let third_lsl = ir.logical_shift_left_32(third_and, Value::ImmU8(4), Value::ImmU1(false));
    let third_shift = ir.logical_shift_right_32(second, Value::ImmU8(4), Value::ImmU1(false));
    let third_lsr = ir.and_32(third_shift, Value::ImmU32(0x0F0F_0F0F));
    let third = ir.or_32(third_lsl, third_lsr);

    let fourth_hi = ir.logical_shift_left_32(third, Value::ImmU8(24), Value::ImmU1(false));
    let fourth_mid_mask = ir.and_32(third, Value::ImmU32(0x0000_FF00));
    let fourth_mid_lsl =
        ir.logical_shift_left_32(fourth_mid_mask, Value::ImmU8(8), Value::ImmU1(false));
    let fourth_lsl = ir.or_32(fourth_hi, fourth_mid_lsl);

    let fourth_shift = ir.logical_shift_right_32(third, Value::ImmU8(8), Value::ImmU1(false));
    let fourth_mid_lsr = ir.and_32(fourth_shift, Value::ImmU32(0x0000_FF00));
    let fourth_lo = ir.logical_shift_right_32(third, Value::ImmU8(24), Value::ImmU1(false));
    let fourth_lsr = ir.or_32(fourth_mid_lsr, fourth_lo);
    ir.or_32(fourth_lsl, fourth_lsr)
}

fn rev16_32(ir: &mut IREmitter<'_>, operand: Value) -> Value {
    let hihalf_shift = ir.logical_shift_right_32(operand, Value::ImmU8(8), Value::ImmU1(false));
    let hihalf = ir.and_32(hihalf_shift, Value::ImmU32(0x00FF_00FF));
    let lohalf_shift = ir.logical_shift_left_32(operand, Value::ImmU8(8), Value::ImmU1(false));
    let lohalf = ir.and_32(lohalf_shift, Value::ImmU32(0xFF00_FF00));
    ir.or_32(hihalf, lohalf)
}

fn rev16_64(ir: &mut IREmitter<'_>, operand: Value) -> Value {
    let hihalf_shift = ir.logical_shift_right_64(operand, Value::ImmU8(8));
    let hihalf = ir.and_64(hihalf_shift, Value::ImmU64(0x00FF_00FF_00FF_00FF));
    let lohalf_shift = ir.logical_shift_left_64(operand, Value::ImmU8(8));
    let lohalf = ir.and_64(lohalf_shift, Value::ImmU64(0xFF00_FF00_FF00_FF00));
    ir.or_64(hihalf, lohalf)
}

impl<'a> TranslatorVisitor<'a> {
    /// RBIT - Reverse Bits
    pub fn rbit(&mut self, inst: &DecodedInst) -> bool {
        let sf = inst.sf();
        let rn = Reg::from_u32(inst.rn());
        let rd = Reg::from_u32(inst.rd());

        if sf {
            let operand = self.x(64, rn);
            let ir = self.ir.ir();
            // RBIT reverses ALL 64 bits, so the input's high half becomes the
            // output's low half and vice versa. `pack_2x32_to_1x64(lo, hi)`
            // takes the new-low first, so we pass rbit32(input.high) as the
            // low arg and rbit32(input.low) as the high arg.
            let lsw_operand = ir.least_significant_word(operand);
            let lsw_reversed = rbit32(ir, lsw_operand);
            let msw_operand = ir.most_significant_word(operand).result;
            let msw_reversed = rbit32(ir, msw_operand);
            let result = ir.pack_2x32_to_1x64(msw_reversed, lsw_reversed);
            self.set_x(64, rd, result);
        } else {
            let operand = self.x(32, rn);
            let result = rbit32(self.ir.ir(), operand);
            self.set_x(32, rd, result);
        }
        true
    }

    /// REV16 - Reverse bytes in 16-bit halfwords
    pub fn rev16(&mut self, inst: &DecodedInst) -> bool {
        let sf = inst.sf();
        let rn = Reg::from_u32(inst.rn());
        let rd = Reg::from_u32(inst.rd());
        let datasize = if sf { 64 } else { 32 };

        if sf {
            let operand = self.x(datasize, rn);
            let result = rev16_64(self.ir.ir(), operand);
            self.set_x(datasize, rd, result);
        } else {
            let operand = self.x(datasize, rn);
            let result = rev16_32(self.ir.ir(), operand);
            self.set_x(datasize, rd, result);
        }
        true
    }

    /// REV - Reverse Bytes (32 or 64 bit)
    pub fn rev(&mut self, inst: &DecodedInst) -> bool {
        let sf = inst.sf();
        let rn = Reg::from_u32(inst.rn());
        let rd = Reg::from_u32(inst.rd());

        if sf {
            let operand = self.x(64, rn);
            let result = self.ir.ir().byte_reverse_dual(operand);
            self.set_x(64, rd, result);
        } else {
            let operand = self.x(32, rn);
            let result = self.ir.ir().byte_reverse_word(operand);
            self.set_x(32, rd, result);
        }
        true
    }

    /// REV32 - Reverse bytes in 32-bit words (64-bit)
    pub fn rev32(&mut self, inst: &DecodedInst) -> bool {
        let rn = Reg::from_u32(inst.rn());
        let rd = Reg::from_u32(inst.rd());

        let operand = self.x(64, rn);
        let ir = self.ir.ir();
        let lo_operand = ir.least_significant_word(operand);
        let lo = ir.byte_reverse_word(lo_operand);
        let hi_operand = ir.most_significant_word(operand).result;
        let hi = ir.byte_reverse_word(hi_operand);
        let result = ir.pack_2x32_to_1x64(lo, hi);
        self.set_x(64, rd, result);
        true
    }

    /// CLZ - Count Leading Zeros
    pub fn clz(&mut self, inst: &DecodedInst) -> bool {
        let sf = inst.sf();
        let rn = Reg::from_u32(inst.rn());
        let rd = Reg::from_u32(inst.rd());
        let datasize = if sf { 64 } else { 32 };

        let operand = self.x(datasize, rn);
        let result = match datasize {
            32 => self.ir.ir().count_leading_zeros_32(operand),
            _ => self.ir.ir().count_leading_zeros_64(operand),
        };

        self.set_x(datasize, rd, result);
        true
    }

    /// CLS - Count Leading Sign bits
    pub fn cls(&mut self, inst: &DecodedInst) -> bool {
        let sf = inst.sf();
        let rn = Reg::from_u32(inst.rn());
        let rd = Reg::from_u32(inst.rd());
        let datasize = if sf { 64 } else { 32 };

        match datasize {
            32 => {
                let operand = self.x(datasize, rn);
                let asr = self.ir.ir().arithmetic_shift_right_32(
                    operand,
                    Value::ImmU8(datasize as u8),
                    Value::ImmU1(false),
                );
                let eor = self.ir.ir().eor_32(operand, asr);
                let clz = self.ir.ir().count_leading_zeros_32(eor);
                let result = self
                    .ir
                    .ir()
                    .sub_32(clz, Value::ImmU32(1), Value::ImmU1(true));
                self.set_x(datasize, rd, result);
            }
            _ => {
                let operand = self.x(datasize, rn);
                let asr = self
                    .ir
                    .ir()
                    .arithmetic_shift_right_64(operand, Value::ImmU8(datasize as u8));
                let eor = self.ir.ir().eor_64(operand, asr);
                let clz = self.ir.ir().count_leading_zeros_64(eor);
                let result = self
                    .ir
                    .ir()
                    .sub_64(clz, Value::ImmU64(1), Value::ImmU1(true));
                self.set_x(datasize, rd, result);
            }
        }
        true
    }

    pub fn udiv(&mut self, inst: &DecodedInst) -> bool {
        let sf = inst.sf();
        let rm = Reg::from_u32(inst.rm());
        let rn = Reg::from_u32(inst.rn());
        let rd = Reg::from_u32(inst.rd());
        let datasize = if sf { 64 } else { 32 };

        let m = self.x(datasize, rm);
        let n = self.x(datasize, rn);
        let result = match datasize {
            32 => self.ir.ir().unsigned_div_32(n, m),
            _ => self.ir.ir().unsigned_div_64(n, m),
        };

        self.set_x(datasize, rd, result);
        true
    }

    pub fn sdiv(&mut self, inst: &DecodedInst) -> bool {
        let sf = inst.sf();
        let rm = Reg::from_u32(inst.rm());
        let rn = Reg::from_u32(inst.rn());
        let rd = Reg::from_u32(inst.rd());
        let datasize = if sf { 64 } else { 32 };

        let m = self.x(datasize, rm);
        let n = self.x(datasize, rn);
        let result = match datasize {
            32 => self.ir.ir().signed_div_32(n, m),
            _ => self.ir.ir().signed_div_64(n, m),
        };

        self.set_x(datasize, rd, result);
        true
    }
}

#[cfg(test)]
mod tests {
    use crate::frontend::a64::translate::{translate, TranslationOptions};
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;

    fn translate_single(raw: u32) -> crate::ir::block::Block {
        let loc = A64LocationDescriptor::new(0x1000, 0, true);
        translate(
            loc,
            &move |pc: u64| {
                if pc == 0x1000 {
                    Some(raw)
                } else {
                    None
                }
            },
            TranslationOptions::default(),
        )
    }

    #[test]
    fn rev16_int_translates_without_interpret_terminal() {
        let block = translate_single(0x5AC00441);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::Or32));
    }

    #[test]
    fn rev32_int_translates_without_interpret_terminal() {
        let block = translate_single(0xDAC00841);
        assert!(
            block
                .instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::ByteReverseWord)
                .count()
                >= 2
        );
    }

    #[test]
    fn cls_int_translates_without_interpret_terminal() {
        let block = translate_single(0x5AC01441);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::CountLeadingZeros32));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::Eor32));
    }

    #[test]
    fn udiv_translates_without_interpret_terminal() {
        let block = translate_single(0x9AC10841);
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, Opcode::UnsignedDiv32 | Opcode::UnsignedDiv64)));
    }

    #[test]
    fn sdiv_translates_without_interpret_terminal() {
        let block = translate_single(0x9AC10C41);
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, Opcode::SignedDiv32 | Opcode::SignedDiv64)));
    }
}
