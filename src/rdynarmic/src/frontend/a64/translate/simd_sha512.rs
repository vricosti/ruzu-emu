//! Port of upstream `dynarmic/frontend/A64/translate/impl/simd_sha512.cpp`.

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;
use crate::ir::emitter::IREmitter;
use crate::ir::value::Value;

fn add64(ir: &mut IREmitter<'_>, a: Value, b: Value) -> Value {
    let carry = ir.imm1(false);
    ir.add_64(a, b, carry)
}

fn rotate_right32(ir: &mut IREmitter<'_>, value: Value, amount: u8) -> Value {
    let shift = ir.imm8(amount);
    let carry = ir.imm1(false);
    ir.rotate_right_32(value, shift, carry)
}

fn make_sig(
    ir: &mut IREmitter<'_>,
    data: Value,
    first_rot_amount: u8,
    second_rot_amount: u8,
    shift_amount: u8,
) -> Value {
    let shift = ir.imm8(first_rot_amount);
    let tmp1 = ir.rotate_right_64(data, shift);
    let shift = ir.imm8(second_rot_amount);
    let tmp2 = ir.rotate_right_64(data, shift);
    let shift = ir.imm8(shift_amount);
    let tmp3 = ir.logical_shift_right_64(data, shift);

    let tmp2_eor_tmp3 = ir.eor_64(tmp2, tmp3);
    ir.eor_64(tmp1, tmp2_eor_tmp3)
}

fn make_mn_sig(
    ir: &mut IREmitter<'_>,
    data: Value,
    first_rot_amount: u8,
    second_rot_amount: u8,
    third_rot_amount: u8,
) -> Value {
    let shift = ir.imm8(first_rot_amount);
    let tmp1 = ir.rotate_right_64(data, shift);
    let shift = ir.imm8(second_rot_amount);
    let tmp2 = ir.rotate_right_64(data, shift);
    let shift = ir.imm8(third_rot_amount);
    let tmp3 = ir.rotate_right_64(data, shift);

    let tmp2_eor_tmp3 = ir.eor_64(tmp2, tmp3);
    ir.eor_64(tmp1, tmp2_eor_tmp3)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sha512HashPart {
    Part1,
    Part2,
}

fn make_sha512_sigma(ir: &mut IREmitter<'_>, data: Value, part: Sha512HashPart) -> Value {
    match part {
        Sha512HashPart::Part1 => make_mn_sig(ir, data, 14, 18, 41),
        Sha512HashPart::Part2 => make_mn_sig(ir, data, 28, 34, 39),
    }
}

fn make_sha512_partial_half(
    ir: &mut IREmitter<'_>,
    a: Value,
    b: Value,
    c: Value,
    upper_y: Value,
    lower_y: Value,
    part: Sha512HashPart,
) -> Value {
    let tmp1 = ir.and_64(a, b);

    if part == Sha512HashPart::Part1 {
        let tmp2 = ir.and_not_64(c, a);
        return ir.eor_64(tmp1, tmp2);
    }

    let tmp2 = ir.and_64(a, c);
    let tmp3 = ir.and_64(upper_y, lower_y);
    let tmp2_eor_tmp3 = ir.eor_64(tmp2, tmp3);
    ir.eor_64(tmp1, tmp2_eor_tmp3)
}

fn sha512_hash(
    ir: &mut crate::ir::a64_emitter::A64IREmitter<'_>,
    vm: Vec,
    vn: Vec,
    vd: Vec,
    part: Sha512HashPart,
) -> Value {
    let x = ir.get_q(vn);
    let y = ir.get_q(vm);
    let w = ir.get_q(vd);

    let lower_x = ir.ir().vector_get_element(64, x, 0);
    let upper_x = ir.ir().vector_get_element(64, x, 1);
    let lower_y = ir.ir().vector_get_element(64, y, 0);
    let upper_y = ir.ir().vector_get_element(64, y, 1);

    let partial = if part == Sha512HashPart::Part1 {
        make_sha512_partial_half(ir.ir(), upper_y, lower_x, upper_x, upper_y, lower_y, part)
    } else {
        make_sha512_partial_half(ir.ir(), lower_x, upper_y, lower_y, upper_y, lower_y, part)
    };
    let upper_w = ir.ir().vector_get_element(64, w, 1);
    let sig = if part == Sha512HashPart::Part1 {
        make_sha512_sigma(ir.ir(), upper_y, part)
    } else {
        make_sha512_sigma(ir.ir(), lower_y, part)
    };
    let sig_plus_w = add64(ir.ir(), sig, upper_w);
    let vtmp = add64(ir.ir(), partial, sig_plus_w);

    let tmp = if part == Sha512HashPart::Part1 {
        add64(ir.ir(), vtmp, lower_y)
    } else {
        vtmp
    };
    let partial = if part == Sha512HashPart::Part1 {
        make_sha512_partial_half(ir.ir(), tmp, upper_y, lower_x, upper_y, lower_y, part)
    } else {
        make_sha512_partial_half(ir.ir(), vtmp, lower_y, upper_y, upper_y, lower_y, part)
    };
    let sig = make_sha512_sigma(ir.ir(), tmp, part);
    let lower_w = ir.ir().vector_get_element(64, w, 0);
    let sig_plus_w = add64(ir.ir(), sig, lower_w);
    let low_result = add64(ir.ir(), partial, sig_plus_w);
    let low_result = ir.ir().zero_extend_to_quad(low_result);

    ir.ir().vector_set_element(64, low_result, 1, vtmp)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sm4RotationType {
    Sm4e,
    Sm4ekey,
}

fn sm4_rotation(
    ir: &mut IREmitter<'_>,
    intval: Value,
    round_result_low_word: Value,
    rotation_type: Sm4RotationType,
) -> Value {
    if rotation_type == Sm4RotationType::Sm4e {
        let tmp1 = rotate_right32(ir, intval, 30);
        let tmp2 = rotate_right32(ir, intval, 22);
        let tmp3 = rotate_right32(ir, intval, 14);
        let tmp4 = rotate_right32(ir, intval, 8);
        let tmp3_eor_tmp4 = ir.eor_32(tmp3, tmp4);
        let tmp2_eor_rest = ir.eor_32(tmp2, tmp3_eor_tmp4);
        let tmp1_eor_rest = ir.eor_32(tmp1, tmp2_eor_rest);
        let tmp5 = ir.eor_32(intval, tmp1_eor_rest);

        return ir.eor_32(tmp5, round_result_low_word);
    }

    let tmp1 = rotate_right32(ir, intval, 19);
    let tmp2 = rotate_right32(ir, intval, 9);
    let tmp1_eor_tmp2 = ir.eor_32(tmp1, tmp2);
    let intval_eor_rest = ir.eor_32(intval, tmp1_eor_tmp2);
    ir.eor_32(round_result_low_word, intval_eor_rest)
}

fn sm4_hash(
    ir: &mut crate::ir::a64_emitter::A64IREmitter<'_>,
    vn: Vec,
    vd: Vec,
    rotation_type: Sm4RotationType,
) -> Value {
    let n = ir.get_q(vn);
    let mut round_result = ir.get_q(vd);

    for i in 0..4 {
        let round_key = ir.ir().vector_get_element(32, n, i);
        let upper_round = ir.ir().vector_get_element(32, round_result, 3);
        let before_upper_round = ir.ir().vector_get_element(32, round_result, 2);
        let after_lower_round = ir.ir().vector_get_element(32, round_result, 1);

        let lower_eor_key = ir.ir().eor_32(after_lower_round, round_key);
        let before_eor_rest = ir.ir().eor_32(before_upper_round, lower_eor_key);
        let intval = ir.ir().eor_32(upper_round, before_eor_rest);
        let mut intval_vec = ir.ir().zero_extend_to_quad(intval);

        for j in 0..4 {
            let byte_element = ir.ir().vector_get_element(8, intval_vec, j);
            let substituted = ir.ir().sm4_access_substitution_box(byte_element);
            intval_vec = ir.ir().vector_set_element(8, intval_vec, j, substituted);
        }

        let intval_low_word = ir.ir().vector_get_element(32, intval_vec, 0);
        let round_result_low_word = ir.ir().vector_get_element(32, round_result, 0);
        let intval = sm4_rotation(
            ir.ir(),
            intval_low_word,
            round_result_low_word,
            rotation_type,
        );
        round_result = ir.ir().vector_rotate_whole_vector_right(round_result, 32);
        round_result = ir.ir().vector_set_element(32, round_result, 3, intval);
    }

    round_result
}

impl<'a> TranslatorVisitor<'a> {
    pub fn sha512su0(&mut self, inst: &DecodedInst) -> bool {
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let x = self.ir.get_q(vn);
        let w = self.ir.get_q(vd);

        let lower_x = self.ir.ir().vector_get_element(64, x, 0);
        let lower_w = self.ir.ir().vector_get_element(64, w, 0);
        let upper_w = self.ir.ir().vector_get_element(64, w, 1);

        let sig0_upper_w = make_sig(self.ir.ir(), upper_w, 1, 8, 7);
        let low_result = add64(self.ir.ir(), lower_w, sig0_upper_w);
        let low_result = self.ir.ir().zero_extend_to_quad(low_result);
        let sig0_lower_x = make_sig(self.ir.ir(), lower_x, 1, 8, 7);
        let high_result = add64(self.ir.ir(), upper_w, sig0_lower_x);
        let result = self
            .ir
            .ir()
            .vector_set_element(64, low_result, 1, high_result);

        self.ir.set_q(vd, result);
        true
    }

    pub fn sha512su1(&mut self, inst: &DecodedInst) -> bool {
        let vm = Vec::from_u32(inst.rm());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let x = self.ir.get_q(vn);
        let y = self.ir.get_q(vm);
        let w = self.ir.get_q(vd);

        let lower_x = self.ir.ir().vector_get_element(64, x, 0);
        let upper_x = self.ir.ir().vector_get_element(64, x, 1);
        let lower_sig = make_sig(self.ir.ir(), lower_x, 19, 61, 6);
        let low_result = self.ir.ir().zero_extend_to_quad(lower_sig);
        let upper_sig = make_sig(self.ir.ir(), upper_x, 19, 61, 6);
        let sig_vector = self
            .ir
            .ir()
            .vector_set_element(64, low_result, 1, upper_sig);
        let y_plus_sig = self.ir.ir().vector_add(64, y, sig_vector);
        let result = self.ir.ir().vector_add(64, w, y_plus_sig);

        self.ir.set_q(vd, result);
        true
    }

    pub fn sha512h(&mut self, inst: &DecodedInst) -> bool {
        let vm = Vec::from_u32(inst.rm());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let result = sha512_hash(&mut self.ir, vm, vn, vd, Sha512HashPart::Part1);
        self.ir.set_q(vd, result);
        true
    }

    pub fn sha512h2(&mut self, inst: &DecodedInst) -> bool {
        let vm = Vec::from_u32(inst.rm());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let result = sha512_hash(&mut self.ir, vm, vn, vd, Sha512HashPart::Part2);
        self.ir.set_q(vd, result);
        true
    }

    pub fn rax1(&mut self, inst: &DecodedInst) -> bool {
        let vm = Vec::from_u32(inst.rm());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let m = self.ir.get_q(vm);
        let n = self.ir.get_q(vn);

        let rotated_m = self.ir.ir().vector_rotate_left(64, m, 1);
        let result = self.ir.ir().vector_eor(n, rotated_m);

        self.ir.set_q(vd, result);
        true
    }

    pub fn xar(&mut self, inst: &DecodedInst) -> bool {
        let vm = Vec::from_u32(inst.rm());
        let imm6 = inst.bits(15, 10) as u8;
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let m = self.ir.get_q(vm);
        let n = self.ir.get_q(vn);

        let tmp = self.ir.ir().vector_eor(m, n);
        let result = self.ir.ir().vector_rotate_right(64, tmp, imm6);

        self.ir.set_q(vd, result);
        true
    }

    pub fn sm3partw1(&mut self, inst: &DecodedInst) -> bool {
        let vm = Vec::from_u32(inst.rm());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let d = self.ir.get_q(vd);
        let m = self.ir.get_q(vm);
        let n = self.ir.get_q(vn);

        let eor_d_n = self.ir.ir().vector_eor(d, n);
        let shuffled_m = self.ir.ir().vector_rotate_whole_vector_right(m, 32);
        let rotated_m = self.ir.ir().vector_rotate_left(32, shuffled_m, 15);
        let mut result = self.ir.ir().vector_eor(eor_d_n, rotated_m);

        for i in 0..4 {
            if i == 3 {
                let top_eor_d_n = self.ir.ir().vector_get_element(32, eor_d_n, 3);
                let low_result_word = self.ir.ir().vector_get_element(32, result, 0);
                let rotated = rotate_right32(self.ir.ir(), low_result_word, 17);
                let top_result_word = self.ir.ir().eor_32(top_eor_d_n, rotated);
                result = self
                    .ir
                    .ir()
                    .vector_set_element(32, result, 3, top_result_word);
            }

            let word = self.ir.ir().vector_get_element(32, result, i);
            let rotate17 = rotate_right32(self.ir.ir(), word, 17);
            let rotate9 = rotate_right32(self.ir.ir(), word, 9);
            let rotations = self.ir.ir().eor_32(rotate17, rotate9);
            let modified = self.ir.ir().eor_32(word, rotations);
            result = self.ir.ir().vector_set_element(32, result, i, modified);
        }

        self.ir.set_q(vd, result);
        true
    }

    pub fn sm3partw2(&mut self, inst: &DecodedInst) -> bool {
        let vm = Vec::from_u32(inst.rm());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let d = self.ir.get_q(vd);
        let m = self.ir.get_q(vm);
        let n = self.ir.get_q(vn);

        let rotated_m = self.ir.ir().vector_rotate_left(32, m, 7);
        let temp = self.ir.ir().vector_eor(n, rotated_m);
        let temp_result = self.ir.ir().vector_eor(d, temp);
        let temp_low = self.ir.ir().vector_get_element(32, temp, 0);
        let rotate1 = rotate_right32(self.ir.ir(), temp_low, 17);
        let rotate2 = rotate_right32(self.ir.ir(), rotate1, 17);
        let rotate3 = rotate_right32(self.ir.ir(), rotate1, 9);
        let rotate2_eor_rotate3 = self.ir.ir().eor_32(rotate2, rotate3);
        let temp2 = self.ir.ir().eor_32(rotate1, rotate2_eor_rotate3);

        let high_temp_result = self.ir.ir().vector_get_element(32, temp_result, 3);
        let replacement = self.ir.ir().eor_32(high_temp_result, temp2);
        let result = self
            .ir
            .ir()
            .vector_set_element(32, temp_result, 3, replacement);

        self.ir.set_q(vd, result);
        true
    }

    pub fn sm4e(&mut self, inst: &DecodedInst) -> bool {
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let result = sm4_hash(&mut self.ir, vn, vd, Sm4RotationType::Sm4e);
        self.ir.set_q(vd, result);
        true
    }

    pub fn sm4ekey(&mut self, inst: &DecodedInst) -> bool {
        let vm = Vec::from_u32(inst.rm());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        let result = sm4_hash(&mut self.ir, vm, vn, Sm4RotationType::Sm4ekey);
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
    use crate::ir::terminal::Terminal;

    fn translate_one(raw: u32) -> (Block, bool) {
        let decoded = decode(raw).expect("instruction should decode");
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let mut visitor = TranslatorVisitor::new(
            &mut block,
            A64LocationDescriptor::new(0x1000, 0, false),
            crate::frontend::a64::translate::visitor::TranslationOptions::default(),
        );
        let should_continue = visitor.dispatch(&decoded);
        drop(visitor);
        (block, should_continue)
    }

    fn opcode_count(block: &Block, opcode: Opcode) -> usize {
        block
            .instructions
            .iter()
            .filter(|instruction| instruction.opcode == opcode)
            .count()
    }

    #[test]
    fn all_sha512_sm3_sm4_visitors_dispatch_to_their_owner() {
        let cases = [
            (0xCEC0_8043, Opcode::LogicalShiftRight64),
            (0xCE61_8843, Opcode::VectorAdd64),
            (0xCE61_8043, Opcode::AndNot64),
            (0xCE61_8443, Opcode::And64),
            (0xCE61_8C43, Opcode::VectorLogicalShiftLeft64),
            (0xCE81_1C43, Opcode::VectorLogicalShiftRight64),
            (0xCE61_C043, Opcode::VectorRotateWholeVectorRight),
            (0xCE61_C443, Opcode::VectorLogicalShiftLeft32),
            (0xCEC0_8443, Opcode::SM4AccessSubstitutionBox),
            (0xCE61_C843, Opcode::SM4AccessSubstitutionBox),
        ];

        for (encoding, expected_opcode) in cases {
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert!(
                block
                    .instructions
                    .iter()
                    .any(|instruction| instruction.opcode == expected_opcode),
                "encoding 0x{encoding:08X} did not emit {expected_opcode:?}"
            );
            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        }
    }

    #[test]
    fn sha512_hash_parts_preserve_choice_and_majority_shapes() {
        let (part1, should_continue) = translate_one(0xCE61_8043);
        assert!(should_continue);
        assert_eq!(opcode_count(&part1, Opcode::AndNot64), 2);

        let (part2, should_continue) = translate_one(0xCE61_8443);
        assert!(should_continue);
        assert_eq!(opcode_count(&part2, Opcode::AndNot64), 0);
        assert!(opcode_count(&part2, Opcode::And64) > opcode_count(&part1, Opcode::And64));
    }

    #[test]
    fn sm4_hash_runs_four_rounds_and_four_sboxes_per_round() {
        for encoding in [0xCEC0_8443, 0xCE61_C843] {
            let (block, should_continue) = translate_one(encoding);
            assert!(should_continue, "encoding 0x{encoding:08X}");
            assert_eq!(opcode_count(&block, Opcode::SM4AccessSubstitutionBox), 16);
            assert_eq!(
                opcode_count(&block, Opcode::VectorRotateWholeVectorRight),
                4
            );
        }
    }
}
