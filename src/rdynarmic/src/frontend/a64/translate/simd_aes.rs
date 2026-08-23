use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;

impl<'a> TranslatorVisitor<'a> {
    pub fn aesd(&mut self, inst: &DecodedInst) -> bool {
        let vd = Vec::from_u32(inst.rd());
        let vn = Vec::from_u32(inst.rn());
        let operand1 = self.ir.get_q(vd);
        let operand2 = self.ir.get_q(vn);
        let xored = self.ir.ir().vector_eor(operand1, operand2);
        let result = self.ir.ir().aes_decrypt_single_round(xored);
        self.ir.set_q(vd, result);
        true
    }

    pub fn aese(&mut self, inst: &DecodedInst) -> bool {
        let vd = Vec::from_u32(inst.rd());
        let vn = Vec::from_u32(inst.rn());
        let operand1 = self.ir.get_q(vd);
        let operand2 = self.ir.get_q(vn);
        let xored = self.ir.ir().vector_eor(operand1, operand2);
        let result = self.ir.ir().aes_encrypt_single_round(xored);
        self.ir.set_q(vd, result);
        true
    }

    pub fn aesimc(&mut self, inst: &DecodedInst) -> bool {
        let vd = Vec::from_u32(inst.rd());
        let vn = Vec::from_u32(inst.rn());
        let operand = self.ir.get_q(vn);
        let result = self.ir.ir().aes_inverse_mix_columns(operand);
        self.ir.set_q(vd, result);
        true
    }

    pub fn aesmc(&mut self, inst: &DecodedInst) -> bool {
        let vd = Vec::from_u32(inst.rd());
        let vn = Vec::from_u32(inst.rn());
        let operand = self.ir.get_q(vn);
        let result = self.ir.ir().aes_mix_columns(operand);
        self.ir.set_q(vd, result);
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

    fn translate_one(raw: u32) -> (Block, bool) {
        let decoded = decode(raw).expect("instruction should decode");
        let location = A64LocationDescriptor::new(0x1000, 0, false);
        let mut block = Block::new(location.to_location());
        let mut visitor = TranslatorVisitor::new(
            &mut block,
            location,
            crate::frontend::a64::translate::TranslationOptions::default(),
        );
        let should_continue = visitor.dispatch(&decoded);
        drop(visitor);
        (block, should_continue)
    }

    #[test]
    fn aes_family_emits_upstream_ir() {
        let cases = [
            (0x4e28_4800, Opcode::AESEncryptSingleRound),
            (0x4e28_5800, Opcode::AESDecryptSingleRound),
            (0x4e28_6800, Opcode::AESMixColumns),
            (0x4e28_7800, Opcode::AESInverseMixColumns),
        ];

        for (encoding, expected_opcode) in cases {
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "encoding 0x{encoding:08x}");
            assert!(block
                .instructions
                .iter()
                .any(|inst| inst.opcode == expected_opcode));
        }
    }
}
