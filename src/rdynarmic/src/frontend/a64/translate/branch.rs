use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Reg;
use crate::ir::cond::Cond;
use crate::ir::terminal::Terminal;

impl<'a> TranslatorVisitor<'a> {
    /// B - Unconditional branch
    pub fn b_uncond(&mut self, inst: &DecodedInst) -> bool {
        let offset = inst.imm26_sext() * 4;
        let pc = self.ir.pc();
        let target = (pc as i64).wrapping_add(offset) as u64;

        let loc = self.ir.current_location.unwrap();
        // Upstream `B_uncond`: `B .` keeps LinkBlock (its halt check stops the loop); every
        // other unconditional branch is LinkBlockFast.
        if target == pc {
            self.ir.set_term(Terminal::LinkBlock {
                next: loc.set_pc(target).to_location(),
            });
            return false;
        }
        self.ir.set_term(Terminal::LinkBlockFast {
            next: loc.set_pc(target).to_location(),
        });
        false
    }

    /// BL - Branch with link
    pub fn bl(&mut self, inst: &DecodedInst) -> bool {
        let offset = inst.imm26_sext() * 4;
        let pc = self.ir.pc();
        let target = (pc as i64).wrapping_add(offset) as u64;

        // Save return address in LR (X30)
        let return_addr = self.ir.ir().imm64(pc + 4);
        self.set_x(64, Reg::LR, return_addr);

        // Push RSB hint
        let loc = self.ir.current_location.unwrap();
        let return_loc = loc.advance_pc(4).to_location();
        self.ir.ir().push_rsb(return_loc);

        // Upstream `BL`: LinkBlockFast.
        self.ir.set_term(Terminal::LinkBlockFast {
            next: loc.set_pc(target).to_location(),
        });
        false
    }

    /// BR - Branch to register
    pub fn br(&mut self, inst: &DecodedInst) -> bool {
        let rn = Reg::from_u32(inst.rn());
        let target = self.x(64, rn);
        self.ir.set_pc(target);
        self.ir.set_term(Terminal::FastDispatchHint);
        false
    }

    /// BLR - Branch with link to register
    pub fn blr(&mut self, inst: &DecodedInst) -> bool {
        let rn = Reg::from_u32(inst.rn());
        let pc = self.ir.pc();

        let target = self.x(64, rn);

        // Save return address
        let return_addr = self.ir.ir().imm64(pc + 4);
        self.set_x(64, Reg::LR, return_addr);

        // Push RSB hint
        let loc = self.ir.current_location.unwrap();
        let return_loc = loc.advance_pc(4).to_location();
        self.ir.ir().push_rsb(return_loc);

        self.ir.set_pc(target);
        self.ir.set_term(Terminal::FastDispatchHint);
        false
    }

    /// RET - Return from subroutine
    pub fn ret(&mut self, inst: &DecodedInst) -> bool {
        let rn = Reg::from_u32(inst.rn());
        let target = self.x(64, rn);
        self.ir.set_pc(target);
        self.ir.set_term(Terminal::PopRSBHint);
        false
    }

    /// B.cond - Conditional branch
    pub fn b_cond(&mut self, inst: &DecodedInst) -> bool {
        // ARM B.cond encoding: 0_1010100_imm19_0_cond — cond is at bits[3:0],
        // NOT bits[15:12] (that's where CCMN/CCMP put their cond field).
        // The shared `inst.cond_field()` helper extracts bits[15:12], which is
        // wrong here. Using it caused every B.cond to use cond=0 (= EQ) regardless
        // of the actual encoding — STK boot wedge investigation 2026-05-03.
        let cond = Cond::from_u8(inst.bits(3, 0) as u8);
        let offset = inst.imm19_sext() * 4;
        let pc = self.ir.pc();
        let target = (pc as i64).wrapping_add(offset) as u64;

        let loc = self.ir.current_location.unwrap();
        let target_loc = loc.set_pc(target).to_location();
        let fallthrough_loc = loc.advance_pc(4).to_location();

        self.ir.set_term(Terminal::if_then_else(
            cond,
            Terminal::LinkBlock { next: target_loc },
            Terminal::LinkBlock {
                next: fallthrough_loc,
            },
        ));
        false
    }

    /// CBZ - Compare and branch if zero
    pub fn cbz(&mut self, inst: &DecodedInst) -> bool {
        let sf = inst.sf();
        let rt = Reg::from_u32(inst.rd()); // Rt is in Rd field for CBZ/CBNZ
        let offset = inst.imm19_sext() * 4;
        let pc = self.ir.pc();
        let target = (pc as i64).wrapping_add(offset) as u64;
        let datasize = if sf { 64 } else { 32 };

        let operand = self.x(datasize, rt);
        let is_zero = match datasize {
            32 => self.ir.ir().is_zero_32(operand),
            _ => self.ir.ir().is_zero_64(operand),
        };
        self.ir.set_check_bit(is_zero);

        let loc = self.ir.current_location.unwrap();
        let target_loc = loc.set_pc(target).to_location();
        let fallthrough_loc = loc.advance_pc(4).to_location();

        self.ir.set_term(Terminal::check_bit(
            Terminal::LinkBlock { next: target_loc }, // if zero (check_bit=true)
            Terminal::LinkBlock {
                next: fallthrough_loc,
            }, // if not zero
        ));
        false
    }

    /// CBNZ - Compare and branch if not zero
    pub fn cbnz(&mut self, inst: &DecodedInst) -> bool {
        let sf = inst.sf();
        let rt = Reg::from_u32(inst.rd());
        let offset = inst.imm19_sext() * 4;
        let pc = self.ir.pc();
        let target = (pc as i64).wrapping_add(offset) as u64;
        let datasize = if sf { 64 } else { 32 };

        let operand = self.x(datasize, rt);
        let is_zero = match datasize {
            32 => self.ir.ir().is_zero_32(operand),
            _ => self.ir.ir().is_zero_64(operand),
        };
        self.ir.set_check_bit(is_zero);

        let loc = self.ir.current_location.unwrap();
        let target_loc = loc.set_pc(target).to_location();
        let fallthrough_loc = loc.advance_pc(4).to_location();

        // Swapped compared to CBZ: branch if NOT zero
        self.ir.set_term(Terminal::check_bit(
            Terminal::LinkBlock {
                next: fallthrough_loc,
            }, // if zero (check_bit=true), don't branch
            Terminal::LinkBlock { next: target_loc }, // if not zero, branch
        ));
        false
    }

    /// TBZ - Test bit and branch if zero
    pub fn tbz(&mut self, inst: &DecodedInst) -> bool {
        let b5 = inst.bit(31) as u8;
        let b40 = inst.bits(23, 19) as u8;
        let bit_pos = (b5 << 5) | b40;
        let rt = Reg::from_u32(inst.rd());
        let datasize = if b5 != 0 { 64 } else { 32 };

        let imm14 = inst.bits(18, 5);
        let offset = (((imm14 as i32) << 18) >> 18) as i64 * 4;
        let pc = self.ir.pc();
        let target = (pc as i64).wrapping_add(offset) as u64;

        let operand = self.x(datasize, rt);
        // TestBit needs a 64-bit value
        let operand_64 = if datasize == 32 {
            self.ir.ir().zero_extend_word_to_long(operand)
        } else {
            operand
        };
        let bit_val = self.ir.ir().imm8(bit_pos);
        let test = self.ir.ir().test_bit(operand_64, bit_val);
        self.ir.set_check_bit(test);

        let loc = self.ir.current_location.unwrap();
        let target_loc = loc.set_pc(target).to_location();
        let fallthrough_loc = loc.advance_pc(4).to_location();

        // TestBit returns true if bit is set; TBZ branches if bit is zero
        self.ir.set_term(Terminal::check_bit(
            Terminal::LinkBlock {
                next: fallthrough_loc,
            }, // bit=1 (check_bit=true), don't branch
            Terminal::LinkBlock { next: target_loc }, // bit=0, branch
        ));
        false
    }

    /// TBNZ - Test bit and branch if not zero
    pub fn tbnz(&mut self, inst: &DecodedInst) -> bool {
        let b5 = inst.bit(31) as u8;
        let b40 = inst.bits(23, 19) as u8;
        let bit_pos = (b5 << 5) | b40;
        let rt = Reg::from_u32(inst.rd());
        let datasize = if b5 != 0 { 64 } else { 32 };

        let imm14 = inst.bits(18, 5);
        let offset = (((imm14 as i32) << 18) >> 18) as i64 * 4;
        let pc = self.ir.pc();
        let target = (pc as i64).wrapping_add(offset) as u64;

        let operand = self.x(datasize, rt);
        let operand_64 = if datasize == 32 {
            self.ir.ir().zero_extend_word_to_long(operand)
        } else {
            operand
        };
        let bit_val = self.ir.ir().imm8(bit_pos);
        let test = self.ir.ir().test_bit(operand_64, bit_val);
        self.ir.set_check_bit(test);

        let loc = self.ir.current_location.unwrap();
        let target_loc = loc.set_pc(target).to_location();
        let fallthrough_loc = loc.advance_pc(4).to_location();

        // TBNZ: branch if bit is set (check_bit=true)
        self.ir.set_term(Terminal::check_bit(
            Terminal::LinkBlock { next: target_loc }, // bit=1, branch
            Terminal::LinkBlock {
                next: fallthrough_loc,
            }, // bit=0, don't branch
        ));
        false
    }
}
