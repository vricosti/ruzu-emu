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
    ADD_imm_1,
    CMN_imm,
    ADC_imm,
    SBC_imm,
    SUB_imm_1,
    CMP_imm,
    RSB_imm,

    // Data processing (plain binary immediate)
    ADR_t3,
    ADD_imm_2,
    MOVW_imm,
    ADR_t2,
    MOVT,
    SUB_imm_2,
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
    B,
    B_cond,
    BL_imm,
    BLX_imm,

    // Load/Store single
    LDR_imm_t3,
    LDR_imm_t4,
    LDR_lit,
    LDR_reg,
    LDRT,
    LDRB_imm_t2,
    LDRB_imm_t3,
    LDRB_lit,
    LDRB_reg,
    LDRBT,
    LDRH_imm_t2,
    LDRH_imm_t3,
    LDRH_lit,
    LDRH_reg,
    LDRHT,
    LDRSB_imm_t1,
    LDRSB_imm_t2,
    LDRSB_lit,
    LDRSB_reg,
    LDRSBT,
    LDRSH_imm_t1,
    LDRSH_imm_t2,
    LDRSH_lit,
    LDRSH_reg,
    LDRSHT,
    STR_imm_1,
    STR_imm_2,
    STR_imm_3,
    STRT,
    STR_reg,
    STRB_imm_1,
    STRB_imm_2,
    STRB_imm_3,
    STRBT,
    STRB_reg,
    STRH_imm_1,
    STRH_imm_2,
    STRH_imm_3,
    STRHT,
    STRH_reg,

    // Load/Store dual/exclusive
    LDA,
    LDRD_imm_1,
    LDRD_imm_2,
    LDRD_lit_1,
    LDRD_lit_2,
    STRD_imm_1,
    STRD_imm_2,
    LDREX,
    LDREXB,
    LDREXH,
    LDREXD,
    STREX,
    STREXB,
    STREXH,
    STREXD,
    STL,
    TBB,
    TBH,

    // Load/Store multiple
    LDMIA,
    LDMDB,
    STMIA,
    STMDB,
    PUSH,
    POP,

    // Multiply
    MUL,
    MLA,
    MLS,
    SMLAD,
    SMLAXY,
    SMLAWY,
    SMLSD,
    SMMUL,
    SMMLA,
    SMMLS,
    SMUAD,
    SMUSD,
    SMULXY,
    SMULWY,
    USAD8,
    USADA8,
    SMULL,
    UMULL,
    SMLAL,
    SMLALD,
    SMLALXY,
    SMLSLD,
    UMLAL,
    UMAAL,
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
    LSL_reg,
    LSR_reg,
    ASR_reg,
    ROR_reg,
    QADD,
    QDADD,
    QSUB,
    QDSUB,
    SEL,
    SADD8,
    SADD16,
    SASX,
    SSAX,
    SSUB8,
    SSUB16,
    UADD8,
    UADD16,
    UASX,
    USAX,
    USUB8,
    USUB16,
    QADD8,
    QADD16,
    QASX,
    QSAX,
    QSUB8,
    QSUB16,
    UQADD8,
    UQADD16,
    UQASX,
    UQSAX,
    UQSUB8,
    UQSUB16,
    SHADD8,
    SHADD16,
    SHASX,
    SHSAX,
    SHSUB8,
    SHSUB16,
    UHADD8,
    UHADD16,
    UHASX,
    UHSAX,
    UHSUB8,
    UHSUB16,
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
    SXTB16,
    SXTAB16,
    UXTAH,
    UXTAB,
    UXTB16,
    UXTAB16,

    // Barriers
    DMB,
    DSB,
    ISB,
    CLREX,
    BXJ,

    // System
    MRS_reg,
    MSR_reg,
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

/// Decode a 32-bit Thumb instruction from two halfwords.
pub fn decode_thumb32(hw1: u16, hw2: u16) -> DecodedThumb32 {
    let raw = ((hw1 as u32) << 16) | (hw2 as u32);
    // Exact upstream thumb32.inc hint/preload encodings. These precede the
    // broader load-byte and branch groups in the generated decoder.
    let id = if matches_thumb32(raw, 0xFF7F_F000, 0xF81F_F000)
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
    } else if matches_thumb32(raw, 0xFF7F_F000, 0xF93F_F000)
        || matches_thumb32(raw, 0xFFF0_FFC0, 0xF930_F000)
        || matches_thumb32(raw, 0xFFF0_FF00, 0xF930_FC00)
        || matches_thumb32(raw, 0xFFF0_F000, 0xF9B0_F000)
    {
        Thumb32InstId::NOP
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
    } else if raw >> 24 == 0xfa {
        decode_thumb32_fa(raw)
    } else if matches_thumb32(raw, 0xFF80_0000, 0xFB00_0000) {
        decode_thumb32_multiply(raw)
    } else if matches_thumb32(raw, 0xFF80_0000, 0xFB80_0000) {
        decode_thumb32_long_multiply(raw)
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

fn decode_thumb32_fa(raw: u32) -> Thumb32InstId {
    let register = decode_thumb32_dp_register(raw);
    if register != Thumb32InstId::Unknown {
        return register;
    }
    let parallel = decode_thumb32_parallel(raw);
    if parallel != Thumb32InstId::Unknown {
        return parallel;
    }
    decode_thumb32_misc(raw)
}

fn decode_thumb32_parallel(raw: u32) -> Thumb32InstId {
    use Thumb32InstId::*;
    for (mask, expected, id) in [
        (0xfff0_f0f0, 0xfa90_f000, SADD16),
        (0xfff0_f0f0, 0xfaa0_f000, SASX),
        (0xfff0_f0f0, 0xfae0_f000, SSAX),
        (0xfff0_f0f0, 0xfad0_f000, SSUB16),
        (0xfff0_f0f0, 0xfa80_f000, SADD8),
        (0xfff0_f0f0, 0xfac0_f000, SSUB8),
        (0xfff0_f0f0, 0xfa90_f010, QADD16),
        (0xfff0_f0f0, 0xfaa0_f010, QASX),
        (0xfff0_f0f0, 0xfae0_f010, QSAX),
        (0xfff0_f0f0, 0xfad0_f010, QSUB16),
        (0xfff0_f0f0, 0xfa80_f010, QADD8),
        (0xfff0_f0f0, 0xfac0_f010, QSUB8),
        (0xfff0_f0f0, 0xfa90_f020, SHADD16),
        (0xfff0_f0f0, 0xfaa0_f020, SHASX),
        (0xfff0_f0f0, 0xfae0_f020, SHSAX),
        (0xfff0_f0f0, 0xfad0_f020, SHSUB16),
        (0xfff0_f0f0, 0xfa80_f020, SHADD8),
        (0xfff0_f0f0, 0xfac0_f020, SHSUB8),
        (0xfff0_f0f0, 0xfa90_f040, UADD16),
        (0xfff0_f0f0, 0xfaa0_f040, UASX),
        (0xfff0_f0f0, 0xfae0_f040, USAX),
        (0xfff0_f0f0, 0xfad0_f040, USUB16),
        (0xfff0_f0f0, 0xfa80_f040, UADD8),
        (0xfff0_f0f0, 0xfac0_f040, USUB8),
        (0xfff0_f0f0, 0xfa90_f050, UQADD16),
        (0xfff0_f0f0, 0xfaa0_f050, UQASX),
        (0xfff0_f0f0, 0xfae0_f050, UQSAX),
        (0xfff0_f0f0, 0xfad0_f050, UQSUB16),
        (0xfff0_f0f0, 0xfa80_f050, UQADD8),
        (0xfff0_f0f0, 0xfac0_f050, UQSUB8),
        (0xfff0_f0f0, 0xfa90_f060, UHADD16),
        (0xfff0_f0f0, 0xfaa0_f060, UHASX),
        (0xfff0_f0f0, 0xfae0_f060, UHSAX),
        (0xfff0_f0f0, 0xfad0_f060, UHSUB16),
        (0xfff0_f0f0, 0xfa80_f060, UHADD8),
        (0xfff0_f0f0, 0xfac0_f060, UHSUB8),
    ] {
        if matches_thumb32(raw, mask, expected) {
            return id;
        }
    }
    Unknown
}

fn decode_thumb32_dp_register(raw: u32) -> Thumb32InstId {
    for (mask, expected, id) in [
        (0xffe0_f0f0, 0xfa00_f000, Thumb32InstId::LSL_reg),
        (0xffe0_f0f0, 0xfa20_f000, Thumb32InstId::LSR_reg),
        (0xffe0_f0f0, 0xfa40_f000, Thumb32InstId::ASR_reg),
        (0xffe0_f0f0, 0xfa60_f000, Thumb32InstId::ROR_reg),
        (0xffff_f0c0, 0xfa0f_f080, Thumb32InstId::SXTH),
        (0xfff0_f0c0, 0xfa00_f080, Thumb32InstId::SXTAH),
        (0xffff_f0c0, 0xfa1f_f080, Thumb32InstId::UXTH),
        (0xfff0_f0c0, 0xfa10_f080, Thumb32InstId::UXTAH),
        (0xffff_f0c0, 0xfa2f_f080, Thumb32InstId::SXTB16),
        (0xfff0_f0c0, 0xfa20_f080, Thumb32InstId::SXTAB16),
        (0xffff_f0c0, 0xfa3f_f080, Thumb32InstId::UXTB16),
        (0xfff0_f0c0, 0xfa30_f080, Thumb32InstId::UXTAB16),
        (0xffff_f0c0, 0xfa4f_f080, Thumb32InstId::SXTB),
        (0xfff0_f0c0, 0xfa40_f080, Thumb32InstId::SXTAB),
        (0xffff_f0c0, 0xfa5f_f080, Thumb32InstId::UXTB),
        (0xfff0_f0c0, 0xfa50_f080, Thumb32InstId::UXTAB),
    ] {
        if matches_thumb32(raw, mask, expected) {
            return id;
        }
    }

    Thumb32InstId::Unknown
}

fn decode_thumb32_misc(raw: u32) -> Thumb32InstId {
    for (mask, expected, id) in [
        (0xfff0_f0f0, 0xfa80_f080, Thumb32InstId::QADD),
        (0xfff0_f0f0, 0xfa80_f090, Thumb32InstId::QDADD),
        (0xfff0_f0f0, 0xfa80_f0a0, Thumb32InstId::QSUB),
        (0xfff0_f0f0, 0xfa80_f0b0, Thumb32InstId::QDSUB),
        (0xfff0_f0f0, 0xfa90_f080, Thumb32InstId::REV),
        (0xfff0_f0f0, 0xfa90_f090, Thumb32InstId::REV16),
        (0xfff0_f0f0, 0xfa90_f0a0, Thumb32InstId::RBIT),
        (0xfff0_f0f0, 0xfa90_f0b0, Thumb32InstId::REVSH),
        (0xfff0_f0f0, 0xfaa0_f080, Thumb32InstId::SEL),
        (0xfff0_f0f0, 0xfab0_f080, Thumb32InstId::CLZ),
    ] {
        if matches_thumb32(raw, mask, expected) {
            return id;
        }
    }

    Thumb32InstId::Unknown
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
        _ => Thumb32InstId::Unknown,
    }
}

fn decode_thumb32_ls_multiple(raw: u32) -> Thumb32InstId {
    for (mask, expected, id) in [
        (0xffd0_8000, 0xe880_0000, Thumb32InstId::STMIA),
        (0xffff_0000, 0xe8bd_0000, Thumb32InstId::POP),
        (0xffd0_0000, 0xe890_0000, Thumb32InstId::LDMIA),
        (0xffff_8000, 0xe92d_0000, Thumb32InstId::PUSH),
        (0xffd0_8000, 0xe900_0000, Thumb32InstId::STMDB),
        (0xffd0_0000, 0xe910_0000, Thumb32InstId::LDMDB),
    ] {
        if matches_thumb32(raw, mask, expected) {
            return id;
        }
    }

    Thumb32InstId::Unknown
}

fn decode_thumb32_ls_dual_excl(raw: u32) -> Thumb32InstId {
    for (mask, expected, id) in [
        (0xfff0_0000, 0xe840_0000, Thumb32InstId::STREX),
        (0xfff0_0f00, 0xe850_0f00, Thumb32InstId::LDREX),
        (0xff70_0000, 0xe860_0000, Thumb32InstId::STRD_imm_1),
        (0xff50_0000, 0xe940_0000, Thumb32InstId::STRD_imm_2),
        (0xff7f_0000, 0xe87f_0000, Thumb32InstId::LDRD_lit_1),
        (0xff5f_0000, 0xe95f_0000, Thumb32InstId::LDRD_lit_2),
        (0xff70_0000, 0xe870_0000, Thumb32InstId::LDRD_imm_1),
        (0xff50_0000, 0xe950_0000, Thumb32InstId::LDRD_imm_2),
        (0xfff0_0fff, 0xe8c0_0faf, Thumb32InstId::STL),
        (0xfff0_0fff, 0xe8d0_0faf, Thumb32InstId::LDA),
        (0xfff0_0ff0, 0xe8c0_0f40, Thumb32InstId::STREXB),
        (0xfff0_0ff0, 0xe8c0_0f50, Thumb32InstId::STREXH),
        (0xfff0_00f0, 0xe8c0_0070, Thumb32InstId::STREXD),
        (0xfff0_fff0, 0xe8d0_f000, Thumb32InstId::TBB),
        (0xfff0_fff0, 0xe8d0_f010, Thumb32InstId::TBH),
        (0xfff0_0fff, 0xe8d0_0f4f, Thumb32InstId::LDREXB),
        (0xfff0_0fff, 0xe8d0_0f5f, Thumb32InstId::LDREXH),
        (0xfff0_00ff, 0xe8d0_007f, Thumb32InstId::LDREXD),
    ] {
        if matches_thumb32(raw, mask, expected) {
            return id;
        }
    }

    Thumb32InstId::Unknown
}

fn decode_thumb32_dp_shifted_reg(raw: u32) -> Thumb32InstId {
    for (mask, expected, id) in [
        (0xfff0_8f00, 0xea10_0f00, Thumb32InstId::TST_reg),
        (0xffe0_8000, 0xea00_0000, Thumb32InstId::AND_reg),
        (0xffe0_8000, 0xea20_0000, Thumb32InstId::BIC_reg),
        (0xffef_8000, 0xea4f_0000, Thumb32InstId::MOV_reg),
        (0xffe0_8000, 0xea40_0000, Thumb32InstId::ORR_reg),
        (0xffef_8000, 0xea6f_0000, Thumb32InstId::MVN_reg),
        (0xffe0_8000, 0xea60_0000, Thumb32InstId::ORN_reg),
        (0xfff0_8f00, 0xea90_0f00, Thumb32InstId::TEQ_reg),
        (0xffe0_8000, 0xea80_0000, Thumb32InstId::EOR_reg),
        (0xfff0_8010, 0xeac0_0000, Thumb32InstId::PKH),
        (0xfff0_8f00, 0xeb10_0f00, Thumb32InstId::CMN_reg),
        (0xffe0_8000, 0xeb00_0000, Thumb32InstId::ADD_reg),
        (0xffe0_8000, 0xeb40_0000, Thumb32InstId::ADC_reg),
        (0xffe0_8000, 0xeb60_0000, Thumb32InstId::SBC_reg),
        (0xfff0_8f00, 0xebb0_0f00, Thumb32InstId::CMP_reg),
        (0xffe0_8000, 0xeba0_0000, Thumb32InstId::SUB_reg),
        (0xffe0_8000, 0xebc0_0000, Thumb32InstId::RSB_reg),
    ] {
        if matches_thumb32(raw, mask, expected) {
            return id;
        }
    }

    Thumb32InstId::Unknown
}

fn decode_thumb32_dp_mod_imm(raw: u32) -> Thumb32InstId {
    for (mask, expected, id) in [
        (0xfbf0_8f00, 0xf010_0f00, Thumb32InstId::TST_imm),
        (0xfbe0_8000, 0xf000_0000, Thumb32InstId::AND_imm),
        (0xfbe0_8000, 0xf020_0000, Thumb32InstId::BIC_imm),
        (0xfbef_8000, 0xf04f_0000, Thumb32InstId::MOV_imm),
        (0xfbe0_8000, 0xf040_0000, Thumb32InstId::ORR_imm),
        (0xfbef_8000, 0xf06f_0000, Thumb32InstId::MVN_imm),
        (0xfbe0_8000, 0xf060_0000, Thumb32InstId::ORN_imm),
        (0xfbf0_8f00, 0xf090_0f00, Thumb32InstId::TEQ_imm),
        (0xfbe0_8000, 0xf080_0000, Thumb32InstId::EOR_imm),
        (0xfbf0_8f00, 0xf110_0f00, Thumb32InstId::CMN_imm),
        (0xfbe0_8000, 0xf100_0000, Thumb32InstId::ADD_imm_1),
        (0xfbe0_8000, 0xf140_0000, Thumb32InstId::ADC_imm),
        (0xfbe0_8000, 0xf160_0000, Thumb32InstId::SBC_imm),
        (0xfbf0_8f00, 0xf1b0_0f00, Thumb32InstId::CMP_imm),
        (0xfbe0_8000, 0xf1a0_0000, Thumb32InstId::SUB_imm_1),
        (0xfbe0_8000, 0xf1c0_0000, Thumb32InstId::RSB_imm),
    ] {
        if matches_thumb32(raw, mask, expected) {
            return id;
        }
    }

    Thumb32InstId::Unknown
}

fn decode_thumb32_dp_plain_imm(raw: u32) -> Thumb32InstId {
    // Keep the exact first-match ordering from upstream `thumb32.inc`.
    for (mask, expected, id) in [
        (0xfbff_8000, 0xf20f_0000, Thumb32InstId::ADR_t3),
        (0xfbf0_8000, 0xf200_0000, Thumb32InstId::ADD_imm_2),
        (0xfbf0_8000, 0xf240_0000, Thumb32InstId::MOVW_imm),
        (0xfbff_8000, 0xf2af_0000, Thumb32InstId::ADR_t2),
        (0xfbf0_8000, 0xf2a0_0000, Thumb32InstId::SUB_imm_2),
        (0xfbf0_8000, 0xf2c0_0000, Thumb32InstId::MOVT),
        (0xff70_f0f0, 0xf320_0010, Thumb32InstId::UDF),
        (0xfff0_f0f0, 0xf320_0000, Thumb32InstId::SSAT16),
        (0xfff0_f0f0, 0xf3a0_0000, Thumb32InstId::USAT16),
        (0xffd0_8020, 0xf300_0000, Thumb32InstId::SSAT),
        (0xffd0_8020, 0xf380_0000, Thumb32InstId::USAT),
        (0xfff0_8020, 0xf340_0000, Thumb32InstId::SBFX),
        (0xffff_8020, 0xf36f_0000, Thumb32InstId::BFC),
        (0xfff0_8020, 0xf360_0000, Thumb32InstId::BFI),
        (0xfff0_8020, 0xf3c0_0000, Thumb32InstId::UBFX),
    ] {
        if matches_thumb32(raw, mask, expected) {
            return id;
        }
    }

    Thumb32InstId::Unknown
}

fn decode_thumb32_branch(raw: u32) -> Thumb32InstId {
    if matches_thumb32(raw, 0xff70_f0f0, 0xf320_0010) {
        return Thumb32InstId::UDF;
    }

    for (mask, expected, id) in [
        (0xffe0_f0ff, 0xf380_8000, Thumb32InstId::MSR_reg),
        (0xffff_ffff, 0xf3af_8000, Thumb32InstId::NOP),
        (0xffff_ffff, 0xf3af_8001, Thumb32InstId::YIELD),
        (0xffff_ffff, 0xf3af_8002, Thumb32InstId::WFE),
        (0xffff_ffff, 0xf3af_8003, Thumb32InstId::WFI),
        (0xffff_ffff, 0xf3af_8004, Thumb32InstId::SEV),
        (0xffff_ffff, 0xf3af_8005, Thumb32InstId::SEVL),
        (0xffff_ffff, 0xf3bf_8f2f, Thumb32InstId::CLREX),
        (0xffff_fff0, 0xf3bf_8f40, Thumb32InstId::DSB),
        (0xffff_fff0, 0xf3bf_8f50, Thumb32InstId::DMB),
        (0xffff_fff0, 0xf3bf_8f60, Thumb32InstId::ISB),
        (0xfff0_ffff, 0xf3c0_8f00, Thumb32InstId::BXJ),
        (0xffef_f0ff, 0xf3ef_8000, Thumb32InstId::MRS_reg),
        (0xfff0_f000, 0xf7f0_a000, Thumb32InstId::UDF),
        (0xf800_d000, 0xf000_d000, Thumb32InstId::BL_imm),
        (0xf800_d000, 0xf000_c000, Thumb32InstId::BLX_imm),
        (0xf800_d000, 0xf000_9000, Thumb32InstId::B),
        (0xfb80_d000, 0xf380_8000, Thumb32InstId::UDF),
        (0xf800_d000, 0xf000_8000, Thumb32InstId::B_cond),
    ] {
        if matches_thumb32(raw, mask, expected) {
            return id;
        }
    }

    Thumb32InstId::Unknown
}

fn decode_thumb32_ls_single(raw: u32) -> Thumb32InstId {
    let op1 = (raw >> 23) & 3;
    let rn = (raw >> 16) & 0xF;
    let load = (raw >> 20) & 1 != 0;
    let half = (raw >> 21) & 1 != 0;
    let word = (raw >> 22) & 1 != 0;
    let indexed_imm8 = raw & (1 << 11) != 0;
    let unprivileged = (raw >> 8) & 0xf == 0b1110;

    match op1 {
        0b00 => {
            if load {
                if word {
                    if rn == 15 {
                        return Thumb32InstId::LDR_lit;
                    }
                    if indexed_imm8 {
                        if unprivileged {
                            Thumb32InstId::LDRT
                        } else {
                            Thumb32InstId::LDR_imm_t4
                        }
                    } else {
                        Thumb32InstId::LDR_reg
                    }
                } else if half {
                    if rn == 15 {
                        return Thumb32InstId::LDRH_lit;
                    }
                    if indexed_imm8 {
                        if unprivileged {
                            Thumb32InstId::LDRHT
                        } else {
                            Thumb32InstId::LDRH_imm_t3
                        }
                    } else {
                        Thumb32InstId::LDRH_reg
                    }
                } else {
                    if rn == 15 {
                        return Thumb32InstId::LDRB_lit;
                    }
                    if indexed_imm8 {
                        if unprivileged {
                            Thumb32InstId::LDRBT
                        } else {
                            Thumb32InstId::LDRB_imm_t3
                        }
                    } else {
                        Thumb32InstId::LDRB_reg
                    }
                }
            } else {
                if word {
                    let control = (raw >> 8) & 0xf;
                    if control == 0xc {
                        Thumb32InstId::STR_imm_2
                    } else if control == 0xe {
                        Thumb32InstId::STRT
                    } else if control & 0x9 == 0x9 {
                        Thumb32InstId::STR_imm_1
                    } else if raw & 0x0fc0 == 0 {
                        Thumb32InstId::STR_reg
                    } else {
                        Thumb32InstId::Unknown
                    }
                } else if half {
                    let control = (raw >> 8) & 0xf;
                    if control == 0xc {
                        Thumb32InstId::STRH_imm_2
                    } else if control == 0xe {
                        Thumb32InstId::STRHT
                    } else if control & 0x9 == 0x9 {
                        Thumb32InstId::STRH_imm_1
                    } else if raw & 0x0fc0 == 0 {
                        Thumb32InstId::STRH_reg
                    } else {
                        Thumb32InstId::Unknown
                    }
                } else {
                    let control = (raw >> 8) & 0xf;
                    if control == 0xc {
                        Thumb32InstId::STRB_imm_2
                    } else if control == 0xe {
                        Thumb32InstId::STRBT
                    } else if control & 0x9 == 0x9 {
                        Thumb32InstId::STRB_imm_1
                    } else if raw & 0x0fc0 == 0 {
                        Thumb32InstId::STRB_reg
                    } else {
                        Thumb32InstId::Unknown
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
                    Thumb32InstId::STR_imm_3
                } else if half {
                    Thumb32InstId::STRH_imm_3
                } else {
                    Thumb32InstId::STRB_imm_3
                }
            }
        }
        0b10 => {
            if load {
                if rn == 15 {
                    return if half {
                        Thumb32InstId::LDRSH_lit
                    } else {
                        Thumb32InstId::LDRSB_lit
                    };
                }
                if half {
                    if indexed_imm8 {
                        if unprivileged {
                            Thumb32InstId::LDRSHT
                        } else {
                            Thumb32InstId::LDRSH_imm_t2
                        }
                    } else {
                        Thumb32InstId::LDRSH_reg
                    }
                } else if indexed_imm8 {
                    if unprivileged {
                        Thumb32InstId::LDRSBT
                    } else {
                        Thumb32InstId::LDRSB_imm_t2
                    }
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
    for (mask, expected, id) in [
        (0xfff0_f0f0, 0xfb00_f000, Thumb32InstId::MUL),
        (0xfff0_00f0, 0xfb00_0000, Thumb32InstId::MLA),
        (0xfff0_00f0, 0xfb00_0010, Thumb32InstId::MLS),
        (0xfff0_f0c0, 0xfb10_f000, Thumb32InstId::SMULXY),
        (0xfff0_00c0, 0xfb10_0000, Thumb32InstId::SMLAXY),
        (0xfff0_f0e0, 0xfb20_f000, Thumb32InstId::SMUAD),
        (0xfff0_00e0, 0xfb20_0000, Thumb32InstId::SMLAD),
        (0xfff0_f0e0, 0xfb30_f000, Thumb32InstId::SMULWY),
        (0xfff0_00e0, 0xfb30_0000, Thumb32InstId::SMLAWY),
        (0xfff0_f0e0, 0xfb40_f000, Thumb32InstId::SMUSD),
        (0xfff0_00e0, 0xfb40_0000, Thumb32InstId::SMLSD),
        (0xfff0_f0e0, 0xfb50_f000, Thumb32InstId::SMMUL),
        (0xfff0_00e0, 0xfb50_0000, Thumb32InstId::SMMLA),
        (0xfff0_00e0, 0xfb60_0000, Thumb32InstId::SMMLS),
        (0xfff0_f0f0, 0xfb70_f000, Thumb32InstId::USAD8),
        (0xfff0_00f0, 0xfb70_0000, Thumb32InstId::USADA8),
    ] {
        if matches_thumb32(raw, mask, expected) {
            return id;
        }
    }

    Thumb32InstId::Unknown
}

fn decode_thumb32_long_multiply(raw: u32) -> Thumb32InstId {
    for (mask, expected, id) in [
        (0xfff0_00f0, 0xfb80_0000, Thumb32InstId::SMULL),
        (0xfff0_f0f0, 0xfb90_f0f0, Thumb32InstId::SDIV),
        (0xfff0_00f0, 0xfba0_0000, Thumb32InstId::UMULL),
        (0xfff0_f0f0, 0xfbb0_f0f0, Thumb32InstId::UDIV),
        (0xfff0_00f0, 0xfbc0_0000, Thumb32InstId::SMLAL),
        (0xfff0_00c0, 0xfbc0_0080, Thumb32InstId::SMLALXY),
        (0xfff0_00e0, 0xfbc0_00c0, Thumb32InstId::SMLALD),
        (0xfff0_00e0, 0xfbd0_00c0, Thumb32InstId::SMLSLD),
        (0xfff0_00f0, 0xfbe0_0000, Thumb32InstId::UMLAL),
        (0xfff0_00f0, 0xfbe0_0060, Thumb32InstId::UMAAL),
    ] {
        if matches_thumb32(raw, mask, expected) {
            return id;
        }
    }

    Thumb32InstId::Unknown
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
    fn test_decode_thumb32_bl() {
        // BL <offset> typical encoding
        // hw1: 1111 0 S imm10
        // hw2: 1 1 J1 1 J2 imm11
        let hw1: u16 = 0xF000; // S=0, imm10=0
        let hw2: u16 = 0xD000; // J1=0, J2=1, imm11=0
        let dec = decode_thumb32(hw1, hw2);
        assert_eq!(dec.id, Thumb32InstId::BL_imm);
    }

    #[test]
    fn test_decode_thumb32_branch_matches_upstream_patterns_and_udf_priority() {
        for (raw, expected) in [
            (0xF000_D000u32, Thumb32InstId::BL_imm),
            (0xF000_C000, Thumb32InstId::BLX_imm),
            (0xF000_9000, Thumb32InstId::B),
            (0xF000_8000, Thumb32InstId::B_cond),
            (0xF7E0_A123, Thumb32InstId::UDF),
            (0xF7F0_A123, Thumb32InstId::UDF),
            (0xF320_0010, Thumb32InstId::UDF),
        ] {
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                expected,
                "raw={raw:08X}"
            );
        }
    }

    #[test]
    fn test_decode_thumb32_control_matches_upstream_patterns() {
        let variable_bits = 0x001f_0f0fu32;
        for (mask, expected, id) in [
            (0xffe0_f0ff, 0xf380_8000, Thumb32InstId::MSR_reg),
            (0xffff_ffff, 0xf3af_8000, Thumb32InstId::NOP),
            (0xffff_ffff, 0xf3af_8001, Thumb32InstId::YIELD),
            (0xffff_ffff, 0xf3af_8002, Thumb32InstId::WFE),
            (0xffff_ffff, 0xf3af_8003, Thumb32InstId::WFI),
            (0xffff_ffff, 0xf3af_8004, Thumb32InstId::SEV),
            (0xffff_ffff, 0xf3af_8005, Thumb32InstId::SEVL),
            (0xffff_ffff, 0xf3bf_8f2f, Thumb32InstId::CLREX),
            (0xffff_fff0, 0xf3bf_8f40, Thumb32InstId::DSB),
            (0xffff_fff0, 0xf3bf_8f50, Thumb32InstId::DMB),
            (0xffff_fff0, 0xf3bf_8f60, Thumb32InstId::ISB),
            (0xfff0_ffff, 0xf3c0_8f00, Thumb32InstId::BXJ),
            (0xffef_f0ff, 0xf3ef_8000, Thumb32InstId::MRS_reg),
        ] {
            let raw = expected | (variable_bits & !mask);
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                id,
                "raw={raw:08X}"
            );
        }

        // Nearby reserved encodings must not be absorbed by the old broad
        // control classifier.
        assert_ne!(decode_thumb32(0xF3AF, 0x8010).id, Thumb32InstId::NOP);
        assert_ne!(decode_thumb32(0xF3BF, 0x8F20).id, Thumb32InstId::CLREX);
    }

    #[test]
    fn test_decode_thumb32_modified_immediate_matches_upstream_patterns() {
        let variable_bits = 0x0401_2255u32;
        for (mask, expected, id) in [
            (0xfbf0_8f00, 0xf010_0f00, Thumb32InstId::TST_imm),
            (0xfbe0_8000, 0xf000_0000, Thumb32InstId::AND_imm),
            (0xfbe0_8000, 0xf020_0000, Thumb32InstId::BIC_imm),
            (0xfbef_8000, 0xf04f_0000, Thumb32InstId::MOV_imm),
            (0xfbe0_8000, 0xf040_0000, Thumb32InstId::ORR_imm),
            (0xfbef_8000, 0xf06f_0000, Thumb32InstId::MVN_imm),
            (0xfbe0_8000, 0xf060_0000, Thumb32InstId::ORN_imm),
            (0xfbf0_8f00, 0xf090_0f00, Thumb32InstId::TEQ_imm),
            (0xfbe0_8000, 0xf080_0000, Thumb32InstId::EOR_imm),
            (0xfbf0_8f00, 0xf110_0f00, Thumb32InstId::CMN_imm),
            (0xfbe0_8000, 0xf100_0000, Thumb32InstId::ADD_imm_1),
            (0xfbe0_8000, 0xf140_0000, Thumb32InstId::ADC_imm),
            (0xfbe0_8000, 0xf160_0000, Thumb32InstId::SBC_imm),
            (0xfbf0_8f00, 0xf1b0_0f00, Thumb32InstId::CMP_imm),
            (0xfbe0_8000, 0xf1a0_0000, Thumb32InstId::SUB_imm_1),
            (0xfbe0_8000, 0xf1c0_0000, Thumb32InstId::RSB_imm),
        ] {
            let raw = expected | (variable_bits & !mask);
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                id,
                "raw={raw:08X}"
            );
        }
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
        assert_eq!(dec.id, Thumb32InstId::MOVW_imm);
        assert_eq!(dec.rd(), Reg::R0);
        assert_eq!(dec.imm16(), 0x42);
    }

    #[test]
    fn test_decode_thumb32_str_imm_3_zero_offset_not_reg() {
        let dec = decode_thumb32(0xF8C0, 0x1000);
        assert_eq!(dec.id, Thumb32InstId::STR_imm_3);
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
    fn test_decode_thumb32_strb_imm_3_zero_offset_not_reg() {
        let dec = decode_thumb32(0xF880, 0x1000);
        assert_eq!(dec.id, Thumb32InstId::STRB_imm_3);
    }

    #[test]
    fn test_decode_thumb32_ldrh_imm_t2_zero_offset_not_reg() {
        let dec = decode_thumb32(0xF8B0, 0x3000);
        assert_eq!(dec.id, Thumb32InstId::LDRH_imm_t2);
    }

    #[test]
    fn test_decode_thumb32_strh_imm_3_zero_offset_not_reg() {
        let dec = decode_thumb32(0xF8A0, 0x1000);
        assert_eq!(dec.id, Thumb32InstId::STRH_imm_3);
    }

    #[test]
    fn test_decode_thumb32_store_single_matches_upstream_patterns() {
        for (raw, expected) in [
            (0xF841_2B34u32, Thumb32InstId::STR_imm_1),
            (0xF841_2C34, Thumb32InstId::STR_imm_2),
            (0xF8C1_2234, Thumb32InstId::STR_imm_3),
            (0xF841_2E34, Thumb32InstId::STRT),
            (0xF841_2034, Thumb32InstId::STR_reg),
            (0xF801_2B34, Thumb32InstId::STRB_imm_1),
            (0xF801_2C34, Thumb32InstId::STRB_imm_2),
            (0xF881_2234, Thumb32InstId::STRB_imm_3),
            (0xF801_2E34, Thumb32InstId::STRBT),
            (0xF801_2034, Thumb32InstId::STRB_reg),
            (0xF821_2B34, Thumb32InstId::STRH_imm_1),
            (0xF821_2C34, Thumb32InstId::STRH_imm_2),
            (0xF8A1_2234, Thumb32InstId::STRH_imm_3),
            (0xF821_2E34, Thumb32InstId::STRHT),
            (0xF821_2034, Thumb32InstId::STRH_reg),
        ] {
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                expected,
                "raw={raw:08X}"
            );
        }

        for raw in [0xF841_2844u32, 0xF801_2A44, 0xF821_2444] {
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                Thumb32InstId::Unknown,
                "reserved raw={raw:08X}"
            );
        }
    }

    #[test]
    fn test_decode_thumb32_ldrsb_imm_t1() {
        let dec = decode_thumb32(0xF990, 0x3000);
        assert_eq!(dec.id, Thumb32InstId::LDRSB_imm_t1);
    }

    #[test]
    fn test_decode_thumb32_unprivileged_loads_precede_imm8_patterns() {
        for (raw, expected) in [
            (0xF851_2E34u32, Thumb32InstId::LDRT),
            (0xF811_2E34u32, Thumb32InstId::LDRBT),
            (0xF831_2E34, Thumb32InstId::LDRHT),
            (0xF911_2E34, Thumb32InstId::LDRSBT),
            (0xF931_2E34, Thumb32InstId::LDRSHT),
        ] {
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                expected,
                "raw={raw:08X}"
            );
        }

        for (raw, expected) in [
            (0xF851_2C34u32, Thumb32InstId::LDR_imm_t4),
            (0xF811_2C34u32, Thumb32InstId::LDRB_imm_t3),
            (0xF831_2C34, Thumb32InstId::LDRH_imm_t3),
            (0xF911_2C34, Thumb32InstId::LDRSB_imm_t2),
            (0xF931_2C34, Thumb32InstId::LDRSH_imm_t2),
        ] {
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                expected,
                "adjacent imm8 raw={raw:08X}"
            );
        }

        for (raw, expected) in [
            (0xF85F_2E34u32, Thumb32InstId::LDR_lit),
            (0xF83F_2E34, Thumb32InstId::LDRH_lit),
            (0xF91F_2E34, Thumb32InstId::LDRSB_lit),
            (0xF93F_2E34, Thumb32InstId::LDRSH_lit),
        ] {
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                expected,
                "negative literal raw={raw:08X}"
            );
        }

        for raw in [0xF93F_F123u32, 0xF931_F000, 0xF931_FC00, 0xF9B1_F123] {
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                Thumb32InstId::NOP,
                "reserved LDRSH raw={raw:08X}"
            );
        }
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
    fn test_decode_thumb32_load_store_multiple_matches_upstream_patterns() {
        for (raw, expected) in [
            (0xE8A1_0003u32, Thumb32InstId::STMIA),
            (0xE8BD_8003, Thumb32InstId::POP),
            (0xE8B1_0003, Thumb32InstId::LDMIA),
            (0xE92D_4003, Thumb32InstId::PUSH),
            (0xE921_0003, Thumb32InstId::STMDB),
            (0xE931_0003, Thumb32InstId::LDMDB),
        ] {
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                expected,
                "raw={raw:08X}"
            );
        }
    }

    #[test]
    fn test_decode_thumb32_multiple_store_rejects_pc_list_bit() {
        for raw in [0xE8A1_8003u32, 0xE921_8003, 0xE92D_8003] {
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                Thumb32InstId::Unknown,
                "raw={raw:08X}"
            );
        }
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
        assert_eq!(dec.id, Thumb32InstId::STRD_imm_2);
    }

    #[test]
    fn test_decode_thumb32_load_store_dual_matches_upstream_patterns() {
        for (raw, expected) in [
            (0xE841_2304u32, Thumb32InstId::STREX),
            (0xE851_2F04, Thumb32InstId::LDREX),
            (0xE861_2304, Thumb32InstId::STRD_imm_1),
            (0xE941_2304, Thumb32InstId::STRD_imm_2),
            (0xE87F_2304, Thumb32InstId::LDRD_lit_1),
            (0xE95F_2304, Thumb32InstId::LDRD_lit_2),
            (0xE871_2304, Thumb32InstId::LDRD_imm_1),
            (0xE951_2304, Thumb32InstId::LDRD_imm_2),
            (0xE8C1_2FAF, Thumb32InstId::STL),
            (0xE8D1_2FAF, Thumb32InstId::LDA),
            (0xE8C1_2F43, Thumb32InstId::STREXB),
            (0xE8C1_2F53, Thumb32InstId::STREXH),
            (0xE8C1_2473, Thumb32InstId::STREXD),
            (0xE8D1_F003, Thumb32InstId::TBB),
            (0xE8D1_F013, Thumb32InstId::TBH),
            (0xE8D1_2F4F, Thumb32InstId::LDREXB),
            (0xE8D1_2F5F, Thumb32InstId::LDREXH),
            (0xE8D1_247F, Thumb32InstId::LDREXD),
        ] {
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                expected,
                "raw={raw:08X}"
            );
        }
    }

    #[test]
    fn test_decode_thumb32_e92d_with_pc_list_bit_is_reserved() {
        let hw1: u16 = 0xE92D;
        let hw2: u16 = 0xB018;
        let dec = decode_thumb32(hw1, hw2);
        assert_eq!(dec.id, Thumb32InstId::Unknown);
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
    fn test_decode_thumb32_multiply_matches_upstream_patterns() {
        for (raw, expected) in [
            (0xFB01_F203u32, Thumb32InstId::MUL),
            (0xFB01_4203, Thumb32InstId::MLA),
            (0xFB01_4213, Thumb32InstId::MLS),
            (0xFB11_F233, Thumb32InstId::SMULXY),
            (0xFB11_4233, Thumb32InstId::SMLAXY),
            (0xFB21_F213, Thumb32InstId::SMUAD),
            (0xFB21_4213, Thumb32InstId::SMLAD),
            (0xFB31_F213, Thumb32InstId::SMULWY),
            (0xFB31_4213, Thumb32InstId::SMLAWY),
            (0xFB41_F213, Thumb32InstId::SMUSD),
            (0xFB41_4213, Thumb32InstId::SMLSD),
            (0xFB51_F213, Thumb32InstId::SMMUL),
            (0xFB51_4213, Thumb32InstId::SMMLA),
            (0xFB61_4213, Thumb32InstId::SMMLS),
            (0xFB71_F203, Thumb32InstId::USAD8),
            (0xFB71_4203, Thumb32InstId::USADA8),
        ] {
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                expected,
                "raw={raw:08X}"
            );
        }
    }

    #[test]
    fn test_decode_thumb32_multiply_family_prefix_boundaries() {
        assert_eq!(decode_thumb32(0xFB81, 0x2303).id, Thumb32InstId::SMULL);
        assert_eq!(decode_thumb32(0xFC01, 0xF203).id, Thumb32InstId::STC);
    }

    #[test]
    fn test_decode_thumb32_long_multiply_matches_upstream_patterns() {
        for (raw, expected) in [
            (0xFB81_2303u32, Thumb32InstId::SMULL),
            (0xFB91_F2F3, Thumb32InstId::SDIV),
            (0xFBA1_2303, Thumb32InstId::UMULL),
            (0xFBB1_F2F3, Thumb32InstId::UDIV),
            (0xFBC1_2303, Thumb32InstId::SMLAL),
            (0xFBC1_23B3, Thumb32InstId::SMLALXY),
            (0xFBC1_23D3, Thumb32InstId::SMLALD),
            (0xFBD1_23D3, Thumb32InstId::SMLSLD),
            (0xFBE1_2303, Thumb32InstId::UMLAL),
            (0xFBE1_2363, Thumb32InstId::UMAAL),
        ] {
            assert_eq!(
                decode_thumb32((raw >> 16) as u16, raw as u16).id,
                expected,
                "raw={raw:08X}"
            );
        }
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
