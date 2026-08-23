use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::{Exception, Reg};
use crate::ir::terminal::Terminal;

/// System register encodings (op0:op1:CRn:CRm:op2 packed into 16 bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
#[allow(
    non_camel_case_types,
    clippy::upper_case_acronyms,
    clippy::unusual_byte_groupings
)]
enum SystemRegister {
    NZCV = 0b11_0100_011_000_0010,
    FPCR = 0b11_0100_011_000_0100,
    FPSR = 0b11_0100_011_001_0100,
    TPIDR_EL0 = 0b11_1101_011_010_0000,
    TPIDRRO_EL0 = 0b11_1101_011_011_0000,
    CNTFRQ_EL0 = 0b11_1110_011_000_0000,
    CNTPCT_EL0 = 0b11_1110_011_001_0000,
    CTR_EL0 = 0b11_0000_011_001_0000,
    DCZID_EL0 = 0b11_0000_011_111_0000,
}

impl<'a> TranslatorVisitor<'a> {
    fn decode_system_register(&self, inst: &DecodedInst) -> u16 {
        let o0 = inst.bits(19, 19); // 1 bit
        let op1 = inst.bits(18, 16); // 3 bits
        let crn = inst.bits(15, 12); // 4 bits
        let crm = inst.bits(11, 8); // 4 bits
        let op2 = inst.bits(7, 5); // 3 bits

        let o0_val = o0 + 2;
        // Pack matching SystemRegister encoding:
        // We encode as: o0[1:0]:CRn[3:0]:op1[2:0]:op2[2:0]:CRm[3:0] = 16 bits
        ((o0_val as u16) << 14)
            | ((crn as u16) << 10)
            | ((op1 as u16) << 7)
            | ((op2 as u16) << 4)
            | (crm as u16)
    }

    /// MRS - Read system register
    pub fn mrs(&mut self, inst: &DecodedInst) -> bool {
        let rt = Reg::from_u32(inst.rd());
        let sys_reg = self.decode_system_register(inst);

        match sys_reg {
            x if x == SystemRegister::CNTFRQ_EL0 as u16 => {
                let val = self.ir.get_cntfrq();
                self.set_x(32, rt, val);
                true
            }
            x if x == SystemRegister::CNTPCT_EL0 as u16 => {
                // If not at block start and not wall_clock, restart block
                if self.ir.base.block.inst_count() > 0 && !self.options.wall_clock_cntpct {
                    self.ir.base.block.cycle_count -= 1;
                    let loc = self.ir.current_location.unwrap();
                    self.ir.set_term(Terminal::LinkBlock {
                        next: loc.to_location(),
                    });
                    return false;
                }
                let val = self.ir.get_cntpct();
                self.set_x(64, rt, val);
                true
            }
            x if x == SystemRegister::CTR_EL0 as u16 => {
                let val = self.ir.get_ctr();
                self.set_x(32, rt, val);
                true
            }
            x if x == SystemRegister::DCZID_EL0 as u16 => {
                let val = self.ir.get_dczid();
                self.set_x(32, rt, val);
                true
            }
            x if x == SystemRegister::FPCR as u16 => {
                let val = self.ir.get_fpcr();
                self.set_x(32, rt, val);
                true
            }
            x if x == SystemRegister::FPSR as u16 => {
                let val = self.ir.get_fpsr();
                self.set_x(32, rt, val);
                true
            }
            x if x == SystemRegister::NZCV as u16 => {
                let val = self.ir.get_nzcv_raw();
                self.set_x(32, rt, val);
                true
            }
            x if x == SystemRegister::TPIDR_EL0 as u16 => {
                let val = self.ir.get_tpidr();
                self.set_x(64, rt, val);
                true
            }
            x if x == SystemRegister::TPIDRRO_EL0 as u16 => {
                let val = self.ir.get_tpidrro();
                self.set_x(64, rt, val);
                true
            }
            _ => unreachable!("decoded unsupported MRS system register"),
        }
    }

    /// MSR (register) - Write system register
    pub fn msr_reg(&mut self, inst: &DecodedInst) -> bool {
        let rt = Reg::from_u32(inst.rd());
        let sys_reg = self.decode_system_register(inst);

        match sys_reg {
            x if x == SystemRegister::FPCR as u16 => {
                let val = self.x(32, rt);
                self.ir.set_fpcr(val);
                // FPCR change requires ending the block
                let loc = self.ir.current_location.unwrap();
                let next_pc = self.ir.ir().imm64(loc.pc() + 4);
                self.ir.set_pc(next_pc);
                self.ir.set_term(Terminal::FastDispatchHint);
                false
            }
            x if x == SystemRegister::FPSR as u16 => {
                let val = self.x(32, rt);
                self.ir.set_fpsr(val);
                true
            }
            x if x == SystemRegister::NZCV as u16 => {
                let val = self.x(32, rt);
                self.ir.set_nzcv_raw(val);
                true
            }
            x if x == SystemRegister::TPIDR_EL0 as u16 => {
                let val = self.x(64, rt);
                self.ir.set_tpidr(val);
                true
            }
            _ => unreachable!("decoded unsupported MSR system register"),
        }
    }

    /// NOP
    pub fn nop(&mut self, _inst: &DecodedInst) -> bool {
        true // nothing to do
    }

    /// HINT - Generic hint instruction
    pub fn hint(&mut self, _inst: &DecodedInst) -> bool {
        true // treated as NOP
    }

    /// YIELD
    pub fn yield_inst(&mut self, _inst: &DecodedInst) -> bool {
        if !self.options.hook_hint_instructions {
            return true;
        }
        self.raise_exception(Exception::Yield)
    }

    /// WFE - Wait for event
    pub fn wfe(&mut self, _inst: &DecodedInst) -> bool {
        if !self.options.hook_hint_instructions {
            return true;
        }
        self.raise_exception(Exception::WaitForEvent)
    }

    /// WFI - Wait for interrupt
    pub fn wfi(&mut self, _inst: &DecodedInst) -> bool {
        if !self.options.hook_hint_instructions {
            return true;
        }
        self.raise_exception(Exception::WaitForInterrupt)
    }

    /// SEV - Send event
    pub fn sev(&mut self, _inst: &DecodedInst) -> bool {
        if !self.options.hook_hint_instructions {
            return true;
        }
        self.raise_exception(Exception::SendEvent)
    }

    /// SEVL - Send event local
    pub fn sevl(&mut self, _inst: &DecodedInst) -> bool {
        if !self.options.hook_hint_instructions {
            return true;
        }
        self.raise_exception(Exception::SendEventLocal)
    }

    /// CLREX - Clear exclusive monitor
    pub fn clrex(&mut self, _inst: &DecodedInst) -> bool {
        self.ir.clear_exclusive();
        true
    }

    /// DSB - Data synchronization barrier
    pub fn dsb(&mut self, _inst: &DecodedInst) -> bool {
        self.ir.data_synchronization_barrier();
        true
    }

    /// DMB - Data memory barrier
    pub fn dmb(&mut self, _inst: &DecodedInst) -> bool {
        self.ir.data_memory_barrier();
        true
    }

    /// ISB - Instruction synchronization barrier
    pub fn isb(&mut self, _inst: &DecodedInst) -> bool {
        self.ir.instruction_synchronization_barrier();
        let loc = self.ir.current_location.unwrap();
        let next_pc = self.ir.ir().imm64(loc.pc() + 4);
        self.ir.set_pc(next_pc);
        self.ir.set_term(Terminal::ReturnToDispatch);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a64::decoder::{decode, A64InstructionName};
    use crate::frontend::a64::translate::TranslationOptions;
    use crate::ir::block::Block;
    use crate::ir::location::A64LocationDescriptor;

    fn dispatch(raw: u32, expected_name: A64InstructionName) {
        let decoded = decode(raw).expect("instruction should decode");
        assert_eq!(decoded.name, expected_name);
        let location = A64LocationDescriptor::new(0x1000, 0, false);
        let mut block = Block::new(location.to_location());
        let mut visitor =
            TranslatorVisitor::new(&mut block, location, TranslationOptions::default());
        visitor.dispatch(&decoded);
    }

    #[test]
    #[should_panic(expected = "decoded unsupported MRS system register")]
    fn unsupported_decoded_mrs_is_unreachable_like_upstream() {
        dispatch(0xD530_0000, A64InstructionName::MRS);
    }

    #[test]
    #[should_panic(expected = "decoded unsupported MSR system register")]
    fn unsupported_decoded_msr_is_unreachable_like_upstream() {
        dispatch(0xD510_0000, A64InstructionName::MSR_reg);
    }
}
