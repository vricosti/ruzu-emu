use crate::frontend::a32::types::{Reg, ShiftType};
use crate::ir::cond::Cond;

/// Decoded Thumb32 instruction (two 16-bit halfwords).
#[derive(Debug, Clone, Copy)]
pub struct DecodedThumb32 {
    pub raw: u32,
    pub id: Thumb32InstId,
}

/// Thumb32 instruction identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Thumb32InstId {
    // Data processing (modified immediate)
    AND_imm,
    TST_imm,
    BIC_imm,
    ORR_imm,
    MOV_imm,
    ORN_imm,
    MVN_imm,
    EOR_imm,
    TEQ_imm,
    ADD_imm,
    CMN_imm,
    ADC_imm,
    SBC_imm,
    SUB_imm,
    CMP_imm,
    RSB_imm,

    // Data processing (plain binary immediate)
    ADD_imm_wide,
    ADR_sub,
    MOV_imm_wide,
    ADR_add,
    MOVT,
    SUB_imm_wide,
    SSAT,
    SSAT16,
    SBFX,
    BFC,
    BFI,
    USAT,
    USAT16,
    UBFX,

    // Data processing (shifted register)
    AND_reg,
    TST_reg,
    BIC_reg,
    ORR_reg,
    MOV_reg,
    ORN_reg,
    MVN_reg,
    EOR_reg,
    TEQ_reg,
    PKH,
    ADD_reg,
    CMN_reg,
    ADC_reg,
    SBC_reg,
    SUB_reg,
    CMP_reg,
    RSB_reg,

    // Branch
    B_t3,
    B_t4,
    BL,
    BLX_imm,

    // Load/Store single
    LDR_imm_t3,
    LDR_imm_t4,
    LDR_lit,
    LDR_reg,
    LDRB_imm_t2,
    LDRB_imm_t3,
    LDRB_lit,
    LDRB_reg,
    LDRH_imm_t2,
    LDRH_imm_t3,
    LDRH_lit,
    LDRH_reg,
    LDRSB_imm_t1,
    LDRSB_imm_t2,
    LDRSB_lit,
    LDRSB_reg,
    LDRSH_imm_t1,
    LDRSH_imm_t2,
    LDRSH_lit,
    LDRSH_reg,
    STR_imm_t3,
    STR_imm_t4,
    STR_reg,
    STRB_imm_t2,
    STRB_imm_t3,
    STRB_reg,
    STRH_imm_t2,
    STRH_imm_t3,
    STRH_reg,

    // Load/Store dual/exclusive
    LDRD_imm,
    LDRD_lit,
    STRD_imm,
    LDREX,
    LDREXB,
    LDREXH,
    LDREXD,
    STREX,
    STREXB,
    STREXH,
    STREXD,

    // Load/Store multiple
    LDM,
    LDMDB,
    STM,
    STMDB,
    PUSH,
    POP,

    // Multiply
    MUL,
    MLA,
    MLS,
    SMMUL,
    SMMLA,
    SMMLS,
    SMULL,
    UMULL,
    SMLAL,
    UMLAL,
    SDIV,
    UDIV,

    // Coprocessor
    MCRR,
    MRRC,
    STC,
    LDC,
    CDP,
    MCR,
    MRC,

    // Misc
    CLZ,
    RBIT,
    REV,
    REV16,
    REVSH,
    SXTH,
    SXTB,
    UXTH,
    UXTB,
    SXTAH,
    SXTAB,
    UXTAH,
    UXTAB,

    // Barriers
    DMB,
    DSB,
    ISB,
    CLREX,

    // System
    MRS,
    MSR_reg,
    SVC,
    UDF,
    BKPT,
    NOP,
    SEV,
    SEVL,
    WFE,
    WFI,
    YIELD,

    // Hints / IT
    PLD_lit,
    PLD_reg,
    PLD_imm8,
    PLD_imm12,
    PLI_lit,
    PLI_reg,
    PLI_imm8,
    PLI_imm12,

    Unknown,
}

impl DecodedThumb32 {
    /// First halfword (upper 16 bits).
    fn hw1(&self) -> u16 {
        (self.raw >> 16) as u16
    }
    /// Second halfword (lower 16 bits).
    fn hw2(&self) -> u16 {
        self.raw as u16
    }

    /// Extract Rd (bits [11:8] of hw2).
    pub fn rd(&self) -> Reg {
        Reg::from_u8(((self.hw2() >> 8) & 0xF) as u8)
    }
    /// Extract Rn (bits [3:0] of hw1).
    pub fn rn(&self) -> Reg {
        Reg::from_u8((self.hw1() & 0xF) as u8)
    }
    /// Extract Rm (bits [3:0] of hw2).
    pub fn rm(&self) -> Reg {
        Reg::from_u8((self.hw2() & 0xF) as u8)
    }
    /// Extract Rt (bits [15:12] of hw2).
    pub fn rt(&self) -> Reg {
        Reg::from_u8(((self.hw2() >> 12) & 0xF) as u8)
    }
    /// Extract Rt2 (bits [11:8] of hw2).
    pub fn rt2(&self) -> Reg {
        Reg::from_u8(((self.hw2() >> 8) & 0xF) as u8)
    }
    /// Extract Ra (bits [15:12] of hw2).
    pub fn ra(&self) -> Reg {
        Reg::from_u8(((self.hw2() >> 12) & 0xF) as u8)
    }
    /// Extract Rd_hi for long multiply (bits [11:8] of hw2).
    pub fn rd_hi(&self) -> Reg {
        self.rd()
    }
    /// Extract Rd_lo for long multiply (bits [15:12] of hw2).
    pub fn rd_lo(&self) -> Reg {
        self.rt()
    }

    /// Extract S flag (bit 4 of hw1).
    pub fn s_flag(&self) -> bool {
        self.hw1() & (1 << 4) != 0
    }

    /// 12-bit Thumb modified immediate: i:imm3:imm8.
    pub fn thumb_expand_imm_bits(&self) -> u32 {
        let i = ((self.hw1() >> 10) & 1) as u32;
        let imm3 = ((self.hw2() >> 12) & 7) as u32;
        let imm8 = (self.hw2() & 0xFF) as u32;
        (i << 11) | (imm3 << 8) | imm8
    }

    /// Expand Thumb modified immediate to 32-bit value.
    pub fn thumb_expand_imm(&self) -> u32 {
        thumb_expand_imm(self.thumb_expand_imm_bits())
    }

    /// Expand Thumb modified immediate with carry output.
    pub fn thumb_expand_imm_c(&self, carry_in: bool) -> (u32, bool) {
        thumb_expand_imm_c(self.thumb_expand_imm_bits(), carry_in)
    }

    /// 12-bit unsigned immediate for plain binary: i:imm3:imm8.
    pub fn imm12(&self) -> u32 {
        self.thumb_expand_imm_bits()
    }

    /// 16-bit immediate for MOVW/MOVT: imm4:i:imm3:imm8.
    pub fn imm16(&self) -> u32 {
        let imm4 = (self.hw1() & 0xF) as u32;
        let i = ((self.hw1() >> 10) & 1) as u32;
        let imm3 = ((self.hw2() >> 12) & 7) as u32;
        let imm8 = (self.hw2() & 0xFF) as u32;
        (imm4 << 12) | (i << 11) | (imm3 << 8) | imm8
    }

    /// Shift type and amount for shifted register (imm3:imm2 of hw2, type bits [5:4]).
    pub fn shift_type_amount(&self) -> (ShiftType, u32) {
        let type_bits = ((self.hw2() >> 4) & 3) as u8;
        let imm3 = ((self.hw2() >> 12) & 7) as u32;
        let imm2 = ((self.hw2() >> 6) & 3) as u32;
        let imm5 = (imm3 << 2) | imm2;
        (ShiftType::from_u8(type_bits), imm5)
    }

    /// 8-bit immediate (bits [7:0] of hw2).
    pub fn imm8(&self) -> u32 {
        (self.hw2() & 0xFF) as u32
    }

    /// P flag (bit 8 of hw2) for load/store.
    pub fn p_flag(&self) -> bool {
        self.hw2() & (1 << 10) != 0
    }
    /// U flag (bit 9 of hw2) for load/store.
    pub fn u_flag(&self) -> bool {
        self.hw2() & (1 << 9) != 0
    }
    /// W flag (bit 8 of hw2) for load/store.
    pub fn w_flag(&self) -> bool {
        self.hw2() & (1 << 8) != 0
    }

    /// Register list (bits [15:0] of hw2).
    pub fn register_list(&self) -> u16 {
        self.hw2()
    }

    /// Branch offset for B.W (T4 encoding).
    pub fn branch_offset_t4(&self) -> i32 {
        let s = ((self.hw1() >> 10) & 1) as u32;
        let imm10 = (self.hw1() & 0x3FF) as u32;
        let j1 = ((self.hw2() >> 13) & 1) as u32;
        let j2 = ((self.hw2() >> 11) & 1) as u32;
        let imm11 = (self.hw2() & 0x7FF) as u32;
        let i1 = !(j1 ^ s) & 1;
        let i2 = !(j2 ^ s) & 1;
        let imm25 = (s << 24) | (i1 << 23) | (i2 << 22) | (imm10 << 12) | (imm11 << 1);
        // Sign-extend from 25 bits
        ((imm25 as i32) << 7) >> 7
    }

    /// Branch offset for B.W (T3 encoding, conditional).
    pub fn branch_offset_t3(&self) -> i32 {
        let s = ((self.hw1() >> 10) & 1) as u32;
        let imm6 = (self.hw1() & 0x3F) as u32;
        let j1 = ((self.hw2() >> 13) & 1) as u32;
        let j2 = ((self.hw2() >> 11) & 1) as u32;
        let imm11 = (self.hw2() & 0x7FF) as u32;
        let imm21 = (s << 20) | (j2 << 19) | (j1 << 18) | (imm6 << 12) | (imm11 << 1);
        // Sign-extend from 21 bits
        ((imm21 as i32) << 11) >> 11
    }

    /// Condition code for conditional branch T3 encoding.
    pub fn cond(&self) -> Cond {
        let c = ((self.hw1() >> 6) & 0xF) as u8;
        Cond::from_u8(c)
    }

    /// BFI/BFC lsb and width.
    pub fn bfc_lsb_msb(&self) -> (u32, u32) {
        let imm3 = ((self.hw2() >> 12) & 7) as u32;
        let imm2 = ((self.hw2() >> 6) & 3) as u32;
        let lsb = (imm3 << 2) | imm2;
        let msb = (self.hw2() & 0x1F) as u32;
        (lsb, msb)
    }

    /// SBFX/UBFX lsb and width.
    pub fn bfx_lsb_width(&self) -> (u32, u32) {
        let imm3 = ((self.hw2() >> 12) & 7) as u32;
        let imm2 = ((self.hw2() >> 6) & 3) as u32;
        let lsb = (imm3 << 2) | imm2;
        let widthm1 = (self.hw2() & 0x1F) as u32;
        (lsb, widthm1 + 1)
    }

    // --- Coprocessor instruction fields ---

    pub fn coproc_two(&self) -> bool {
        self.raw & (1 << 28) != 0
    }

    pub fn coproc_no(&self) -> u32 {
        (self.raw >> 8) & 0xF
    }

    pub fn coproc_opc1(&self) -> u32 {
        (self.raw >> 21) & 0x7
    }

    pub fn coproc_dp_opc1(&self) -> u32 {
        (self.raw >> 20) & 0xF
    }

    pub fn coproc_opc2(&self) -> u32 {
        (self.raw >> 5) & 0x7
    }

    pub fn coproc_crn(&self) -> u32 {
        (self.raw >> 16) & 0xF
    }

    pub fn coproc_crd(&self) -> u32 {
        (self.raw >> 12) & 0xF
    }

    pub fn coproc_crm(&self) -> u32 {
        self.raw & 0xF
    }

    pub fn coproc_transfer_opc(&self) -> u32 {
        (self.raw >> 4) & 0xF
    }
}

/// Expand Thumb modified immediate (12-bit) to 32-bit value.
pub fn thumb_expand_imm(imm12: u32) -> u32 {
    let top2 = (imm12 >> 10) & 3;
    if top2 == 0 {
        let op = (imm12 >> 8) & 3;
        let imm8 = imm12 & 0xFF;
        match op {
            0 => imm8,
            1 => (imm8 << 16) | imm8,
            2 => (imm8 << 24) | (imm8 << 8),
            3 => (imm8 << 24) | (imm8 << 16) | (imm8 << 8) | imm8,
            _ => unreachable!(),
        }
    } else {
        let unrotated = 0x80 | (imm12 & 0x7F);
        let rotation = (imm12 >> 7) & 0x1F;
        unrotated.rotate_right(rotation)
    }
}

/// Expand Thumb modified immediate with carry output.
pub fn thumb_expand_imm_c(imm12: u32, carry_in: bool) -> (u32, bool) {
    let top2 = (imm12 >> 10) & 3;
    if top2 == 0 {
        (thumb_expand_imm(imm12), carry_in)
    } else {
        let unrotated = 0x80 | (imm12 & 0x7F);
        let rotation = (imm12 >> 7) & 0x1F;
        let result = unrotated.rotate_right(rotation);
        let carry = result & (1 << 31) != 0;
        (result, carry)
    }
}

/// Decode a 32-bit Thumb instruction from two halfwords.
pub fn decode_thumb32(hw1: u16, hw2: u16) -> DecodedThumb32 {
    let raw = ((hw1 as u32) << 16) | (hw2 as u32);
    // Exact upstream thumb32.inc hint/preload encodings. These precede the
    // broader load-byte and branch groups in the generated decoder.
    let id = if raw == 0xF3AF_8005 {
        Thumb32InstId::SEVL
    } else if matches_thumb32(raw, 0xFF7F_F000, 0xF81F_F000)
        || matches_thumb32(raw, 0xFF7F_F000, 0xF83F_F000)
    {
        Thumb32InstId::PLD_lit
    } else if matches_thumb32(raw, 0xFFD0_FFC0, 0xF810_F000) {
        Thumb32InstId::PLD_reg
    } else if matches_thumb32(raw, 0xFFD0_FF00, 0xF810_FC00) {
        Thumb32InstId::PLD_imm8
    } else if matches_thumb32(raw, 0xFFD0_F000, 0xF890_F000) {
        Thumb32InstId::PLD_imm12
    } else if matches_thumb32(raw, 0xFF7F_F000, 0xF91F_F000) {
        Thumb32InstId::PLI_lit
    } else if matches_thumb32(raw, 0xFFF0_FFC0, 0xF910_F000) {
        Thumb32InstId::PLI_reg
    } else if matches_thumb32(raw, 0xFFF0_FF00, 0xF910_FC00) {
        Thumb32InstId::PLI_imm8
    } else if matches_thumb32(raw, 0xFFF0_F000, 0xF990_F000) {
        Thumb32InstId::PLI_imm12
    } else if matches_thumb32(raw, 0xEFF0_0000, 0xEC40_0000) {
        Thumb32InstId::MCRR
    } else if matches_thumb32(raw, 0xEFF0_0000, 0xEC50_0000) {
        Thumb32InstId::MRRC
    } else if matches_thumb32(raw, 0xEF10_0010, 0xEE00_0010) {
        Thumb32InstId::MCR
    } else if matches_thumb32(raw, 0xEF10_0010, 0xEE10_0010) {
        Thumb32InstId::MRC
    } else if matches_thumb32(raw, 0xEF00_0010, 0xEE00_0000) {
        Thumb32InstId::CDP
    } else if matches_thumb32(raw, 0xEE10_0000, 0xEC10_0000) {
        Thumb32InstId::LDC
    } else if matches_thumb32(raw, 0xEE10_0000, 0xEC00_0000) {
        Thumb32InstId::STC
    } else if matches_thumb32(raw, 0xFFF0_F0E0, 0xFB50_F000) {
        Thumb32InstId::SMMUL
    } else if matches_thumb32(raw, 0xFFF0_00E0, 0xFB50_0000) {
        Thumb32InstId::SMMLA
    } else if matches_thumb32(raw, 0xFFF0_00E0, 0xFB60_0000) {
        Thumb32InstId::SMMLS
    } else {
        let op1 = (hw1 >> 11) & 3;
        let op2 = ((hw1 >> 4) & 0x7F) as u32;
        let op = ((hw2 >> 15) & 1) as u32;

        match op1 {
            0b01 => decode_thumb32_01(raw, op2, op),
            0b10 => decode_thumb32_10(raw, op2, op),
            0b11 => decode_thumb32_11(raw, op2),
            _ => Thumb32InstId::Unknown,
        }
    };

    DecodedThumb32 { raw, id }
}

fn decode_thumb32_01(raw: u32, _op2: u32, _op: u32) -> Thumb32InstId {
    let top_byte = (raw >> 24) & 0xFF;
    let op_nibble = (raw >> 20) & 0xF;

    match top_byte {
        0xEA | 0xEB => decode_thumb32_dp_shifted_reg(raw),
        0xE8 => match op_nibble {
            0x8..=0xB => decode_thumb32_ls_multiple(raw),
            _ => decode_thumb32_ls_dual_excl(raw),
        },
        0xE9 => match op_nibble {
            0x0..=0x3 => decode_thumb32_ls_multiple(raw),
            _ => decode_thumb32_ls_dual_excl(raw),
        },
        _ => Thumb32InstId::Unknown,
    }
}

fn decode_thumb32_10(raw: u32, op2: u32, op: u32) -> Thumb32InstId {
    if op == 0 {
        if op2 & 0x20 == 0 {
            // Data processing (modified immediate)
            decode_thumb32_dp_mod_imm(raw)
        } else {
            // Data processing (plain binary immediate)
            decode_thumb32_dp_plain_imm(raw)
        }
    } else {
        // Branch & misc
        decode_thumb32_branch(raw)
    }
}

fn decode_thumb32_11(raw: u32, op2: u32) -> Thumb32InstId {
    match op2 >> 3 {
        // Load/Store single
        0b0000..=0b0011 => decode_thumb32_ls_single(raw),
        // Load byte / Load halfword
        0b0100..=0b0111 => decode_thumb32_ls_single(raw),
        // Multiply / long multiply / divide
        0b1000..=0b1001 => decode_thumb32_multiply(raw),
        0b1010..=0b1011 => decode_thumb32_long_multiply(raw),
        _ => Thumb32InstId::Unknown,
    }
}

fn decode_thumb32_ls_multiple(raw: u32) -> Thumb32InstId {
    let op = (raw >> 23) & 3;
    let l = (raw >> 20) & 1;
    let w = (raw >> 21) & 1;
    let rn = (raw >> 16) & 0xF;

    match (l, op) {
        (0, 0b01) => Thumb32InstId::STM,
        (0, 0b10) => {
            if rn == 13 && w == 1 {
                Thumb32InstId::PUSH
            } else {
                Thumb32InstId::STMDB
            }
        }
        (1, 0b01) => {
            if rn == 13 && w == 1 {
                Thumb32InstId::POP
            } else {
                Thumb32InstId::LDM
            }
        }
        (1, 0b10) => Thumb32InstId::LDMDB,
        _ => Thumb32InstId::Unknown,
    }
}

fn decode_thumb32_ls_dual_excl(raw: u32) -> Thumb32InstId {
    let op1 = (raw >> 23) & 3;
    let op2 = (raw >> 20) & 3;
    let op3 = (raw >> 4) & 0xF;

    match (op1, op2) {
        (0b00, 0b00) => Thumb32InstId::STREX,
        (0b00, 0b01) => Thumb32InstId::LDREX,
        (0b00, 0b10) | (0b01, 0b10) => Thumb32InstId::STRD_imm,
        (0b00, 0b11) | (0b01, 0b11) => {
            let rn = (raw >> 16) & 0xF;
            if rn == 15 {
                Thumb32InstId::LDRD_lit
            } else {
                Thumb32InstId::LDRD_imm
            }
        }
        (0b01, 0b00) => match op3 {
            0b0100 => Thumb32InstId::STREXB,
            0b0101 => Thumb32InstId::STREXH,
            0b0111 => Thumb32InstId::STREXD,
            _ => Thumb32InstId::Unknown,
        },
        (0b01, 0b01) => match op3 {
            0b0000 => Thumb32InstId::LDREXB,
            0b0001 => Thumb32InstId::LDREXH,
            0b0011 => Thumb32InstId::LDREXD,
            _ => Thumb32InstId::Unknown,
        },
        (0b10, _) | (0b11, _) => {
            if op2 & 1 == 0 {
                Thumb32InstId::STRD_imm
            } else {
                let rn = (raw >> 16) & 0xF;
                if rn == 15 {
                    Thumb32InstId::LDRD_lit
                } else {
                    Thumb32InstId::LDRD_imm
                }
            }
        }
        _ => Thumb32InstId::Unknown,
    }
}

fn decode_thumb32_dp_shifted_reg(raw: u32) -> Thumb32InstId {
    let op = (raw >> 21) & 0xF;
    let rn = (raw >> 16) & 0xF;
    let rd = (raw >> 8) & 0xF;
    let s = (raw >> 20) & 1;

    match op {
        0b0000 if rd == 15 && s == 1 => Thumb32InstId::TST_reg,
        0b0000 => Thumb32InstId::AND_reg,
        0b0001 => Thumb32InstId::BIC_reg,
        0b0010 if rn == 15 => Thumb32InstId::MOV_reg,
        0b0010 => Thumb32InstId::ORR_reg,
        0b0011 if rn == 15 => Thumb32InstId::MVN_reg,
        0b0011 => Thumb32InstId::ORN_reg,
        0b0100 if rd == 15 && s == 1 => Thumb32InstId::TEQ_reg,
        0b0100 => Thumb32InstId::EOR_reg,
        0b0110 => Thumb32InstId::PKH,
        0b1000 if rd == 15 && s == 1 => Thumb32InstId::CMN_reg,
        0b1000 => Thumb32InstId::ADD_reg,
        0b1010 => Thumb32InstId::ADC_reg,
        0b1011 => Thumb32InstId::SBC_reg,
        0b1101 if rd == 15 && s == 1 => Thumb32InstId::CMP_reg,
        0b1101 => Thumb32InstId::SUB_reg,
        0b1110 => Thumb32InstId::RSB_reg,
        _ => Thumb32InstId::Unknown,
    }
}

fn decode_thumb32_dp_mod_imm(raw: u32) -> Thumb32InstId {
    let op = (raw >> 21) & 0xF;
    let rn = (raw >> 16) & 0xF;
    let rd = (raw >> 8) & 0xF;
    let s = (raw >> 20) & 1;

    match op {
        0b0000 if rd == 15 && s == 1 => Thumb32InstId::TST_imm,
        0b0000 => Thumb32InstId::AND_imm,
        0b0001 => Thumb32InstId::BIC_imm,
        0b0010 if rn == 15 => Thumb32InstId::MOV_imm,
        0b0010 => Thumb32InstId::ORR_imm,
        0b0011 if rn == 15 => Thumb32InstId::MVN_imm,
        0b0011 => Thumb32InstId::ORN_imm,
        0b0100 if rd == 15 && s == 1 => Thumb32InstId::TEQ_imm,
        0b0100 => Thumb32InstId::EOR_imm,
        0b1000 if rd == 15 && s == 1 => Thumb32InstId::CMN_imm,
        0b1000 => Thumb32InstId::ADD_imm,
        0b1010 => Thumb32InstId::ADC_imm,
        0b1011 => Thumb32InstId::SBC_imm,
        0b1101 if rd == 15 && s == 1 => Thumb32InstId::CMP_imm,
        0b1101 => Thumb32InstId::SUB_imm,
        0b1110 => Thumb32InstId::RSB_imm,
        _ => Thumb32InstId::Unknown,
    }
}

fn decode_thumb32_dp_plain_imm(raw: u32) -> Thumb32InstId {
    // Upstream thumb32.inc lists this "Invalid decoding" UDF entry immediately
    // before SSAT/USAT. The Thumb32 matcher is first-match in source order
    // (no specificity sort), so this reserved encoding (imm3=0, bits[7:4]=0b0001)
    // wins over SSAT/USAT and raises UndefinedInstruction.
    //   11110011-010----0000----0001----
    if (raw & 0xFF70_F0F0) == 0xF320_0010 {
        return Thumb32InstId::UDF;
    }

    let op = (raw >> 20) & 0x1F;
    let rn = (raw >> 16) & 0xF;

    match op {
        0b00000 if rn == 15 => Thumb32InstId::ADR_add,
        0b00000 => Thumb32InstId::ADD_imm_wide,
        0b00100 => Thumb32InstId::MOV_imm_wide,
        0b01010 if rn == 15 => Thumb32InstId::ADR_sub,
        0b01010 => Thumb32InstId::SUB_imm_wide,
        0b01100 => Thumb32InstId::MOVT,
        0b10000 | 0b10010 => Thumb32InstId::SSAT,
        0b10100 => Thumb32InstId::SBFX,
        0b10110 if rn == 15 => Thumb32InstId::BFC,
        0b10110 => Thumb32InstId::BFI,
        0b11000 | 0b11010 => Thumb32InstId::USAT,
        0b11100 => Thumb32InstId::UBFX,
        _ => Thumb32InstId::Unknown,
    }
}

fn decode_thumb32_branch(raw: u32) -> Thumb32InstId {
    let op1 = (raw >> 20) & 0x7F;
    let op2 = (raw >> 12) & 7;

    match (op1 >> 3, op2) {
        // MSR
        (0b0001 | 0b0011, _) if op1 & 5 == 0 => Thumb32InstId::MSR_reg,
        // Misc hints
        (0b0001 | 0b0011, _) if op1 & 5 == 1 => decode_thumb32_hints(raw),
        // MRS
        (0b0001 | 0b0011, _) if op1 & 5 == 5 => Thumb32InstId::MRS,
        // B (T3 conditional)
        (_, 0b000 | 0b010) if op1 & 0x38 != 0x38 => Thumb32InstId::B_t3,
        // UDF
        (_, 0b000 | 0b010) if op1 == 0x7F => Thumb32InstId::UDF,
        // SVC
        (_, 0b000 | 0b010) if op1 == 0x7E => Thumb32InstId::SVC,
        // B.W (T4 unconditional)
        (_, 0b001 | 0b011) if op1 & 0x40 == 0 => Thumb32InstId::B_t4,
        // BL
        (_, 0b101 | 0b111) if op1 & 0x40 == 0 => Thumb32InstId::BL,
        // BLX
        (_, 0b100 | 0b110) if op1 & 0x40 == 0 => Thumb32InstId::BLX_imm,
        _ => Thumb32InstId::Unknown,
    }
}

fn decode_thumb32_hints(raw: u32) -> Thumb32InstId {
    let op = (raw >> 4) & 0xF;
    let hint = raw & 0xFF;
    match hint {
        0 => Thumb32InstId::NOP,
        1 => Thumb32InstId::YIELD,
        2 => Thumb32InstId::WFE,
        3 => Thumb32InstId::WFI,
        4 => Thumb32InstId::SEV,
        5 => Thumb32InstId::SEVL,
        _ if op == 4 => Thumb32InstId::DSB,
        _ if op == 5 => Thumb32InstId::DMB,
        _ if op == 6 => Thumb32InstId::ISB,
        _ => Thumb32InstId::NOP,
    }
}

fn decode_thumb32_ls_single(raw: u32) -> Thumb32InstId {
    let op1 = (raw >> 23) & 3;
    let rn = (raw >> 16) & 0xF;
    let load = (raw >> 20) & 1 != 0;
    let half = (raw >> 21) & 1 != 0;
    let word = (raw >> 22) & 1 != 0;
    let indexed_imm8 = raw & (1 << 11) != 0;

    match op1 {
        0b00 => {
            if load {
                if word {
                    if indexed_imm8 {
                        Thumb32InstId::LDR_imm_t4
                    } else {
                        Thumb32InstId::LDR_reg
                    }
                } else if half {
                    if indexed_imm8 {
                        Thumb32InstId::LDRH_imm_t3
                    } else {
                        Thumb32InstId::LDRH_reg
                    }
                } else {
                    if rn == 15 {
                        return Thumb32InstId::LDRB_lit;
                    }
                    if indexed_imm8 {
                        Thumb32InstId::LDRB_imm_t3
                    } else {
                        Thumb32InstId::LDRB_reg
                    }
                }
            } else {
                if word {
                    if indexed_imm8 {
                        Thumb32InstId::STR_imm_t4
                    } else {
                        Thumb32InstId::STR_reg
                    }
                } else if half {
                    if indexed_imm8 {
                        Thumb32InstId::STRH_imm_t3
                    } else {
                        Thumb32InstId::STRH_reg
                    }
                } else {
                    if indexed_imm8 {
                        Thumb32InstId::STRB_imm_t3
                    } else {
                        Thumb32InstId::STRB_reg
                    }
                }
            }
        }
        0b01 => {
            if load {
                if word {
                    if rn == 15 {
                        return Thumb32InstId::LDR_lit;
                    }
                    Thumb32InstId::LDR_imm_t3
                } else if half {
                    if rn == 15 {
                        return Thumb32InstId::LDRH_lit;
                    }
                    Thumb32InstId::LDRH_imm_t2
                } else {
                    if rn == 15 {
                        return Thumb32InstId::LDRB_lit;
                    }
                    Thumb32InstId::LDRB_imm_t2
                }
            } else {
                if word {
                    Thumb32InstId::STR_imm_t3
                } else if half {
                    Thumb32InstId::STRH_imm_t2
                } else {
                    Thumb32InstId::STRB_imm_t2
                }
            }
        }
        0b10 => {
            if load {
                if half {
                    if indexed_imm8 {
                        Thumb32InstId::LDRSH_imm_t2
                    } else {
                        Thumb32InstId::LDRSH_reg
                    }
                } else if indexed_imm8 {
                    Thumb32InstId::LDRSB_imm_t2
                } else {
                    Thumb32InstId::LDRSB_reg
                }
            } else {
                Thumb32InstId::Unknown
            }
        }
        0b11 => {
            if load {
                if half {
                    if rn == 15 {
                        Thumb32InstId::LDRSH_lit
                    } else {
                        Thumb32InstId::LDRSH_imm_t1
                    }
                } else if rn == 15 {
                    Thumb32InstId::LDRSB_lit
                } else {
                    Thumb32InstId::LDRSB_imm_t1
                }
            } else {
                Thumb32InstId::Unknown
            }
        }
        _ => Thumb32InstId::Unknown,
    }
}

fn decode_thumb32_multiply(raw: u32) -> Thumb32InstId {
    let op1 = (raw >> 20) & 7;
    let op2 = (raw >> 4) & 3;
    let ra = (raw >> 12) & 0xF;

    match op1 {
        0b000 if ra == 15 => Thumb32InstId::MUL,
        0b000 => Thumb32InstId::MLA,
        0b001 => Thumb32InstId::MLS,
        0b101 if ra == 15 && op2 == 0 => Thumb32InstId::SMMUL,
        0b101 if op2 == 0 => Thumb32InstId::SMMLA,
        0b110 if op2 == 0 => Thumb32InstId::SMMLS,
        0b010 if op2 == 0 => {
            let rn = (raw >> 16) & 0xF;
            if rn == 15 {
                Thumb32InstId::Unknown
            } else {
                Thumb32InstId::CLZ
            }
        }
        _ => {
            // Extensions / misc
            let op = (raw >> 20) & 7;
            let sub = (raw >> 4) & 3;
            let rn = (raw >> 16) & 0xF;
            match (op, sub) {
                (0b100, 0b00) if rn == 15 => Thumb32InstId::SXTB,
                (0b100, 0b00) => Thumb32InstId::SXTAB,
                (0b000, 0b10) if rn == 15 => Thumb32InstId::SXTH,
                (0b000, 0b10) => Thumb32InstId::SXTAH,
                (0b100, 0b10) if rn == 15 => Thumb32InstId::UXTB,
                (0b100, 0b10) => Thumb32InstId::UXTAB,
                (0b000, 0b11) if rn == 15 => Thumb32InstId::UXTH,
                (0b000, 0b11) => Thumb32InstId::UXTAH,
                (0b001, 0b00) => Thumb32InstId::REV,
                (0b001, 0b01) => Thumb32InstId::REV16,
                (0b001, 0b10) => Thumb32InstId::RBIT,
                (0b001, 0b11) => Thumb32InstId::REVSH,
                _ => Thumb32InstId::Unknown,
            }
        }
    }
}

fn decode_thumb32_long_multiply(raw: u32) -> Thumb32InstId {
    let op1 = (raw >> 20) & 7;
    let op2 = (raw >> 4) & 0xF;

    match op1 {
        0b000 => Thumb32InstId::SMULL,
        0b001 if op2 == 0xF => Thumb32InstId::SDIV,
        0b010 => Thumb32InstId::UMULL,
        0b011 if op2 == 0xF => Thumb32InstId::UDIV,
        0b100 => Thumb32InstId::SMLAL,
        0b110 => Thumb32InstId::UMLAL,
        _ => Thumb32InstId::Unknown,
    }
}

fn matches_thumb32(raw: u32, mask: u32, expected: u32) -> bool {
    raw & mask == expected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_thumb32_ssat_reserved_maps_to_udf() {
        // `11110011-010----0000----0001----` is upstream's "Invalid decoding"
        // UDF entry, listed before SSAT/USAT (Thumb32 = first-match in order).
        assert_eq!(decode_thumb32(0xF320, 0x0010).id, Thumb32InstId::UDF);
        // Variable fields (Rn, Rd, sat_imm, and bit23 = the SSAT/USAT selector)
        // do not change the classification:
        assert_eq!(decode_thumb32(0xF32F, 0x0F1F).id, Thumb32InstId::UDF);
        assert_eq!(decode_thumb32(0xF3A0, 0x0010).id, Thumb32InstId::UDF); // bit23=1 (USAT space)
                                                                           // A real SSAT (imm3/shift and bits[7:4] != 0b0001) still decodes as SSAT:
        assert_eq!(decode_thumb32(0xF301, 0x0207).id, Thumb32InstId::SSAT);
    }

    #[test]
    fn test_thumb_expand_imm() {
        // Constant 0x42
        assert_eq!(thumb_expand_imm(0x042), 0x42);
        // Repeated pattern: 0x1AB => op=1 => (AB << 16) | AB = 0x00AB00AB
        assert_eq!(thumb_expand_imm(0x1AB), 0x00AB00AB);
        // Rotated: top2 != 0
        let imm12 = 0x407; // top2=01, rotation=8, value=0x87
        let result = thumb_expand_imm(imm12);
        assert_eq!(result, 0x87u32.rotate_right(8));
    }

    #[test]
    fn test_decode_thumb32_bl() {
        // BL <offset> typical encoding
        // hw1: 1111 0 S imm10
        // hw2: 1 1 J1 1 J2 imm11
        let hw1: u16 = 0xF000; // S=0, imm10=0
        let hw2: u16 = 0xD000; // J1=0, J2=1, imm11=0
        let dec = decode_thumb32(hw1, hw2);
        assert_eq!(dec.id, Thumb32InstId::BL);
    }

    #[test]
    fn test_decode_thumb32_hint_and_preload_families() {
        for (raw, expected) in [
            (0xF3AF_8005u32, Thumb32InstId::SEVL),
            (0xF81F_F123, Thumb32InstId::PLD_lit),
            (0xF83F_F123, Thumb32InstId::PLD_lit),
            (0xF815_F012, Thumb32InstId::PLD_reg),
            (0xF835_FC12, Thumb32InstId::PLD_imm8),
            (0xF895_F123, Thumb32InstId::PLD_imm12),
            (0xF91F_F123, Thumb32InstId::PLI_lit),
            (0xF915_F012, Thumb32InstId::PLI_reg),
            (0xF915_FC12, Thumb32InstId::PLI_imm8),
            (0xF995_F123, Thumb32InstId::PLI_imm12),
        ] {
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                expected,
                "raw={raw:08X}"
            );
        }
    }

    #[test]
    fn test_decode_thumb32_movw() {
        // MOVW Rd, #imm16
        // hw1: 1111 0 i 10 0100 imm4
        // hw2: 0 imm3 Rd imm8
        let hw1: u16 = 0xF240; // MOV_imm_wide
        let hw2: u16 = 0x0042; // Rd=R0, imm8=0x42
        let dec = decode_thumb32(hw1, hw2);
        assert_eq!(dec.id, Thumb32InstId::MOV_imm_wide);
        assert_eq!(dec.rd(), Reg::R0);
        assert_eq!(dec.imm16(), 0x42);
    }

    #[test]
    fn test_decode_thumb32_str_imm_t3_zero_offset_not_reg() {
        let dec = decode_thumb32(0xF8C0, 0x1000);
        assert_eq!(dec.id, Thumb32InstId::STR_imm_t3);
    }

    #[test]
    fn test_decode_thumb32_str_reg_not_imm() {
        let dec = decode_thumb32(0xF840, 0x1002);
        assert_eq!(dec.id, Thumb32InstId::STR_reg);
    }

    #[test]
    fn test_decode_thumb32_ldr_imm_t3_zero_offset_not_reg() {
        let dec = decode_thumb32(0xF8D0, 0x3000);
        assert_eq!(dec.id, Thumb32InstId::LDR_imm_t3);
    }

    #[test]
    fn test_decode_thumb32_ldrb_imm_t2_zero_offset_not_reg() {
        let dec = decode_thumb32(0xF890, 0x3000);
        assert_eq!(dec.id, Thumb32InstId::LDRB_imm_t2);
    }

    #[test]
    fn test_decode_thumb32_strb_imm_t2_zero_offset_not_reg() {
        let dec = decode_thumb32(0xF880, 0x1000);
        assert_eq!(dec.id, Thumb32InstId::STRB_imm_t2);
    }

    #[test]
    fn test_decode_thumb32_ldrh_imm_t2_zero_offset_not_reg() {
        let dec = decode_thumb32(0xF8B0, 0x3000);
        assert_eq!(dec.id, Thumb32InstId::LDRH_imm_t2);
    }

    #[test]
    fn test_decode_thumb32_strh_imm_t2_zero_offset_not_reg() {
        let dec = decode_thumb32(0xF8A0, 0x1000);
        assert_eq!(dec.id, Thumb32InstId::STRH_imm_t2);
    }

    #[test]
    fn test_decode_thumb32_ldrsb_imm_t1() {
        let dec = decode_thumb32(0xF990, 0x3000);
        assert_eq!(dec.id, Thumb32InstId::LDRSB_imm_t1);
    }

    #[test]
    fn test_decode_thumb32_ldrsh_imm_t1() {
        let dec = decode_thumb32(0xF9B0, 0x3000);
        assert_eq!(dec.id, Thumb32InstId::LDRSH_imm_t1);
    }

    #[test]
    fn test_decode_thumb32_push() {
        // PUSH.W {regs} = STMDB SP!, {regs}
        // hw1: 1110 1001 0010 1101
        // hw2: register list
        let hw1: u16 = 0xE92D;
        let hw2: u16 = 0x4010; // R4, LR
        let dec = decode_thumb32(hw1, hw2);
        assert_eq!(dec.id, Thumb32InstId::PUSH);
    }

    #[test]
    fn test_decode_thumb32_eb00_is_data_processing_not_strd() {
        let hw1: u16 = 0xEB00;
        let hw2: u16 = 0x0000;
        let dec = decode_thumb32(hw1, hw2);
        assert_eq!(dec.id, Thumb32InstId::ADD_reg);
    }

    #[test]
    fn test_decode_thumb32_e940_stays_load_store_dual() {
        let hw1: u16 = 0xE940;
        let hw2: u16 = 0x0000;
        let dec = decode_thumb32(hw1, hw2);
        assert_eq!(dec.id, Thumb32InstId::STRD_imm);
    }

    #[test]
    fn test_decode_thumb32_e92d_is_push() {
        let hw1: u16 = 0xE92D;
        let hw2: u16 = 0xB018;
        let dec = decode_thumb32(hw1, hw2);
        assert_eq!(dec.id, Thumb32InstId::PUSH);
    }

    #[test]
    fn test_decode_thumb32_ldr_imm() {
        // LDR.W Rt, [Rn, #imm12]
        // hw1: 1111 1000 1101 nnnn
        // hw2: tttt iiiiiiiiiiii
        let hw1: u16 = 0xF8D1; // Rn=R1
        let hw2: u16 = 0x0004; // Rt=R0, imm12=4
        let dec = decode_thumb32(hw1, hw2);
        // This should decode to an LDR variant
        assert!(matches!(
            dec.id,
            Thumb32InstId::LDR_imm_t3 | Thumb32InstId::LDR_reg
        ));
    }

    #[test]
    fn test_decode_thumb32_smmul() {
        let hw1: u16 = 0xFB50;
        let hw2: u16 = 0xF000;
        let dec = decode_thumb32(hw1, hw2);
        assert_eq!(dec.id, Thumb32InstId::SMMUL);
    }

    #[test]
    fn test_decode_thumb32_f8b3_800a_as_ldrh_imm_t2() {
        let hw1: u16 = 0xF8B3;
        let hw2: u16 = 0x800A;
        let dec = decode_thumb32(hw1, hw2);
        assert_eq!(dec.id, Thumb32InstId::LDRH_imm_t2);
        assert_eq!(dec.rn(), Reg::R3);
        assert_eq!(dec.rt(), Reg::R8);
        assert_eq!(dec.imm12(), 0x000A);
    }

    #[test]
    fn test_decode_thumb32_coprocessor_forms() {
        let cases: [(u32, Thumb32InstId); 7] = [
            (0xEC42_3F1E, Thumb32InstId::MCRR),
            (0xFC52_3F1E, Thumb32InstId::MRRC),
            (0xED24_7F22, Thumb32InstId::STC),
            (0xFDB4_7F22, Thumb32InstId::LDC),
            (0xEEF4_3F41, Thumb32InstId::CDP),
            (0xFE64_3F51, Thumb32InstId::MCR),
            (0xEE74_3F51, Thumb32InstId::MRC),
        ];

        for (raw, expected) in cases {
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                expected,
                "raw={raw:#010x}"
            );
        }
    }
}
