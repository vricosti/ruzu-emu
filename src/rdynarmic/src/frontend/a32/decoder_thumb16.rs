use crate::frontend::a32::types::Reg;

/// Decoded Thumb16 instruction.
#[derive(Debug, Clone, Copy)]
pub struct DecodedThumb16 {
    pub raw: u16,
    pub id: Thumb16InstId,
}

/// Thumb16 instruction identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Thumb16InstId {
    // Shift, Add, Subtract
    LslImm,
    LsrImm,
    AsrImm,
    AddRegT1,
    SubReg,
    AddImmT1,
    SubImmT1,
    MovImm,
    CmpImm,
    AddImmT2,
    SubImmT2,
    // Data processing
    AndReg,
    EorReg,
    LslReg,
    LsrReg,
    AsrReg,
    AdcReg,
    SbcReg,
    RorReg,
    TstReg,
    RsbImm,
    CmpRegT1,
    CmnReg,
    OrrReg,
    MulReg,
    BicReg,
    MvnReg,
    // Special data
    AddRegT2,
    CmpRegT2,
    MovReg,
    // Load/Store
    LdrLiteral,
    StrReg,
    StrhReg,
    StrbReg,
    LdrsbReg,
    LdrReg,
    LdrhReg,
    LdrbReg,
    LdrshReg,
    StrImmT1,
    LdrImmT1,
    StrbImm,
    LdrbImm,
    StrhImm,
    LdrhImm,
    StrImmT2,
    LdrImmT2,
    // Address generation
    ADR,
    AddSpT1,
    AddSpT2,
    SubSp,
    // Extensions
    SXTH,
    SXTB,
    UXTH,
    UXTB,
    // Misc
    PUSH,
    POP,
    REV,
    REV16,
    REVSH,
    NOP,
    SEV,
    SEVL,
    WFE,
    WFI,
    YIELD,
    BKPT,
    IT,
    SETEND,
    CPS,
    CbzCbnz,
    // Load/Store multiple
    STMIA,
    LDMIA,
    // Branch
    BX,
    BlxReg,
    BT1,
    BT2,
    SVC,
    UDF,
    // Unknown
    Unknown,
}

impl DecodedThumb16 {
    /// Extract Rd (bits [2:0]).
    pub fn rd_lo(&self) -> Reg {
        Reg::from_u8((self.raw & 7) as u8)
    }
    /// Extract Rn (bits [5:3]).
    pub fn rn_lo(&self) -> Reg {
        Reg::from_u8(((self.raw >> 3) & 7) as u8)
    }
    /// Extract Rm (bits [8:6]).
    pub fn rm_lo(&self) -> Reg {
        Reg::from_u8(((self.raw >> 6) & 7) as u8)
    }
    /// Extract Rt (bits [10:8]) for SP-relative and literal loads.
    pub fn rt_hi(&self) -> Reg {
        Reg::from_u8(((self.raw >> 8) & 7) as u8)
    }
    /// Extract Rm for special data instructions (bits [6:3]).
    pub fn rm_hi(&self) -> Reg {
        Reg::from_u8(((self.raw >> 3) & 0xF) as u8)
    }

    /// Extract Rd with D bit (bit 7) for special data instructions.
    pub fn rd_dn(&self) -> Reg {
        let d = ((self.raw >> 7) & 1) as u8;
        let rd = (self.raw & 7) as u8;
        Reg::from_u8((d << 3) | rd)
    }
    /// Extract Rn with N bit (bit 7) for CMP high register.
    pub fn rn_dn(&self) -> Reg {
        let n = ((self.raw >> 7) & 1) as u8;
        let rn = (self.raw & 7) as u8;
        Reg::from_u8((n << 3) | rn)
    }

    /// Extract 5-bit immediate (bits [10:6]).
    pub fn imm5(&self) -> u32 {
        ((self.raw >> 6) & 0x1F) as u32
    }
    /// Extract 3-bit immediate (bits [8:6]).
    pub fn imm3(&self) -> u32 {
        ((self.raw >> 6) & 7) as u32
    }
    /// Extract 8-bit immediate (bits [7:0]).
    pub fn imm8(&self) -> u32 {
        (self.raw & 0xFF) as u32
    }
    /// Extract 7-bit immediate (bits [6:0]).
    pub fn imm7(&self) -> u32 {
        (self.raw & 0x7F) as u32
    }
    /// Extract register list (bits [7:0]).
    pub fn register_list(&self) -> u16 {
        self.raw & 0xFF
    }
    /// Extract condition (bits [11:8]) for conditional branches.
    pub fn cond(&self) -> u8 {
        ((self.raw >> 8) & 0xF) as u8
    }
    /// Extract 11-bit immediate (bits [10:0]).
    pub fn imm11(&self) -> u32 {
        (self.raw & 0x7FF) as u32
    }
}

/// Decode a 16-bit Thumb instruction.
pub fn decode_thumb16(instr: u16) -> DecodedThumb16 {
    let opcode = (instr >> 10) & 0x3F;

    let id = match opcode >> 2 {
        // 00xxxx: Shift, add, subtract, move, compare
        0b0000..=0b0011 => decode_thumb16_shift_add(instr),
        // 010000: Data processing
        0b0100 if (opcode & 3) == 0 => decode_thumb16_dp(instr),
        // 010001: Special data / branch exchange
        0b0100 if (opcode & 3) == 1 => decode_thumb16_special(instr),
        // 01001x: LDR literal
        0b0100 if (opcode & 2) == 2 => Thumb16InstId::LdrLiteral,
        // 0101xx, 011xxx, 100xxx: Load/Store
        0b0101..=0b1001 => decode_thumb16_load_store(instr),
        // 1010xx: Generate PC/SP-relative address
        0b1010 => {
            if opcode & 2 == 0 {
                Thumb16InstId::ADR
            } else {
                Thumb16InstId::AddSpT1
            }
        }
        // 1011xx: Miscellaneous
        0b1011 => decode_thumb16_misc(instr),
        // 1100xx: STM/LDM
        0b1100 => {
            if opcode & 2 == 0 {
                Thumb16InstId::STMIA
            } else {
                Thumb16InstId::LDMIA
            }
        }
        // 1101xx: Conditional branch / SVC
        0b1101 => decode_thumb16_cond_branch(instr),
        // 11100x: Unconditional branch
        0b1110 if opcode & 2 == 0 => Thumb16InstId::BT2,
        _ => Thumb16InstId::Unknown,
    };

    DecodedThumb16 { raw: instr, id }
}

fn decode_thumb16_shift_add(instr: u16) -> Thumb16InstId {
    let op = (instr >> 11) & 0x1F;
    match op {
        0b00000 => Thumb16InstId::LslImm,
        0b00001 => Thumb16InstId::LsrImm,
        0b00010 => Thumb16InstId::AsrImm,
        0b00011 => {
            let op2 = (instr >> 9) & 3;
            match op2 {
                0b00 => Thumb16InstId::AddRegT1,
                0b01 => Thumb16InstId::SubReg,
                0b10 => Thumb16InstId::AddImmT1,
                0b11 => Thumb16InstId::SubImmT1,
                _ => unreachable!(),
            }
        }
        0b00100 => Thumb16InstId::MovImm,
        0b00101 => Thumb16InstId::CmpImm,
        0b00110 => Thumb16InstId::AddImmT2,
        0b00111 => Thumb16InstId::SubImmT2,
        _ => Thumb16InstId::Unknown,
    }
}

fn decode_thumb16_dp(instr: u16) -> Thumb16InstId {
    let op = (instr >> 6) & 0xF;
    match op {
        0b0000 => Thumb16InstId::AndReg,
        0b0001 => Thumb16InstId::EorReg,
        0b0010 => Thumb16InstId::LslReg,
        0b0011 => Thumb16InstId::LsrReg,
        0b0100 => Thumb16InstId::AsrReg,
        0b0101 => Thumb16InstId::AdcReg,
        0b0110 => Thumb16InstId::SbcReg,
        0b0111 => Thumb16InstId::RorReg,
        0b1000 => Thumb16InstId::TstReg,
        0b1001 => Thumb16InstId::RsbImm,
        0b1010 => Thumb16InstId::CmpRegT1,
        0b1011 => Thumb16InstId::CmnReg,
        0b1100 => Thumb16InstId::OrrReg,
        0b1101 => Thumb16InstId::MulReg,
        0b1110 => Thumb16InstId::BicReg,
        0b1111 => Thumb16InstId::MvnReg,
        _ => unreachable!(),
    }
}

fn decode_thumb16_special(instr: u16) -> Thumb16InstId {
    let op = (instr >> 8) & 3;
    match op {
        0b00 => Thumb16InstId::AddRegT2,
        0b01 => Thumb16InstId::CmpRegT2,
        0b10 => Thumb16InstId::MovReg,
        0b11 => {
            if instr & (1 << 7) != 0 {
                Thumb16InstId::BlxReg
            } else {
                Thumb16InstId::BX
            }
        }
        _ => unreachable!(),
    }
}

fn decode_thumb16_load_store(instr: u16) -> Thumb16InstId {
    let op = (instr >> 9) & 0x7F;
    match op >> 3 {
        0b0101 => match op & 7 {
            0b000 => Thumb16InstId::StrReg,
            0b001 => Thumb16InstId::StrhReg,
            0b010 => Thumb16InstId::StrbReg,
            0b011 => Thumb16InstId::LdrsbReg,
            0b100 => Thumb16InstId::LdrReg,
            0b101 => Thumb16InstId::LdrhReg,
            0b110 => Thumb16InstId::LdrbReg,
            0b111 => Thumb16InstId::LdrshReg,
            _ => unreachable!(),
        },
        0b0110 => {
            if op & 4 == 0 {
                Thumb16InstId::StrImmT1
            } else {
                Thumb16InstId::LdrImmT1
            }
        }
        0b0111 => {
            if op & 4 == 0 {
                Thumb16InstId::StrbImm
            } else {
                Thumb16InstId::LdrbImm
            }
        }
        0b1000 => {
            if op & 4 == 0 {
                Thumb16InstId::StrhImm
            } else {
                Thumb16InstId::LdrhImm
            }
        }
        0b1001 => {
            if op & 4 == 0 {
                Thumb16InstId::StrImmT2
            } else {
                Thumb16InstId::LdrImmT2
            }
        }
        _ => Thumb16InstId::Unknown,
    }
}

fn decode_thumb16_misc(instr: u16) -> Thumb16InstId {
    // Misc 16-bit: bits[15:12] = 1011
    // Use bits[11:8] for primary decode, with sub-fields as needed
    let op119 = (instr >> 9) & 7; // bits[11:9]
    let op118 = (instr >> 8) & 0xF; // bits[11:8]
    let op = (instr >> 5) & 0x7F; // bits[11:5] for finer decode

    // PUSH/POP checked via bits[11:9]
    if op119 == 0b010 {
        return Thumb16InstId::PUSH;
    }
    if op119 == 0b110 {
        return Thumb16InstId::POP;
    }

    // CBZ/CBNZ: bits[11:10,8] patterns
    // CBZ:  1011 00i1 xxxx xxxx (bits[11:10]=00, bit8=1)
    // CBNZ: 1011 10i1 xxxx xxxx (bits[11:10]=10, bit8=1)
    let bit10 = (instr >> 10) & 1;
    let bit8 = (instr >> 8) & 1;
    if bit8 == 1 && bit10 == 0 {
        let bit11 = (instr >> 11) & 1;
        if bit11 == 0 || bit11 == 1 {
            // Check bit[11:10] = 00 or 10
            if (instr >> 10) & 1 == 0 {
                return Thumb16InstId::CbzCbnz;
            }
        }
    }

    match op118 {
        0b0000 => Thumb16InstId::AddSpT2,
        0b0001 => Thumb16InstId::SubSp,
        // Signed/unsigned extend: bits[11:6]
        _ => {
            let op116 = (instr >> 6) & 0x3F; // bits[11:6]
            match op116 {
                0b001000 => Thumb16InstId::SXTH,
                0b001001 => Thumb16InstId::SXTB,
                0b001010 => Thumb16InstId::UXTH,
                0b001011 => Thumb16InstId::UXTB,
                0b101000 => Thumb16InstId::REV,
                0b101001 => Thumb16InstId::REV16,
                0b101011 => Thumb16InstId::REVSH,
                _ if op118 == 0b1110 => Thumb16InstId::BKPT,
                _ if op118 == 0b1111 => decode_thumb16_hint(instr),
                _ if op == 0b1100101 => Thumb16InstId::SETEND,
                _ if (op >> 1) == 0b110011 => Thumb16InstId::CPS,
                _ => Thumb16InstId::Unknown,
            }
        }
    }
}

fn decode_thumb16_hint(instr: u16) -> Thumb16InstId {
    let op = instr & 0xFF;
    match op {
        0b0000_0000 => Thumb16InstId::NOP,
        0b0001_0000 => Thumb16InstId::YIELD,
        0b0010_0000 => Thumb16InstId::WFE,
        0b0011_0000 => Thumb16InstId::WFI,
        0b0100_0000 => Thumb16InstId::SEV,
        0b0101_0000 => Thumb16InstId::SEVL,
        _ if (instr >> 8) & 0xFF == 0xBF && (instr & 0xF) != 0 => Thumb16InstId::IT,
        _ => Thumb16InstId::NOP,
    }
}

fn decode_thumb16_cond_branch(instr: u16) -> Thumb16InstId {
    let cond = (instr >> 8) & 0xF;
    match cond {
        0b1110 => Thumb16InstId::UDF,
        0b1111 => Thumb16InstId::SVC,
        _ => Thumb16InstId::BT1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_thumb16_mov_imm() {
        // MOV R0, #42
        let instr = 0x202A; // 00100 000 00101010
        let dec = decode_thumb16(instr);
        assert_eq!(dec.id, Thumb16InstId::MovImm);
        assert_eq!(dec.rt_hi(), Reg::R0);
        assert_eq!(dec.imm8(), 42);
    }

    #[test]
    fn test_decode_thumb16_sevl() {
        assert_eq!(decode_thumb16(0xBF50).id, Thumb16InstId::SEVL);
    }

    #[test]
    fn test_decode_thumb16_add_reg() {
        // ADD R0, R1, R2
        let instr = 0x1888; // 0001100 010 001 000
        let dec = decode_thumb16(instr);
        assert_eq!(dec.id, Thumb16InstId::AddRegT1);
    }

    #[test]
    fn test_decode_thumb16_bx_lr() {
        // BX LR
        let instr = 0x4770; // 010001110 1110 000
        let dec = decode_thumb16(instr);
        assert_eq!(dec.id, Thumb16InstId::BX);
    }

    #[test]
    fn test_decode_thumb16_push() {
        // PUSH {R4, LR}
        let instr = 0xB510; // 1011 0101 00010000
        let dec = decode_thumb16(instr);
        assert_eq!(dec.id, Thumb16InstId::PUSH);
    }

    #[test]
    fn test_decode_thumb16_ldr_literal() {
        // LDR R0, [PC, #0]
        let instr = 0x4800; // 01001 000 00000000
        let dec = decode_thumb16(instr);
        assert_eq!(dec.id, Thumb16InstId::LdrLiteral);
    }
}
