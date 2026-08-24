use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;
use crate::ir::value::Value;

impl<'a> TranslatorVisitor<'a> {
    pub fn eor3(&mut self, inst: &DecodedInst) -> bool {
        let vd = Vec::from_u32(inst.rd());
        let a = self.ir.get_q(Vec::from_u32(inst.ra()));
        let m = self.ir.get_q(Vec::from_u32(inst.rm()));
        let n = self.ir.get_q(Vec::from_u32(inst.rn()));

        let n_eor_m = self.ir.ir().vector_eor(n, m);
        let result = self.ir.ir().vector_eor(n_eor_m, a);

        self.ir.set_q(vd, result);
        true
    }

    pub fn bcax(&mut self, inst: &DecodedInst) -> bool {
        let vd = Vec::from_u32(inst.rd());
        let a = self.ir.get_q(Vec::from_u32(inst.ra()));
        let m = self.ir.get_q(Vec::from_u32(inst.rm()));
        let n = self.ir.get_q(Vec::from_u32(inst.rn()));

        let m_and_not_a = self.ir.ir().vector_and_not(m, a);
        let result = self.ir.ir().vector_eor(n, m_and_not_a);

        self.ir.set_q(vd, result);
        true
    }

    pub fn sm3ss1(&mut self, inst: &DecodedInst) -> bool {
        let vd = Vec::from_u32(inst.rd());
        let a = self.ir.get_q(Vec::from_u32(inst.ra()));
        let m = self.ir.get_q(Vec::from_u32(inst.rm()));
        let n = self.ir.get_q(Vec::from_u32(inst.rn()));

        let top_a = self.ir.ir().vector_get_element(32, a, 3);
        let top_m = self.ir.ir().vector_get_element(32, m, 3);
        let top_n = self.ir.ir().vector_get_element(32, n, 3);

        let carry = Value::ImmU1(false);
        let rotated_n = self.ir.ir().rotate_right_32(top_n, Value::ImmU8(20), carry);
        let rotated_n_plus_m = self.ir.ir().add_32(rotated_n, top_m, carry);
        let sum = self.ir.ir().add_32(rotated_n_plus_m, top_a, carry);
        let result = self.ir.ir().rotate_right_32(sum, Value::ImmU8(25), carry);

        let zero_vector = self.ir.ir().zero_vector();
        let vector_result = self.ir.ir().vector_set_element(32, zero_vector, 3, result);

        self.ir.set_q(vd, vector_result);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a64::decoder::{A64InstructionName, decode};
    use crate::frontend::a64::translate::TranslationOptions;
    use crate::ir::block::Block;
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;

    fn translate_one(encoding: u32) -> (A64InstructionName, Block) {
        let decoded = decode(encoding).expect("crypto instruction must decode");
        let location = A64LocationDescriptor::new(0x1000, 0, false);
        let mut block = Block::new(location.to_location());
        let mut visitor =
            TranslatorVisitor::new(&mut block, location, TranslationOptions::default());
        assert!(visitor.dispatch(&decoded));
        drop(visitor);
        (decoded.name, block)
    }

    #[test]
    fn eor3_and_bcax_match_upstream_ir_order() {
        let cases = [
            (
                0xce01_0864,
                A64InstructionName::EOR3,
                vec![
                    Opcode::A64GetQ,
                    Opcode::A64GetQ,
                    Opcode::A64GetQ,
                    Opcode::VectorEor,
                    Opcode::VectorEor,
                    Opcode::A64SetQ,
                ],
            ),
            (
                0xce21_0864,
                A64InstructionName::BCAX,
                vec![
                    Opcode::A64GetQ,
                    Opcode::A64GetQ,
                    Opcode::A64GetQ,
                    Opcode::VectorAndNot,
                    Opcode::VectorEor,
                    Opcode::A64SetQ,
                ],
            ),
        ];

        for (encoding, expected_name, expected_opcodes) in cases {
            let (name, block) = translate_one(encoding);
            assert_eq!(name, expected_name);
            let opcodes: std::vec::Vec<_> =
                block.instructions.iter().map(|inst| inst.opcode).collect();
            assert_eq!(opcodes, expected_opcodes);
            assert_eq!(
                block.instructions[0].args[0],
                Value::ImmA64Vec(Vec::from_u32(2))
            );
            assert_eq!(
                block.instructions[1].args[0],
                Value::ImmA64Vec(Vec::from_u32(1))
            );
            assert_eq!(
                block.instructions[2].args[0],
                Value::ImmA64Vec(Vec::from_u32(3))
            );
        }
    }

    #[test]
    fn sm3ss1_uses_upstream_top_lane_and_rotation_sequence() {
        let (name, block) = translate_one(0xce41_0864);
        assert_eq!(name, A64InstructionName::SM3SS1);
        for (instruction, register) in block.instructions.iter().take(3).zip([2, 1, 3]) {
            assert_eq!(
                instruction.args[0],
                Value::ImmA64Vec(Vec::from_u32(register))
            );
        }
        let opcodes: std::vec::Vec<_> = block.instructions.iter().map(|inst| inst.opcode).collect();
        assert_eq!(
            opcodes
                .iter()
                .filter(|opcode| **opcode == Opcode::VectorGetElement32)
                .count(),
            3
        );
        assert_eq!(
            opcodes
                .iter()
                .filter(|opcode| **opcode == Opcode::BitRotateRight32)
                .count(),
            2
        );
        assert_eq!(
            opcodes
                .iter()
                .filter(|opcode| **opcode == Opcode::Add32)
                .count(),
            2
        );
        assert_eq!(opcodes.last(), Some(&Opcode::A64SetQ));
    }
}
