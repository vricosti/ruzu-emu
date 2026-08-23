use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;
use crate::ir::a64_emitter::A64IREmitter;
use crate::ir::value::Value;

fn sha_choose(ir: &mut A64IREmitter<'_>, x: Value, y: Value, z: Value) -> Value {
    let yz = ir.ir().eor_32(y, z);
    let selected = ir.ir().and_32(yz, x);
    ir.ir().eor_32(selected, z)
}

fn sha_majority(ir: &mut A64IREmitter<'_>, x: Value, y: Value, z: Value) -> Value {
    let xy = ir.ir().and_32(x, y);
    let x_or_y = ir.ir().or_32(x, y);
    let remaining = ir.ir().and_32(x_or_y, z);
    ir.ir().or_32(xy, remaining)
}

fn sha_parity(ir: &mut A64IREmitter<'_>, x: Value, y: Value, z: Value) -> Value {
    let yz = ir.ir().eor_32(y, z);
    ir.ir().eor_32(yz, x)
}

type Sha1HashUpdateFunction = fn(&mut A64IREmitter<'_>, Value, Value, Value) -> Value;

fn sha1_hash_update(
    ir: &mut A64IREmitter<'_>,
    vm: Vec,
    vn: Vec,
    vd: Vec,
    function: Sha1HashUpdateFunction,
) -> Value {
    let mut x = ir.get_q(vd);
    let n = ir.get_q(vn);
    let mut y = ir.ir().vector_get_element(32, n, 0);
    let w = ir.get_q(vm);
    let carry = Value::ImmU1(false);

    for index in 0..4 {
        let low_x = ir.ir().vector_get_element(32, x, 0);
        let after_low_x = ir.ir().vector_get_element(32, x, 1);
        let before_high_x = ir.ir().vector_get_element(32, x, 2);
        let high_x = ir.ir().vector_get_element(32, x, 3);
        let t = function(ir, after_low_x, before_high_x, high_x);
        let w_segment = ir.ir().vector_get_element(32, w, index);

        let rotated_low_x = ir.ir().rotate_right_32(low_x, Value::ImmU8(27), carry);
        let y_plus_rotated = ir.ir().add_32(y, rotated_low_x, carry);
        let with_t = ir.ir().add_32(y_plus_rotated, t, carry);
        y = ir.ir().add_32(with_t, w_segment, carry);

        let rotated_after_low_x = ir.ir().rotate_right_32(after_low_x, Value::ImmU8(2), carry);
        x = ir.ir().vector_set_element(32, x, 1, rotated_after_low_x);
        let shuffled_x = ir.ir().vector_rotate_whole_vector_right(x, 96);
        x = ir.ir().vector_set_element(32, shuffled_x, 0, y);
        y = high_x;
    }
    x
}

impl<'a> TranslatorVisitor<'a> {
    pub fn sha1c(&mut self, inst: &DecodedInst) -> bool {
        let result = sha1_hash_update(
            &mut self.ir,
            Vec::from_u32(inst.rm()),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            sha_choose,
        );
        self.ir.set_q(Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn sha1m(&mut self, inst: &DecodedInst) -> bool {
        let result = sha1_hash_update(
            &mut self.ir,
            Vec::from_u32(inst.rm()),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            sha_majority,
        );
        self.ir.set_q(Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn sha1p(&mut self, inst: &DecodedInst) -> bool {
        let result = sha1_hash_update(
            &mut self.ir,
            Vec::from_u32(inst.rm()),
            Vec::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
            sha_parity,
        );
        self.ir.set_q(Vec::from_u32(inst.rd()), result);
        true
    }

    pub fn sha1su0(&mut self, inst: &DecodedInst) -> bool {
        let vd = Vec::from_u32(inst.rd());
        let d = self.ir.get_q(vd);
        let m = self.ir.get_q(Vec::from_u32(inst.rm()));
        let n = self.ir.get_q(Vec::from_u32(inst.rn()));
        let d_high = self.ir.ir().vector_get_element(64, d, 1);
        let n_low = self.ir.ir().vector_get_element(64, n, 0);
        let zero = self.ir.ir().zero_vector();
        let tmp1 = self.ir.ir().vector_set_element(64, zero, 0, d_high);
        let joined = self.ir.ir().vector_set_element(64, tmp1, 1, n_low);
        let joined_xor_d = self.ir.ir().vector_eor(joined, d);
        let result = self.ir.ir().vector_eor(joined_xor_d, m);
        self.ir.set_q(vd, result);
        true
    }

    pub fn sha1su1(&mut self, inst: &DecodedInst) -> bool {
        let vd = Vec::from_u32(inst.rd());
        let d = self.ir.get_q(vd);
        let n = self.ir.get_q(Vec::from_u32(inst.rn()));
        let rotated_n = self.ir.ir().vector_rotate_whole_vector_right(n, 32);
        let shuffled_n = self
            .ir
            .ir()
            .vector_set_element(32, rotated_n, 3, Value::ImmU32(0));
        let t = self.ir.ir().vector_eor(d, shuffled_n);
        let rotated_t = self.ir.ir().vector_rotate_left(32, t, 1);
        let low = self.ir.ir().vector_get_element(32, rotated_t, 0);
        let low_rotated = self
            .ir
            .ir()
            .rotate_right_32(low, Value::ImmU8(31), Value::ImmU1(false));
        let high_t = self.ir.ir().vector_get_element(32, rotated_t, 3);
        let high = self.ir.ir().eor_32(low_rotated, high_t);
        let result = self.ir.ir().vector_set_element(32, rotated_t, 3, high);
        self.ir.set_q(vd, result);
        true
    }

    pub fn sha1h(&mut self, inst: &DecodedInst) -> bool {
        let vd = Vec::from_u32(inst.rd());
        let data = self.ir.get_s(Vec::from_u32(inst.rn()));
        let left = self.ir.ir().vector_logical_shift_left(32, data, 30);
        let right = self.ir.ir().vector_logical_shift_right(32, data, 2);
        let result = self.ir.ir().vector_or(left, right);
        self.ir.set_s(vd, result);
        true
    }

    pub fn sha256su0(&mut self, inst: &DecodedInst) -> bool {
        let vd = Vec::from_u32(inst.rd());
        let x = self.ir.get_q(vd);
        let y = self.ir.get_q(Vec::from_u32(inst.rn()));
        let result = self.ir.ir().sha256_message_schedule_0(x, y);
        self.ir.set_q(vd, result);
        true
    }

    pub fn sha256su1(&mut self, inst: &DecodedInst) -> bool {
        let vd = Vec::from_u32(inst.rd());
        let x = self.ir.get_q(vd);
        let y = self.ir.get_q(Vec::from_u32(inst.rn()));
        let z = self.ir.get_q(Vec::from_u32(inst.rm()));
        let result = self.ir.ir().sha256_message_schedule_1(x, y, z);
        self.ir.set_q(vd, result);
        true
    }

    pub fn sha256h(&mut self, inst: &DecodedInst) -> bool {
        let vd = Vec::from_u32(inst.rd());
        let x = self.ir.get_q(vd);
        let y = self.ir.get_q(Vec::from_u32(inst.rn()));
        let w = self.ir.get_q(Vec::from_u32(inst.rm()));
        let result = self.ir.ir().sha256_hash(x, y, w, Value::ImmU1(true));
        self.ir.set_q(vd, result);
        true
    }

    pub fn sha256h2(&mut self, inst: &DecodedInst) -> bool {
        let vd = Vec::from_u32(inst.rd());
        let x = self.ir.get_q(Vec::from_u32(inst.rn()));
        let y = self.ir.get_q(vd);
        let w = self.ir.get_q(Vec::from_u32(inst.rm()));
        let result = self.ir.ir().sha256_hash(x, y, w, Value::ImmU1(false));
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

    #[test]
    fn sha_family_translates_without_interpreter_fallback() {
        let cases = [
            (0x5e00_0000, None),
            (0x5e00_1000, None),
            (0x5e00_2000, None),
            (0x5e00_3000, None),
            (0x5e28_0800, None),
            (0x5e28_1800, None),
            (0x5e28_2800, Some(Opcode::SHA256MessageSchedule0)),
            (0x5e00_4000, Some(Opcode::SHA256Hash)),
            (0x5e00_5000, Some(Opcode::SHA256Hash)),
            (0x5e00_6000, Some(Opcode::SHA256MessageSchedule1)),
        ];

        for (encoding, expected_opcode) in cases {
            let decoded = decode(encoding).expect("instruction should decode");
            let location = A64LocationDescriptor::new(0x1000, 0, false);
            let mut block = Block::new(location.to_location());
            let mut visitor = TranslatorVisitor::new(
                &mut block,
                location,
                crate::frontend::a64::translate::TranslationOptions::default(),
            );
            assert!(visitor.dispatch(&decoded), "encoding 0x{encoding:08x}");
            drop(visitor);
            if let Some(opcode) = expected_opcode {
                assert!(block.instructions.iter().any(|inst| inst.opcode == opcode));
            }
        }
    }
}
