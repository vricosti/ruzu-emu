// Port of eden-nx2 frontend/A64/translate/impl/load_store_atomic.cpp.
use super::visitor::TranslatorVisitor;
use crate::frontend::a64::{decoder::DecodedInst, types::{AccType, Reg}};
use crate::ir::value::Value;

fn atomic_address(v: &mut TranslatorVisitor<'_>, rn: Reg) -> Value {
    if rn == Reg::SP { v.sp(64) } else { v.x(64, rn) }
}

fn atomic_mask_if_zero(v: &mut TranslatorVisitor<'_>, value: Value) -> Value {
    let negative = v.ir.ir().sub_64(Value::ImmU64(0), value, Value::ImmU1(true));
    let combined = v.ir.ir().or_64(value, negative);
    let nonzero = v.ir.ir().logical_shift_right_64(combined, Value::ImmU8(63));
    v.ir.ir().sub_64(nonzero, Value::ImmU64(1), Value::ImmU1(true))
}
fn atomic_select(v: &mut TranslatorVisitor<'_>, mask: Value, if_set: Value, if_clear: Value) -> Value {
    let set = v.ir.ir().and_64(if_set, mask);
    let clear = v.ir.ir().and_not_64(if_clear, mask);
    v.ir.ir().or_64(set, clear)
}
fn atomic_truncate(v: &mut TranslatorVisitor<'_>, value: Value, bytes: usize) -> Value {
    match bytes {
        1 => v.ir.ir().least_significant_byte(value),
        2 => v.ir.ir().least_significant_half(value),
        4 => v.ir.ir().least_significant_word(value),
        _ => value,
    }
}
fn atomic_load_zext(v: &mut TranslatorVisitor<'_>, address: Value, bytes: usize, access: AccType) -> Value {
    let value = v.exclusive_mem_read(address, bytes, access);
    if bytes == 8 { value } else { v.ir.ir().zero_extend_to_long(value) }
}
fn atomic_zero_extend_reg(v: &mut TranslatorVisitor<'_>, r: Reg, bytes: usize) -> Value {
    let value = v.x(64, r);
    if bytes == 8 { value } else {
        let value = atomic_truncate(v, value, bytes);
        v.ir.ir().zero_extend_to_long(value)
    }
}
fn atomic_commit_reg(v: &mut TranslatorVisitor<'_>, r: Reg, ok: Value, value: Value) {
    if r == Reg::ZR { return; }
    let previous = v.x(64, r);
    let value = atomic_select(v, ok, value, previous);
    v.set_x(64, r, value);
}
fn atomic_finish(v: &mut TranslatorVisitor<'_>, status: Value) -> bool {
    use crate::ir::terminal::Terminal;
    let success = v.ir.ir().is_zero_32(status);
    v.ir.set_check_bit(success);
    let location = v.ir.current_location.unwrap();
    v.ir.set_term(Terminal::check_bit(
        Terminal::LinkBlock { next: location.advance_pc(4).to_location() },
        Terminal::LinkBlock { next: location.to_location() },
    ));
    false
}
fn atomic_acc_type(acquire: bool, release: bool) -> AccType {
    if acquire || release { AccType::Ordered } else { AccType::Atomic }
}

fn atomic_sign_extend(v: &mut TranslatorVisitor<'_>, value: Value, bytes: usize) -> Value {
    if bytes == 8 { value } else {
        let value = atomic_truncate(v, value, bytes);
        v.ir.ir().sign_extend_to_long(value)
    }
}

impl<'a> TranslatorVisitor<'a> {
    pub fn atomic_mem_op(&mut self, inst: &DecodedInst) -> bool {
        let bytes = 1usize << inst.bits(31, 30);
        let op = inst.bits(15, 12);
        if op > 8 { return self.unallocated_encoding(); }
        let address = atomic_address(self, Reg::from_u32(inst.rn()));
        let access = atomic_acc_type(inst.bit(23), inst.bit(22));
        let old = atomic_load_zext(self, address, bytes, access);
        let source = atomic_zero_extend_reg(self, Reg::from_u32(inst.bits(20, 16)), bytes);
        let result = match op {
            0 => self.ir.ir().add_64(old, source, Value::ImmU1(false)),
            1 => self.ir.ir().and_not_64(old, source),
            2 => self.ir.ir().eor_64(old, source),
            3 => self.ir.ir().or_64(old, source),
            4 | 5 => {
                let a = atomic_sign_extend(self, old, bytes);
                let b = atomic_sign_extend(self, source, bytes);
                if op == 4 { self.ir.ir().max_signed(64, a, b) }
                else { self.ir.ir().min_signed(64, a, b) }
            }
            6 => self.ir.ir().max_unsigned(64, old, source),
            7 => self.ir.ir().min_unsigned(64, old, source),
            _ => source,
        };
        let truncated = atomic_truncate(self, result, bytes);
        let status = self.exclusive_mem_write(address, bytes, access, truncated);
        let status64 = self.ir.ir().zero_extend_to_long(status);
        let ok = atomic_mask_if_zero(self, status64);
        atomic_commit_reg(self, Reg::from_u32(inst.rd()), ok, old);
        atomic_finish(self, status)
    }

    pub fn compare_and_swap(&mut self, inst: &DecodedInst) -> bool {
        let bytes = 1usize << inst.bits(31, 30);
        let rs = Reg::from_u32(inst.bits(20, 16));
        let address = atomic_address(self, Reg::from_u32(inst.rn()));
        let access = atomic_acc_type(inst.bit(22), inst.bit(15));
        let old = atomic_load_zext(self, address, bytes, access);
        let expected = atomic_zero_extend_reg(self, rs, bytes);
        let desired = atomic_zero_extend_reg(self, Reg::from_u32(inst.rd()), bytes);
        let difference = self.ir.ir().eor_64(old, expected);
        let equal = atomic_mask_if_zero(self, difference);
        let result = atomic_select(self, equal, desired, old);
        let truncated = atomic_truncate(self, result, bytes);
        let status = self.exclusive_mem_write(address, bytes, access, truncated);
        let status64 = self.ir.ir().zero_extend_to_long(status);
        let ok = atomic_mask_if_zero(self, status64);
        atomic_commit_reg(self, rs, ok, old);
        atomic_finish(self, status)
    }

    pub fn ldapr_sz(&mut self, inst: &DecodedInst) -> bool {
        let bytes = 1usize << inst.bits(31, 30);
        let regsize = if bytes == 8 { 64 } else { 32 };
        let address = atomic_address(self, Reg::from_u32(inst.rn()));
        let data = self.mem_read(address, bytes, AccType::Ordered);
        let extended = self.zero_extend(data, regsize);
        self.set_x(regsize, Reg::from_u32(inst.rd()), extended);
        true
    }
}

#[cfg(test)]
mod tests {
    use crate::frontend::a64::translate::{translate, TranslationOptions};
    use crate::ir::location::A64LocationDescriptor;

    #[test]
    fn cas_uses_exclusive_access_and_retries_at_same_pc() {
        use crate::ir::{opcode::Opcode, terminal::Terminal};
        let reads = [Opcode::A64ExclusiveReadMemory8, Opcode::A64ExclusiveReadMemory16, Opcode::A64ExclusiveReadMemory32, Opcode::A64ExclusiveReadMemory64];
        let writes = [Opcode::A64ExclusiveWriteMemory8, Opcode::A64ExclusiveWriteMemory16, Opcode::A64ExclusiveWriteMemory32, Opcode::A64ExclusiveWriteMemory64];
        for size in 0..4usize {
            for ordering in [0, 1 << 22, 1 << 15, (1 << 22) | (1 << 15)] {
                let word = 0x08a07c00 | ((size as u32) << 30) | ordering | (10 << 16) | (19 << 5) | 11;
                let location = A64LocationDescriptor::new(0x1000, 0, false);
                let block = translate(location, &|pc| (pc == 0x1000).then_some(word), TranslationOptions::default());
                let read = block.instructions.iter().position(|i| i.opcode == reads[size]).unwrap();
                let write = block.instructions.iter().position(|i| i.opcode == writes[size]).unwrap();
                assert!(read < write);
                let Terminal::CheckBit { then_, else_ } = block.terminal else { panic!("CAS must branch on exclusive status") };
                assert!(matches!(*then_, Terminal::LinkBlock { next } if next == location.advance_pc(4).to_location()));
                assert!(matches!(*else_, Terminal::LinkBlock { next } if next == location.to_location()));
            }
        }
    }

    #[test]
    fn rcpc_loads_match_upstream_ordered_load_semantics() {
        let translate_word = |word| translate(
            A64LocationDescriptor::new(0x1000, 0, false),
            &|pc| match pc { 0x1000 => Some(word), 0x1004 => Some(0xd65f03c0), _ => None },
            TranslationOptions::default(),
        );
        for size in 0..4 {
            for rn in [0, 8, 31] {
                for rt in [0, 8, 31] {
                    let fields = (size << 30) | (rn << 5) | rt;
                    let actual = translate_word(0x38bfc000 | fields);
                    let reference = translate_word(0x08dffc00 | fields); // LDAR{B,H}
                    assert_eq!(format!("{:?}", actual.instructions), format!("{:?}", reference.instructions));
                }
            }
        }
    }
}
