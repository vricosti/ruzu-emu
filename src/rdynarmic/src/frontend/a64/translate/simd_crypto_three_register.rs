use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;
use crate::ir::a64_emitter::A64IREmitter;
use crate::ir::value::Value;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sm3TtVariant {
    A,
    B,
}

fn sm3tt1(ir: &mut A64IREmitter<'_>, vm: Vec, index: u8, vn: Vec, vd: Vec, behavior: Sm3TtVariant) {
    let d = ir.get_q(vd);
    let m = ir.get_q(vm);
    let n = ir.get_q(vn);

    let top_d = ir.ir().vector_get_element(32, d, 3);
    let before_top_d = ir.ir().vector_get_element(32, d, 2);
    let after_low_d = ir.ir().vector_get_element(32, d, 1);
    let low_d = ir.ir().vector_get_element(32, d, 0);
    let top_n = ir.ir().vector_get_element(32, n, 3);

    let wj_prime = ir.ir().vector_get_element(32, m, index);
    let carry = Value::ImmU1(false);
    let rotated_top_d = ir.ir().rotate_right_32(top_d, Value::ImmU8(20), carry);
    let ss2 = ir.ir().eor_32(top_n, rotated_top_d);
    let tt1 = match behavior {
        Sm3TtVariant::A => {
            let top_eor_before = ir.ir().eor_32(top_d, before_top_d);
            ir.ir().eor_32(after_low_d, top_eor_before)
        }
        Sm3TtVariant::B => {
            let tmp1 = ir.ir().and_32(top_d, after_low_d);
            let tmp2 = ir.ir().and_32(top_d, before_top_d);
            let tmp3 = ir.ir().and_32(after_low_d, before_top_d);
            let tmp1_or_tmp2 = ir.ir().or_32(tmp1, tmp2);
            ir.ir().or_32(tmp1_or_tmp2, tmp3)
        }
    };
    let ss2_plus_wj = ir.ir().add_32(ss2, wj_prime, carry);
    let low_plus_rest = ir.ir().add_32(low_d, ss2_plus_wj, carry);
    let final_tt1 = ir.ir().add_32(tt1, low_plus_rest, carry);

    let zero_vector = ir.ir().zero_vector();
    let tmp1 = ir.ir().vector_set_element(32, zero_vector, 0, after_low_d);
    let rotated_before = ir
        .ir()
        .rotate_right_32(before_top_d, Value::ImmU8(23), carry);
    let tmp2 = ir.ir().vector_set_element(32, tmp1, 1, rotated_before);
    let tmp3 = ir.ir().vector_set_element(32, tmp2, 2, top_d);
    let result = ir.ir().vector_set_element(32, tmp3, 3, final_tt1);

    ir.set_q(vd, result);
}

fn sm3tt2(ir: &mut A64IREmitter<'_>, vm: Vec, index: u8, vn: Vec, vd: Vec, behavior: Sm3TtVariant) {
    let d = ir.get_q(vd);
    let m = ir.get_q(vm);
    let n = ir.get_q(vn);

    let top_d = ir.ir().vector_get_element(32, d, 3);
    let before_top_d = ir.ir().vector_get_element(32, d, 2);
    let after_low_d = ir.ir().vector_get_element(32, d, 1);
    let low_d = ir.ir().vector_get_element(32, d, 0);
    let top_n = ir.ir().vector_get_element(32, n, 3);

    let wj = ir.ir().vector_get_element(32, m, index);
    let tt2 = match behavior {
        Sm3TtVariant::A => {
            let top_eor_before = ir.ir().eor_32(top_d, before_top_d);
            ir.ir().eor_32(after_low_d, top_eor_before)
        }
        Sm3TtVariant::B => {
            let tmp1 = ir.ir().and_32(top_d, before_top_d);
            let tmp2 = ir.ir().and_not_32(after_low_d, top_d);
            ir.ir().or_32(tmp1, tmp2)
        }
    };
    let carry = Value::ImmU1(false);
    let top_n_plus_wj = ir.ir().add_32(top_n, wj, carry);
    let low_plus_rest = ir.ir().add_32(low_d, top_n_plus_wj, carry);
    let final_tt2 = ir.ir().add_32(tt2, low_plus_rest, carry);
    let rotate_23 = ir.ir().rotate_right_32(final_tt2, Value::ImmU8(23), carry);
    let rotate_15 = ir.ir().rotate_right_32(final_tt2, Value::ImmU8(15), carry);
    let rotations = ir.ir().eor_32(rotate_23, rotate_15);
    let top_result = ir.ir().eor_32(final_tt2, rotations);

    let zero_vector = ir.ir().zero_vector();
    let tmp1 = ir.ir().vector_set_element(32, zero_vector, 0, after_low_d);
    let rotated_before = ir
        .ir()
        .rotate_right_32(before_top_d, Value::ImmU8(13), carry);
    let tmp2 = ir.ir().vector_set_element(32, tmp1, 1, rotated_before);
    let tmp3 = ir.ir().vector_set_element(32, tmp2, 2, top_d);
    let result = ir.ir().vector_set_element(32, tmp3, 3, top_result);

    ir.set_q(vd, result);
}

impl<'a> TranslatorVisitor<'a> {
    pub fn sm3tt1a(&mut self, inst: &DecodedInst) -> bool {
        sm3tt1(
            &mut self.ir,
            Vec::from_u32(inst.rm()),
            inst.bits(13, 12) as u8,
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            Sm3TtVariant::A,
        );
        true
    }

    pub fn sm3tt1b(&mut self, inst: &DecodedInst) -> bool {
        sm3tt1(
            &mut self.ir,
            Vec::from_u32(inst.rm()),
            inst.bits(13, 12) as u8,
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            Sm3TtVariant::B,
        );
        true
    }

    pub fn sm3tt2a(&mut self, inst: &DecodedInst) -> bool {
        sm3tt2(
            &mut self.ir,
            Vec::from_u32(inst.rm()),
            inst.bits(13, 12) as u8,
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            Sm3TtVariant::A,
        );
        true
    }

    pub fn sm3tt2b(&mut self, inst: &DecodedInst) -> bool {
        sm3tt2(
            &mut self.ir,
            Vec::from_u32(inst.rm()),
            inst.bits(13, 12) as u8,
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            Sm3TtVariant::B,
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a64::decoder::{decode, A64InstructionName};
    use crate::frontend::a64::translate::TranslationOptions;
    use crate::ir::block::Block;
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;

    #[test]
    fn sm3tt_variants_decode_and_emit_upstream_result_shape() {
        let cases = [
            (0xce41_b044, A64InstructionName::SM3TT1A),
            (0xce41_b444, A64InstructionName::SM3TT1B),
            (0xce41_b844, A64InstructionName::SM3TT2A),
            (0xce41_bc44, A64InstructionName::SM3TT2B),
        ];

        for (encoding, expected_name) in cases {
            let decoded = decode(encoding).expect("SM3TT instruction must decode");
            assert_eq!(decoded.name, expected_name);
            let location = A64LocationDescriptor::new(0x1000, 0, false);
            let mut block = Block::new(location.to_location());
            let mut visitor =
                TranslatorVisitor::new(&mut block, location, TranslationOptions::default());
            assert!(visitor.dispatch(&decoded));
            drop(visitor);
            for (instruction, register) in block.instructions.iter().take(3).zip([4, 1, 2]) {
                assert_eq!(
                    instruction.args[0],
                    Value::ImmA64Vec(Vec::from_u32(register))
                );
            }
            let indexed_word = block
                .instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::VectorGetElement32)
                .nth(5)
                .expect("SM3TT indexed message word");
            assert_eq!(indexed_word.args[1], Value::ImmU8(3));
            assert_eq!(
                block.instructions.last().map(|inst| inst.opcode),
                Some(Opcode::A64SetQ)
            );
            assert_eq!(
                block
                    .instructions
                    .iter()
                    .filter(|inst| inst.opcode == Opcode::VectorSetElement32)
                    .count(),
                4
            );
        }
    }
}
