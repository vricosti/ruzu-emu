// Port of eden-nx2 frontend/A64/translate/impl/pointer_authentication.cpp.
use super::visitor::TranslatorVisitor;
use crate::frontend::a64::{decoder::DecodedInst, types::Reg};
use crate::ir::terminal::Terminal;

impl<'a> TranslatorVisitor<'a> {
    // NX2's identity PAC model does not insert or strip pointer authentication bits.
    pub fn pacaut_reg(&mut self, inst: &DecodedInst) -> bool {
        if inst.bit(13) && inst.bits(9, 5) != 31 {
            return self.unallocated_encoding();
        }
        true
    }

    pub fn xpac_1(&mut self, _inst: &DecodedInst) -> bool {
        true
    }

    pub fn pacga(&mut self, inst: &DecodedInst) -> bool {
        let rm = Reg::from_u32(inst.bits(20, 16));
        let rn = Reg::from_u32(inst.bits(9, 5));
        let rd = Reg::from_u32(inst.bits(4, 0));
        // Match NX2's deterministic authentication code, including SP for Rm.
        let modifier = if rm == Reg::SP { self.sp(64) } else { self.x(64, rm) };
        let value = self.x(64, rn);
        let code = self.ir.ir().eor_64(value, modifier);
        let mask = self.ir.ir().imm64(0xffff_ffff_0000_0000);
        let result = self.ir.ir().and_64(code, mask);
        self.set_x(64, rd, result);
        true
    }

    pub fn ldra(&mut self, inst: &DecodedInst) -> bool {
        let rn = Reg::from_u32(inst.bits(9, 5));
        let rt = Reg::from_u32(inst.bits(4, 0));
        let immediate = (inst.bits(22, 22) << 9) | inst.bits(20, 12);
        let offset = (((immediate as i64) << 54) >> 54) << 3;
        let base = if rn == Reg::SP { self.sp(64) } else { self.x(64, rn) };
        let offset = self.ir.ir().imm64(offset as u64);
        let address = self.addr_add(base, offset);
        let data = self.mem_read(address, 8, crate::frontend::a64::types::AccType::Normal);
        if inst.bit(11) {
            if rn == Reg::SP {
                self.set_sp(64, address);
            } else {
                self.set_x(64, rn, address);
            }
        }
        self.set_x(64, rt, data);
        true
    }

    pub fn bra_blra(&mut self, inst: &DecodedInst) -> bool {
        let z = inst.bits(24, 24) != 0;
        let link = inst.bits(21, 21) != 0;
        let rn = Reg::from_u32(inst.bits(9, 5));
        let rm = inst.bits(4, 0);
        if !z && rm != 31 {
            return self.unallocated_encoding();
        }
        let target = self.x(64, rn);
        if link {
            let pc = self.ir.pc();
            let return_address = self.ir.ir().imm64(pc + 4);
            self.set_x(64, Reg::LR, return_address);
            let location = self.ir.current_location.unwrap().advance_pc(4).to_location();
            self.ir.ir().push_rsb(location);
        }
        self.ir.set_pc(target);
        self.ir.set_term(Terminal::FastDispatchHint);
        false
    }

    // Upstream models pointer authentication as identity; both key variants use LR.
    pub fn reta(&mut self, _inst: &DecodedInst) -> bool {
        let target = self.x(64, Reg::LR);
        self.ir.set_pc(target);
        self.ir.set_term(Terminal::PopRSBHint);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a64::translate::{translate, TranslationOptions};
    use crate::ir::location::A64LocationDescriptor;

    #[test]
    fn ldra_sign_extends_scaled_offset_and_writes_back_before_destination() {
        use crate::ir::{opcode::Opcode, value::Value};
        for offset in [-4096i64, -8, 0, 4088] {
            for rn in [2, 31] {
                for writeback in [false, true] {
                    for key in [false, true] {
                        let immediate = ((offset / 8) as u32) & 1023;
                        let word = 0xf8200400 | ((key as u32) << 23)
                            | ((immediate >> 9) << 22) | ((immediate & 511) << 12)
                            | ((writeback as u32) << 11) | (rn << 5) | rn;
                        let block = translate(
                            A64LocationDescriptor::new(0x1000, 0, false),
                            &|pc| match pc { 0x1000 => Some(word), 0x1004 => Some(0xd65f03c0), _ => None },
                            TranslationOptions::default(),
                        );
                        assert!(!block.instructions.iter().any(|i| i.opcode == Opcode::A64ExceptionRaised));
                        let add = block.instructions.iter().find(|i| i.opcode == Opcode::Add64).unwrap();
                        assert_eq!(add.args[1], Value::ImmU64(offset as u64));
                        let Value::Inst(base) = add.args[0] else { panic!("base register expected") };
                        assert_eq!(block.instructions[base.index()].opcode,
                            if rn == 31 { Opcode::A64GetSP } else { Opcode::A64GetX });
                        let load = block.instructions.iter().position(|i| i.opcode == Opcode::A64ReadMemory64).unwrap();
                        let writes: Vec<_> = block.instructions.iter().enumerate()
                            .filter(|(_, i)| matches!(i.opcode, Opcode::A64SetX | Opcode::A64SetSP)).collect();
                        assert_eq!(writes.len(), usize::from(writeback) + usize::from(rn != 31));
                        assert!(writes.iter().all(|(index, _)| *index > load));
                        if writeback {
                            let Value::Inst(address) = writes[0].1.args[if rn == 31 { 0 } else { 1 }] else { panic!("writeback address expected") };
                            assert_eq!(block.instructions[address.index()].opcode, Opcode::Add64);
                        }
                        if rn != 31 {
                            let Value::Inst(value) = writes.last().unwrap().1.args[1] else { panic!("loaded value expected") };
                            assert_eq!(value.index(), load);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn pacga_uses_upper_xor_bits_and_distinguishes_sp_from_zero_register() {
        use crate::ir::{opcode::Opcode, value::Value};
        for (rm, rn, rd) in [(6, 29, 6), (31, 2, 3), (4, 31, 5), (31, 31, 31)] {
            let word = 0x9ac03000 | (rm << 16) | (rn << 5) | rd;
            let block = translate(
                A64LocationDescriptor::new(0x1000, 0, false),
                &|pc| match pc { 0x1000 => Some(word), 0x1004 => Some(0xd65f03c0), _ => None },
                TranslationOptions::default(),
            );
            assert!(!block.instructions.iter().any(|i| i.opcode == Opcode::A64ExceptionRaised));
            let xor = block.instructions.iter().find(|i| i.opcode == Opcode::Eor64).unwrap();
            if rn == 31 {
                assert_eq!(xor.args[0], Value::ImmU64(0));
            } else {
                let Value::Inst(source) = xor.args[0] else { panic!("register input expected") };
                let read = &block.instructions[source.index()];
                assert_eq!(read.opcode, Opcode::A64GetX);
                assert_eq!(read.args[0], Value::ImmA64Reg(Reg::from_u32(rn)));
            }
            let Value::Inst(modifier) = xor.args[1] else { panic!("modifier input expected") };
            let read = &block.instructions[modifier.index()];
            assert_eq!(read.opcode, if rm == 31 { Opcode::A64GetSP } else { Opcode::A64GetX });
            if rm != 31 {
                assert_eq!(read.args[0], Value::ImmA64Reg(Reg::from_u32(rm)));
            }
            let mask = block.instructions.iter().find(|i| i.opcode == Opcode::And64).unwrap();
            assert_eq!(mask.args[1], Value::ImmU64(0xffff_ffff_0000_0000));
            let Value::Inst(code) = mask.args[0] else { panic!("authentication code expected") };
            assert_eq!(block.instructions[code.index()].opcode, Opcode::Eor64);
            let writes: Vec<_> = block.instructions.iter().filter(|i| i.opcode == Opcode::A64SetX).collect();
            assert_eq!(writes.len(), usize::from(rd != 31));
            if rd != 31 {
                assert_eq!(writes[0].args[0], Value::ImmA64Reg(Reg::from_u32(rd)));
                let Value::Inst(result) = writes[0].args[1] else { panic!("masked result expected") };
                assert_eq!(block.instructions[result.index()].opcode, Opcode::And64);
            }
            assert!(!block.instructions.iter().any(|i| i.opcode == Opcode::A64SetSP));
        }
    }

    #[test]
    fn pac_identity_forms_validate_zero_modifier() {
        let translate_word = |word| translate(
            A64LocationDescriptor::new(0x1000, 0, false),
            &|pc| match pc { 0x1000 => Some(word), 0x1004 => Some(0xd65f03c0), _ => None },
            TranslationOptions::default(),
        );
        let reference = translate_word(0xd503201f); // NOP followed by RET
        for op in 0..8 {
            for rn in [0, 30, 31] {
                for zero in [false, true] {
                    let word = 0xdac10000 | ((zero as u32) << 13) | (op << 10) | (rn << 5) | 2;
                    let block = translate_word(word);
                    if zero && rn != 31 {
                        assert!(block.instructions.iter().any(|i| i.opcode == crate::ir::opcode::Opcode::A64ExceptionRaised));
                    } else {
                        assert_eq!(format!("{:?}", block.instructions), format!("{:?}", reference.instructions));
                    }
                }
            }
        }
        for word in [0xdac143e2, 0xdac147e2] { // XPACI / XPACD X2
            let block = translate_word(word);
            assert_eq!(format!("{:?}", block.instructions), format!("{:?}", reference.instructions));
        }
    }

    #[test]
    fn authenticated_branches_preserve_target_and_link_order() {
        let translate_word = |word| translate(
            A64LocationDescriptor::new(0x1000, 0, false),
            &|pc| (pc == 0x1000).then_some(word),
            TranslationOptions::default(),
        );
        for (authenticated, plain) in [
            (0xd71f0801, 0xd61f0000), // BRAA X0, X1 / BR X0
            (0xd61f081f, 0xd61f0000), // BRAAZ X0
            (0xd73f0bc1, 0xd63f03c0), // BLRAA X30, X1 / BLR X30 (target before LR write)
            (0xd63f0fff, 0xd63f03e0), // BLRABZ X31 / BLR X31
        ] {
            let block = translate_word(authenticated);
            let reference = translate_word(plain);
            assert!(matches!(block.terminal, Terminal::FastDispatchHint));
            assert_eq!(format!("{:?}", block.instructions), format!("{:?}", reference.instructions));
        }
        let invalid = translate_word(0xd61f0800); // Z form requires Rm == 31
        assert!(invalid.instructions.iter().any(|i| i.opcode == crate::ir::opcode::Opcode::A64ExceptionRaised));
    }

    #[test]
    fn authenticated_returns_match_upstream_plain_lr_return() {
        let translate_word = |word| translate(
            A64LocationDescriptor::new(0x1000, 0, false),
            &|pc| (pc == 0x1000).then_some(word),
            TranslationOptions::default(),
        );
        let reference = translate_word(0xd65f03c0); // RET X30
        for word in [0xd65f0bff, 0xd65f0fff] { // RETAA / RETAB
            let block = translate_word(word);
            assert!(matches!(block.terminal, Terminal::PopRSBHint));
            let actual: Vec<_> = block.instructions.iter().map(|i| format!("{:?}", i)).collect();
            let expected: Vec<_> = reference.instructions.iter().map(|i| format!("{:?}", i)).collect();
            assert_eq!(actual, expected);
        }
    }
}
