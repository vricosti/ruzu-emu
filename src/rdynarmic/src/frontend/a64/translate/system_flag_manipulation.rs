//! Port of upstream `dynarmic/frontend/A64/translate/impl/system_flag_manipulation.cpp`.

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Reg;

impl<'a> TranslatorVisitor<'a> {
    pub fn cfinv(&mut self, _inst: &DecodedInst) -> bool {
        let nzcv = self.ir.get_nzcv_raw();
        let mask = self.ir.ir().imm32(0x2000_0000);
        let result = self.ir.ir().eor_32(nzcv, mask);

        self.ir.set_nzcv_raw(result);
        true
    }

    pub fn rmif(&mut self, inst: &DecodedInst) -> bool {
        let lsb = inst.bits(20, 15);
        let rn = Reg::from_u32(inst.rn());
        let mask_value = inst.bits(3, 0);

        if mask_value == 0 {
            let nzcv = self.ir.get_nzcv_raw();
            self.ir.set_nzcv_raw(nzcv);
            return true;
        }

        let tmp_reg = self.ir.get_x(rn);
        let shift = self.ir.ir().imm8(lsb as u8);
        let rotated = self.ir.ir().rotate_right_64(tmp_reg, shift);
        let shift = self.ir.ir().imm8(28);
        let shifted = self.ir.ir().logical_shift_left_64(rotated, shift);
        let shifted = self.ir.ir().least_significant_word(shifted);

        if mask_value == 0b1111 {
            self.ir.set_nzcv_raw(shifted);
            return true;
        }

        let mut preservation_mask = 0u32;
        if mask_value & 0b1000 == 0 {
            preservation_mask |= 1 << 31;
        }
        if mask_value & 0b0100 == 0 {
            preservation_mask |= 1 << 30;
        }
        if mask_value & 0b0010 == 0 {
            preservation_mask |= 1 << 29;
        }
        if mask_value & 0b0001 == 0 {
            preservation_mask |= 1 << 28;
        }

        let replacement_mask = self.ir.ir().imm32(!preservation_mask);
        let masked = self.ir.ir().and_32(shifted, replacement_mask);
        let nzcv = self.ir.get_nzcv_raw();
        let preservation_mask = self.ir.ir().imm32(preservation_mask);
        let nzcv = self.ir.ir().and_32(nzcv, preservation_mask);
        let result = self.ir.ir().or_32(nzcv, masked);

        self.ir.set_nzcv_raw(result);
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
    use crate::ir::value::Value;

    fn translate_one(raw: u32) -> (Block, bool) {
        let decoded = decode(raw).expect("instruction should decode");
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let mut visitor = TranslatorVisitor::new(
            &mut block,
            A64LocationDescriptor::new(0x1000, 0, false),
            crate::frontend::a64::translate::TranslationOptions::default(),
        );
        let should_continue = visitor.dispatch(&decoded);
        drop(visitor);
        (block, should_continue)
    }

    #[test]
    fn cfinv_toggles_only_the_raw_carry_bit() {
        let (block, should_continue) = translate_one(0xD500_401F);

        assert!(should_continue);
        let eor = block
            .instructions
            .iter()
            .find(|instruction| instruction.opcode == Opcode::Eor32)
            .expect("CFINV must emit Eor32");
        assert_eq!(eor.arg(1), Value::ImmU32(0x2000_0000));
        assert!(block
            .instructions
            .iter()
            .any(|instruction| instruction.opcode == Opcode::A64SetNZCVRaw));
    }

    #[test]
    fn rmif_preserves_upstream_zero_full_and_partial_mask_paths() {
        let cases = [
            (0xBA02_8460, 1, 0, 0),
            (0xBA02_846F, 0, 0, 0),
            (0xBA02_8465, 1, 2, 1),
        ];

        for (encoding, get_nzcv_count, and_count, or_count) in cases {
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert_eq!(
                block
                    .instructions
                    .iter()
                    .filter(|instruction| instruction.opcode == Opcode::A64GetNZCVRaw)
                    .count(),
                get_nzcv_count
            );
            assert_eq!(
                block
                    .instructions
                    .iter()
                    .filter(|instruction| instruction.opcode == Opcode::And32)
                    .count(),
                and_count
            );
            assert_eq!(
                block
                    .instructions
                    .iter()
                    .filter(|instruction| instruction.opcode == Opcode::Or32)
                    .count(),
                or_count
            );
        }
    }
}
