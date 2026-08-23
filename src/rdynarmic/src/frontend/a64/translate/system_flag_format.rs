//! Port of upstream `dynarmic/frontend/A64/translate/impl/system_flag_format.cpp`.

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;

impl<'a> TranslatorVisitor<'a> {
    pub fn axflag(&mut self, _inst: &DecodedInst) -> bool {
        let nzcv = self.ir.get_nzcv_raw();

        let z_mask = self.ir.ir().imm32(0x4000_0000);
        let z = self.ir.ir().and_32(nzcv, z_mask);
        let c_mask = self.ir.ir().imm32(0x2000_0000);
        let c = self.ir.ir().and_32(nzcv, c_mask);
        let v_mask = self.ir.ir().imm32(0x1000_0000);
        let v = self.ir.ir().and_32(nzcv, v_mask);

        let shift = self.ir.ir().imm8(2);
        let carry = self.ir.ir().imm1(false);
        let shifted_v = self.ir.ir().logical_shift_left_32(v, shift, carry);
        let new_z = self.ir.ir().or_32(shifted_v, z);
        let shift = self.ir.ir().imm8(1);
        let carry = self.ir.ir().imm1(false);
        let shifted_v = self.ir.ir().logical_shift_left_32(v, shift, carry);
        let new_c = self.ir.ir().and_not_32(c, shifted_v);
        let mask = self.ir.ir().imm32(0x2000_0000);
        let new_c = self.ir.ir().and_32(new_c, mask);

        let result = self.ir.ir().or_32(new_z, new_c);
        self.ir.set_nzcv_raw(result);
        true
    }

    pub fn xaflag(&mut self, _inst: &DecodedInst) -> bool {
        let nzcv = self.ir.get_nzcv_raw();

        let z_mask = self.ir.ir().imm32(0x4000_0000);
        let z = self.ir.ir().and_32(nzcv, z_mask);
        let c_mask = self.ir.ir().imm32(0x2000_0000);
        let c = self.ir.ir().and_32(nzcv, c_mask);

        let z_mask = self.ir.ir().imm32(0x4000_0000);
        let not_z = self.ir.ir().and_not_32(z_mask, z);
        let c_mask = self.ir.ir().imm32(0x2000_0000);
        let not_c = self.ir.ir().and_not_32(c_mask, c);

        let shift = self.ir.ir().imm8(2);
        let carry = self.ir.ir().imm1(false);
        let shifted_not_c = self.ir.ir().logical_shift_left_32(not_c, shift, carry);
        let shift = self.ir.ir().imm8(1);
        let carry = self.ir.ir().imm1(false);
        let shifted_not_z = self.ir.ir().logical_shift_left_32(not_z, shift, carry);
        let new_n = self.ir.ir().and_32(shifted_not_c, shifted_not_z);

        let shift = self.ir.ir().imm8(1);
        let carry = self.ir.ir().imm1(false);
        let shifted_c = self.ir.ir().logical_shift_left_32(c, shift, carry);
        let new_z = self.ir.ir().and_32(z, shifted_c);

        let shift = self.ir.ir().imm8(1);
        let carry = self.ir.ir().imm1(false);
        let shifted_z = self.ir.ir().logical_shift_right_32(z, shift, carry);
        let new_c = self.ir.ir().or_32(c, shifted_z);

        let shift = self.ir.ir().imm8(1);
        let carry = self.ir.ir().imm1(false);
        let shifted_not_c = self.ir.ir().logical_shift_right_32(not_c, shift, carry);
        let shift = self.ir.ir().imm8(2);
        let carry = self.ir.ir().imm1(false);
        let shifted_z = self.ir.ir().logical_shift_right_32(z, shift, carry);
        let new_v = self.ir.ir().and_32(shifted_not_c, shifted_z);

        let result = self.ir.ir().or_32(new_n, new_z);
        let result = self.ir.ir().or_32(result, new_c);
        let result = self.ir.ir().or_32(result, new_v);

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
    use crate::ir::terminal::Terminal;

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
    fn flag_format_visitors_emit_upstream_ir_shapes() {
        let cases = [(0xD500_405F, 4, 1), (0xD500_403F, 5, 2)];

        for (encoding, and_count, and_not_count) in cases {
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert_eq!(
                block
                    .instructions
                    .iter()
                    .filter(|instruction| instruction.opcode == Opcode::A64GetNZCVRaw)
                    .count(),
                1
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
                    .filter(|instruction| instruction.opcode == Opcode::AndNot32)
                    .count(),
                and_not_count
            );
            assert_eq!(
                block
                    .instructions
                    .iter()
                    .filter(|instruction| instruction.opcode == Opcode::A64SetNZCVRaw)
                    .count(),
                1
            );
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }
}
