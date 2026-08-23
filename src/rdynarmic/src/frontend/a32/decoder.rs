use crate::frontend::a32::types::{Reg, ShiftType};
use crate::ir::cond::Cond;

/// Decoded ARM (32-bit) instruction.
#[derive(Debug, Clone, Copy)]
pub struct DecodedArm {
    pub raw: u32,
    pub id: ArmInstId,
}

/// ARM instruction identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArmInstId {
    // Data processing - immediate
    AND_imm,
    EOR_imm,
    SUB_imm,
    RSB_imm,
    ADD_imm,
    ADC_imm,
    SBC_imm,
    RSC_imm,
    TST_imm,
    TEQ_imm,
    CMP_imm,
    CMN_imm,
    ORR_imm,
    MOV_imm,
    BIC_imm,
    MVN_imm,
    // Data processing - register
    AND_reg,
    EOR_reg,
    SUB_reg,
    RSB_reg,
    ADD_reg,
    ADC_reg,
    SBC_reg,
    RSC_reg,
    TST_reg,
    TEQ_reg,
    CMP_reg,
    CMN_reg,
    ORR_reg,
    MOV_reg,
    BIC_reg,
    MVN_reg,
    // Data processing - register-shifted register
    AND_rsr,
    EOR_rsr,
    SUB_rsr,
    RSB_rsr,
    ADD_rsr,
    ADC_rsr,
    SBC_rsr,
    RSC_rsr,
    TST_rsr,
    TEQ_rsr,
    CMP_rsr,
    CMN_rsr,
    ORR_rsr,
    MOV_rsr,
    BIC_rsr,
    MVN_rsr,
    // Branch
    B,
    BL,
    BX,
    BLX_reg,
    BLX_imm,
    // Load/Store
    LDR_imm,
    LDR_reg,
    LDR_lit,
    LDRB_imm,
    LDRB_reg,
    LDRB_lit,
    LDRH_imm,
    LDRH_reg,
    LDRH_lit,
    LDRSB_imm,
    LDRSB_reg,
    LDRSB_lit,
    LDRSH_imm,
    LDRSH_reg,
    LDRSH_lit,
    LDRD_imm,
    LDRD_reg,
    LDRD_lit,
    STR_imm,
    STR_reg,
    STRB_imm,
    STRB_reg,
    STRH_imm,
    STRH_reg,
    STRD_imm,
    STRD_reg,
    // Load/Store multiple
    LDM,
    LDMDA,
    LDMDB,
    LDMIB,
    STM,
    STMDA,
    STMDB,
    STMIB,
    // Multiply
    MUL,
    MLA,
    MLS,
    UMULL,
    UMLAL,
    SMULL,
    SMLAL,
    UMAAL,
    SMLALxy,
    SMLAxy,
    SMULxy,
    SMLAWy,
    SMULWy,
    SMMUL,
    SMMLA,
    SMMLS,
    SDIV,
    UDIV,
    // Extension
    SXTB,
    SXTH,
    SXTB16,
    SXTAB,
    SXTAH,
    SXTAB16,
    UXTB,
    UXTH,
    UXTB16,
    UXTAB,
    UXTAH,
    UXTAB16,
    // Misc
    CLZ,
    RBIT,
    REV,
    REV16,
    REVSH,
    MOVW,
    MOVT,
    NOP,
    BFC,
    BFI,
    SBFX,
    UBFX,
    SEL,
    // Saturated
    SSAT,
    USAT,
    SSAT16,
    USAT16,
    QADD,
    QSUB,
    QDADD,
    QDSUB,
    // Synchronization
    SWP,
    SWPB,
    STL,
    STLEX,
    LDREX,
    LDA,
    LDAEX,
    LDREXB,
    LDAB,
    LDAEXB,
    LDREXH,
    LDAH,
    LDAEXH,
    LDREXD,
    LDAEXD,
    STREX,
    STLEXB,
    STREXB,
    STLEXH,
    STREXH,
    STLEXD,
    STREXD,
    STLB,
    STLH,
    CLREX,
    // Status register
    MRS,
    MSR_imm,
    MSR_reg,
    // Barrier
    DMB,
    DSB,
    ISB,
    // Exception
    SVC,
    UDF,
    BKPT,
    // Hints
    PLD_imm,
    PLD_reg,
    SEV,
    SEVL,
    WFE,
    WFI,
    YIELD,
    // Packing
    PKHBT,
    PKHTB,
    // Coprocessor
    MCR,
    MRC,
    CDP,
    MRRC,
    MCRR,
    LDC,
    STC,
    // VFP load/store
    VPUSH,
    VPOP,
    VLDR_fp,
    VSTR_fp,
    VSTM,
    VLDM,
    // VFP three-register data processing
    VMLA_fp,
    VMLS_fp,
    VNMLS_fp,
    VNMLA_fp,
    VMUL_fp,
    VNMUL_fp,
    VADD_fp,
    VSUB_fp,
    VDIV_fp,
    VFNMS_fp,
    VFNMA_fp,
    VFMA_fp,
    VFMS_fp,
    VSEL_fp,
    VMAXNM_fp,
    VMINNM_fp,
    // VFP unary data processing
    VMOV_fp_reg,
    VMOV_fp_imm,
    VABS_fp,
    VNEG_fp,
    VSQRT_fp,
    VCMP_fp,
    VCMP_zero_fp,
    VCVT_f_to_f,
    VCVT_from_int,
    VCVT_to_u32,
    VCVT_to_s32,
    // VFP core register moves
    VMOV_u32_f64,
    VMOV_f64_u32,
    VMOV_u32_f32,
    VMOV_f32_u32,
    VMOV_2u32_2f32,
    VMOV_2f32_2u32,
    VMOV_2u32_f64,
    VMOV_f64_2u32,
    VMOV_from_i32,
    VMOV_to_i32,
    VMSR,
    VMRS,
    VFP_VDUP,
    // ASIMD
    ASIMD_VMOV_imm,
    ASIMD_VMOVN,
    ASIMD_VRHADD,
    ASIMD_VQRDMULH,
    ASIMD_VMUL_float,
    ASIMD_VADD_float,
    ASIMD_VSUB_float,
    ASIMD_VMLA_float,
    ASIMD_VMLS_float,
    ASIMD_VPADD_float,
    ASIMD_VORR_reg,
    ASIMD_VBSL,
    ASIMD_VRSQRTS,
    ASIMD_VCGT_reg_float,
    ASIMD_VMLA_scalar,
    ASIMD_VMUL_scalar,
    ASIMD_VDUP_scalar,
    ASIMD_VCGT_zero,
    ASIMD_VCGE_zero,
    ASIMD_VCEQ_zero,
    ASIMD_VCLE_zero,
    ASIMD_VCLT_zero,
    ASIMD_VRECPE,
    ASIMD_VRSQRTE,
    ASIMD_VCVT_integer,
    ASIMD_VMAX_float,
    ASIMD_VMIN_float,
    ASIMD_VTRN,
    ASIMD_VUZP,
    ASIMD_VZIP,
    ASIMD_VEXT,
    ASIMD_VTBL,
    ASIMD_VTBX,
    ASIMD_VNEG_int,
    ASIMD_VABS_int,
    VFP_VRINT_rm,
    VFP_VCVT_rm,
    // ASIMD integer three-register same
    ASIMD_VHADD,
    ASIMD_VQADD,
    ASIMD_VHSUB,
    ASIMD_VQSUB,
    ASIMD_VSHL_reg,
    ASIMD_VQSHL_reg,
    ASIMD_VRSHL,
    ASIMD_VADD_int,
    ASIMD_VSUB_int,
    ASIMD_VMUL_int,
    ASIMD_VMLA_int,
    ASIMD_VAND_reg,
    ASIMD_VBIC_reg,
    ASIMD_VORN_reg,
    ASIMD_VEOR_reg,
    ASIMD_VBIT,
    ASIMD_VBIF,
    ASIMD_VCGT_reg_int,
    ASIMD_VCGE_reg_int,
    ASIMD_VCEQ_reg_int,
    ASIMD_VTST,
    ASIMD_VABD_int,
    ASIMD_VABA,
    ASIMD_VMAX_int,
    ASIMD_VMIN_int,
    ASIMD_VPMAX_int,
    ASIMD_VPADD_int,
    ASIMD_VQDMULH,
    // ASIMD float three-register same (additional)
    ASIMD_VABD_float,
    ASIMD_VCEQ_reg_float,
    ASIMD_VCGE_reg_float,
    ASIMD_VACGE,
    ASIMD_VFMA,
    ASIMD_VFMS,
    ASIMD_VPMAX_float,
    ASIMD_VPMIN_float,
    ASIMD_VRECPS,
    // ASIMD three registers of different length
    ASIMD_VADDL,
    ASIMD_VSUBL,
    ASIMD_VABAL,
    ASIMD_VABDL,
    ASIMD_VMLAL,
    ASIMD_VMULL,
    // ASIMD two registers and shift amount
    ASIMD_SHR,
    ASIMD_SRA,
    ASIMD_VSHRN,
    ASIMD_VSHL_imm,
    ASIMD_VSLI,
    ASIMD_VSRI,
    ASIMD_VQSHL_imm,
    V8_VST_multiple,
    V8_VLD_multiple,
    V8_VST_single,
    V8_VLD_single,
    V8_VLD_all_lanes,
    // Unknown
    Unknown,
}

impl DecodedArm {
    /// Extract condition field (bits [31:28]).
    pub fn cond(&self) -> Cond {
        let c = ((self.raw >> 28) & 0xF) as u8;
        Cond::from_u8(c)
    }

    /// Extract Rd (bits [15:12]).
    pub fn rd(&self) -> Reg {
        Reg::from_u32((self.raw >> 12) & 0xF)
    }
    /// Extract Rn (bits [19:16]).
    pub fn rn(&self) -> Reg {
        Reg::from_u32((self.raw >> 16) & 0xF)
    }
    /// Extract Rm (bits [3:0]).
    pub fn rm(&self) -> Reg {
        Reg::from_u32(self.raw & 0xF)
    }
    /// Extract Rs (bits [11:8]).
    pub fn rs(&self) -> Reg {
        Reg::from_u32((self.raw >> 8) & 0xF)
    }
    /// Extract Rt (bits [15:12]) - same as Rd for load/store.
    pub fn rt(&self) -> Reg {
        self.rd()
    }

    /// Extract S flag (bit 20).
    pub fn s_flag(&self) -> bool {
        self.raw & (1 << 20) != 0
    }
    /// Extract P flag (bit 24).
    pub fn p_flag(&self) -> bool {
        self.raw & (1 << 24) != 0
    }
    /// Extract U flag (bit 23).
    pub fn u_flag(&self) -> bool {
        self.raw & (1 << 23) != 0
    }
    /// Extract W flag (bit 21).
    pub fn w_flag(&self) -> bool {
        self.raw & (1 << 21) != 0
    }

    /// Extract 12-bit immediate (bits [11:0]).
    pub fn imm12(&self) -> u32 {
        self.raw & 0xFFF
    }
    /// Extract 8-bit immediate (bits [7:0]).
    pub fn imm8(&self) -> u32 {
        self.raw & 0xFF
    }
    /// Extract rotate amount (bits [11:8]).
    pub fn rotate(&self) -> u32 {
        (self.raw >> 8) & 0xF
    }
    /// Extract 24-bit immediate (bits [23:0]).
    pub fn imm24(&self) -> u32 {
        self.raw & 0x00FF_FFFF
    }
    /// Extract 5-bit shift amount (bits [11:7]).
    pub fn imm5(&self) -> u32 {
        (self.raw >> 7) & 0x1F
    }
    /// Extract shift type (bits [6:5]).
    pub fn shift_type(&self) -> ShiftType {
        ShiftType::from_u8(((self.raw >> 5) & 3) as u8)
    }
    /// Extract register list (bits [15:0]).
    pub fn register_list(&self) -> u16 {
        (self.raw & 0xFFFF) as u16
    }

    /// Extract 4-bit immediate (bits [3:0]).
    pub fn imm4_lo(&self) -> u32 {
        self.raw & 0xF
    }
    /// Extract 4-bit immediate (bits [19:16]).
    pub fn imm4_hi(&self) -> u32 {
        (self.raw >> 16) & 0xF
    }
    /// H flag (bit 24) for BLX_imm
    pub fn h_flag(&self) -> bool {
        self.raw & (1 << 24) != 0
    }

    // --- Coprocessor instruction fields ---

    /// Coprocessor number (bits [11:8]).
    pub fn coproc_no(&self) -> u32 {
        (self.raw >> 8) & 0xF
    }
    /// Coprocessor opc1 (bits [23:21]) for MCR/MRC.
    pub fn coproc_opc1(&self) -> u32 {
        (self.raw >> 21) & 0x7
    }
    /// Coprocessor data-processing opc1 (bits [23:20]) for CDP.
    pub fn coproc_dp_opc1(&self) -> u32 {
        (self.raw >> 20) & 0xF
    }
    /// Coprocessor opc2 (bits [7:5]) for MCR/MRC.
    pub fn coproc_opc2(&self) -> u32 {
        (self.raw >> 5) & 0x7
    }
    /// CRn (bits [19:16]) — coprocessor source register.
    pub fn crn(&self) -> u32 {
        (self.raw >> 16) & 0xF
    }
    /// CRm (bits [3:0]) — coprocessor operand register.
    pub fn crm(&self) -> u32 {
        self.raw & 0xF
    }

    /// Extract Rt2 (bits [19:16]) for MRRC/MCRR.
    pub fn rt2(&self) -> Reg {
        Reg::from_u32((self.raw >> 16) & 0xF)
    }
    /// Extract opc (bits [7:4]) for MRRC/MCRR.
    pub fn mrrc_opc(&self) -> u32 {
        (self.raw >> 4) & 0xF
    }

    /// The `2` encoding selector used by unconditional-space coprocessor forms.
    pub fn coproc_two(&self) -> bool {
        self.cond() == Cond::NV
    }
}

/// Decode a 32-bit ARM instruction.
pub fn decode_arm(instr: u32) -> DecodedArm {
    let cond_bits = (instr >> 28) & 0xF;
    let op1 = (instr >> 25) & 0x7;
    let op = (instr >> 4) & 0xF;

    // Upstream arm.inc lists the architectural hint encodings before the
    // data-processing-immediate family. The low-nibble values 6..=15 are
    // reserved hints and, like upstream's catch-all entry, decode as NOP.
    let id = if matches_arm(instr, 0x0FFF_FFF0, 0x0320_F000) {
        match instr & 0xF {
            0 => ArmInstId::NOP,
            1 => ArmInstId::YIELD,
            2 => ArmInstId::WFE,
            3 => ArmInstId::WFI,
            4 => ArmInstId::SEV,
            5 => ArmInstId::SEVL,
            _ => ArmInstId::NOP,
        }
    } else {
        match (cond_bits, op1) {
            // Unconditional instructions (cond=0xF)
            (0xF, _) => decode_arm_unconditional(instr),
            // Data processing & misc
            (_, 0b000) => decode_arm_dp_misc(instr),
            (_, 0b001) => decode_arm_dp_imm_misc(instr),
            // Load/Store immediate offset
            (_, 0b010) => decode_arm_ls_imm(instr),
            // Load/Store register offset
            (_, 0b011) if op & 1 == 0 => decode_arm_ls_reg(instr),
            (_, 0b011) => decode_arm_media(instr),
            // Load/Store multiple
            (_, 0b100) => decode_arm_ls_multi(instr),
            // Branch
            (_, 0b101) => decode_arm_branch(instr),
            // Coprocessor / SVC
            (_, 0b110) => decode_arm_coproc_ls(instr),
            (_, 0b111) => decode_arm_coproc_svc(instr),
            _ => ArmInstId::Unknown,
        }
    };

    DecodedArm { raw: instr, id }
}

fn decode_arm_unconditional(instr: u32) -> ArmInstId {
    let op1 = (instr >> 20) & 0xFF;
    match op1 {
        // ASIMD load/store UNALLOCATED encodings. Upstream (asimd.inc) routes
        // these reserved patterns to `arm_UDF` → UndefinedInstruction, and its
        // most-specific-mask-wins matcher makes them beat the VST/VLD handlers
        // below (whose `DecodeError` for these types is therefore dead code).
        // Reproduce them here, ahead of the real load/store arms, so ruzu
        // raises UndefinedInstruction rather than DecodeError:
        //   - `111101000--0--------1011--------` : multiple, type == 0b1011
        //   - `111101000--0--------11----------` : multiple, type[3:2] == 0b11
        //   - `111101001-00--------11----------` : single-store, sz == 0b11
        _ if matches_arm(instr, 0xFF90_0F00, 0xF400_0B00) => ArmInstId::UDF,
        _ if matches_arm(instr, 0xFF90_0C00, 0xF400_0C00) => ArmInstId::UDF,
        _ if matches_arm(instr, 0xFFB0_0C00, 0xF480_0C00) => ArmInstId::UDF,
        // ASIMD load/store multiple structures (bit23=0). The mask includes
        // both bit23 (0x0080_0000) and bit20 (0x0010_0000) — both are fixed in
        // the upstream bitstrings (`111101000D{0,1}0...`) — so neither the
        // single-structure forms below (bit23=1) nor the UNALLOCATED encodings
        // with bit20=1 are swallowed by these patterns.
        _ if matches_arm(instr, 0xFFB0_0000, 0xF400_0000) => ArmInstId::V8_VST_multiple,
        _ if matches_arm(instr, 0xFFB0_0000, 0xF420_0000) => ArmInstId::V8_VLD_multiple,
        // ASIMD load/store single structures (bit23=1, bit20=0). VLD "all lanes"
        // is the more specific pattern (bits[11:10]=0b11) and must precede VLD
        // single.
        _ if matches_arm(instr, 0xFFB0_0000, 0xF480_0000) => ArmInstId::V8_VST_single,
        _ if matches_arm(instr, 0xFFB0_0C00, 0xF4A0_0C00) => ArmInstId::V8_VLD_all_lanes,
        _ if matches_arm(instr, 0xFFB0_0000, 0xF4A0_0000) => ArmInstId::V8_VLD_single,
        _ if matches_arm(instr, 0xFF80_0E50, 0xFE00_0A00) => ArmInstId::VSEL_fp,
        // VFPv5 numeric max/min
        _ if matches_arm(instr, 0xFFB0_0E50, 0xFE80_0A00) => ArmInstId::VMAXNM_fp,
        _ if matches_arm(instr, 0xFFB0_0E50, 0xFE80_0A40) => ArmInstId::VMINNM_fp,
        // VFP VCVT{A,N,P,M}: 111111101D1111mmdddd101zU1M0mmmm (unconditional)
        _ if matches_arm(instr, 0xFFBC_0E10, 0xFEBC_0A00) => ArmInstId::VFP_VCVT_rm,
        // VFP VRINT{A,N,P,M}: 111111101D1110mmdddd101z01M0mmmm (unconditional)
        _ if matches_arm(instr, 0xFFBC_0ED0, 0xFEB8_0A40) => ArmInstId::VFP_VRINT_rm,
        // VEXT: 111100101D11nnnnddddiiiiNQM0mmmm
        // Must be checked BEFORE two-register-shift (shares bit23=1 space)
        _ if matches_arm(instr, 0xFFB0_0010, 0xF2B0_0000) => ArmInstId::ASIMD_VEXT,
        // VTBL: 111100111D11nnnndddd10zzN0M0mmmm
        _ if matches_arm(instr, 0xFFB0_0C50, 0xF3B0_0800) => ArmInstId::ASIMD_VTBL,
        // VTBX: 111100111D11nnnndddd10zzN1M0mmmm
        _ if matches_arm(instr, 0xFFB0_0C50, 0xF3B0_0840) => ArmInstId::ASIMD_VTBX,
        // ASIMD modified immediate — must be checked BEFORE two-register-shift
        // because both have bit23=1, but VMOV_imm requires bits[21:19]=000.
        // Upstream decoder checks modified immediate first.
        // Pattern: 1111001i1D000bcdVVVVcccc0Qo1efgh
        _ if matches_arm(instr, 0xFEB8_0090, 0xF280_0010) => ArmInstId::ASIMD_VMOV_imm,
        // VSHRN: 111100101Diiiiiidddd100000M1mmmm
        _ if matches_arm(instr, 0xFF80_0FD0, 0xF280_0810) => ArmInstId::ASIMD_VSHRN,
        // ASIMD two registers and shift amount (bit 23=1, bit 4=1)
        // SHR:   1111001U1Diiiiiidddd0000LQM1mmmm
        _ if matches_arm(instr, 0xFE80_0F10, 0xF280_0010) => ArmInstId::ASIMD_SHR,
        // SRA:   1111001U1Diiiiiidddd0001LQM1mmmm
        _ if matches_arm(instr, 0xFE80_0F10, 0xF280_0110) => ArmInstId::ASIMD_SRA,
        // VSRI:  111100111Diiiiiidddd0100LQM1mmmm
        _ if matches_arm(instr, 0xFF80_0F10, 0xF380_0410) => ArmInstId::ASIMD_VSRI,
        // VSHL:  111100101Diiiiiidddd0101LQM1mmmm
        _ if matches_arm(instr, 0xFF80_0F10, 0xF280_0510) => ArmInstId::ASIMD_VSHL_imm,
        // VSLI:  111100111Diiiiiidddd0101LQM1mmmm
        _ if matches_arm(instr, 0xFF80_0F10, 0xF380_0510) => ArmInstId::ASIMD_VSLI,
        // VQSHL: 1111001U1Diiiiiidddd011oLQM1mmmm
        _ if matches_arm(instr, 0xFE80_0E10, 0xF280_0610) => ArmInstId::ASIMD_VQSHL_imm,

        // ASIMD integer three-register same
        // VHADD: 1111001U0Dzznnnndddd0000NQM0mmmm
        _ if matches_arm(instr, 0xFE80_0F10, 0xF200_0000) => ArmInstId::ASIMD_VHADD,
        // VQADD: 1111001U0Dzznnnndddd0000NQM1mmmm
        _ if matches_arm(instr, 0xFE80_0F10, 0xF200_0010) => ArmInstId::ASIMD_VQADD,
        // VAND: 111100100D00nnnndddd0001NQM1mmmm
        _ if matches_arm(instr, 0xFFB0_0F10, 0xF200_0110) => ArmInstId::ASIMD_VAND_reg,
        // VBIC: 111100100D01nnnndddd0001NQM1mmmm
        _ if matches_arm(instr, 0xFFB0_0F10, 0xF210_0110) => ArmInstId::ASIMD_VBIC_reg,
        // VORN: 111100100D11nnnndddd0001NQM1mmmm
        _ if matches_arm(instr, 0xFFB0_0F10, 0xF230_0110) => ArmInstId::ASIMD_VORN_reg,
        // VEOR: 111100110D00nnnndddd0001NQM1mmmm
        _ if matches_arm(instr, 0xFFB0_0F10, 0xF300_0110) => ArmInstId::ASIMD_VEOR_reg,
        // VBIT: 111100110D10nnnndddd0001NQM1mmmm
        _ if matches_arm(instr, 0xFFB0_0F10, 0xF320_0110) => ArmInstId::ASIMD_VBIT,
        // VBIF: 111100110D11nnnndddd0001NQM1mmmm
        _ if matches_arm(instr, 0xFFB0_0F10, 0xF330_0110) => ArmInstId::ASIMD_VBIF,
        // VHSUB: 1111001U0Dzznnnndddd0010NQM0mmmm
        _ if matches_arm(instr, 0xFE80_0F10, 0xF200_0200) => ArmInstId::ASIMD_VHSUB,
        // VQSUB: 1111001U0Dzznnnndddd0010NQM1mmmm
        _ if matches_arm(instr, 0xFE80_0F10, 0xF200_0210) => ArmInstId::ASIMD_VQSUB,
        // VCGT (integer): 1111001U0Dzznnnndddd0011NQM0mmmm
        _ if matches_arm(instr, 0xFE80_0F10, 0xF200_0300) => ArmInstId::ASIMD_VCGT_reg_int,
        // VCGE (integer): 1111001U0Dzznnnndddd0011NQM1mmmm
        _ if matches_arm(instr, 0xFE80_0F10, 0xF200_0310) => ArmInstId::ASIMD_VCGE_reg_int,
        // VSHL (register): 1111001U0Dzznnnndddd0100NQM0mmmm
        _ if matches_arm(instr, 0xFE80_0F10, 0xF200_0400) => ArmInstId::ASIMD_VSHL_reg,
        // VQSHL (register): 1111001U0Dzznnnndddd0100NQM1mmmm
        _ if matches_arm(instr, 0xFE80_0F10, 0xF200_0410) => ArmInstId::ASIMD_VQSHL_reg,
        // VRSHL: 1111001U0Dzznnnndddd0101NQM0mmmm
        _ if matches_arm(instr, 0xFE80_0F10, 0xF200_0500) => ArmInstId::ASIMD_VRSHL,
        // VMAX/VMIN (integer): 1111001U0Dzznnnnmmmm0110NQMommmm
        _ if matches_arm(instr, 0xFE80_0F00, 0xF200_0600) => ArmInstId::ASIMD_VMAX_int,
        // VABD (integer): 1111001U0Dzznnnndddd0111NQM0mmmm
        _ if matches_arm(instr, 0xFE80_0F10, 0xF200_0700) => ArmInstId::ASIMD_VABD_int,
        // VABA (integer): 1111001U0Dzznnnndddd0111NQM1mmmm
        _ if matches_arm(instr, 0xFE80_0F10, 0xF200_0710) => ArmInstId::ASIMD_VABA,
        // VADD (integer): 111100100Dzznnnndddd1000NQM0mmmm
        _ if matches_arm(instr, 0xFF80_0F10, 0xF200_0800) => ArmInstId::ASIMD_VADD_int,
        // VSUB (integer): 111100110Dzznnnndddd1000NQM0mmmm
        _ if matches_arm(instr, 0xFF80_0F10, 0xF300_0800) => ArmInstId::ASIMD_VSUB_int,
        // VTST:           111100100Dzznnnndddd1000NQM1mmmm
        _ if matches_arm(instr, 0xFF80_0F10, 0xF200_0810) => ArmInstId::ASIMD_VTST,
        // VCEQ (integer): 111100110Dzznnnndddd1000NQM1mmmm
        _ if matches_arm(instr, 0xFF80_0F10, 0xF300_0810) => ArmInstId::ASIMD_VCEQ_reg_int,
        // VMLA/VMLS (integer): 1111001o0Dzznnnndddd1001NQM0mmmm
        _ if matches_arm(instr, 0xFE80_0F10, 0xF200_0900) => ArmInstId::ASIMD_VMLA_int,
        // VMUL (integer): 1111001P0Dzznnnndddd1001NQM1mmmm
        _ if matches_arm(instr, 0xFE80_0F10, 0xF200_0910) => ArmInstId::ASIMD_VMUL_int,
        // VPMAX/VPMIN (integer): 1111001U0Dzznnnndddd1010NQMommmm
        _ if matches_arm(instr, 0xFE80_0F00, 0xF200_0A00) => ArmInstId::ASIMD_VPMAX_int,
        // VQDMULH: 111100100Dzznnnndddd1011NQM0mmmm
        _ if matches_arm(instr, 0xFF80_0F10, 0xF200_0B00) => ArmInstId::ASIMD_VQDMULH,
        // VQRDMULH: 111100110Dzznnnndddd1011NQM0mmmm
        _ if matches_arm(instr, 0xFF80_0F10, 0xF300_0B00) => ArmInstId::ASIMD_VQRDMULH,
        // VPADD (integer): 111100100Dzznnnndddd1011NQM1mmmm
        _ if matches_arm(instr, 0xFF80_0F10, 0xF200_0B10) => ArmInstId::ASIMD_VPADD_int,
        // VRHADD: 1111001U0Dzznnnndddd0001NQM0mmmm
        _ if matches_arm(instr, 0xFE80_0F10, 0xF200_0100) => ArmInstId::ASIMD_VRHADD,
        // ASIMD float three-register same
        // VFMA:   111100100D0znnnndddd1100NQM1mmmm
        _ if matches_arm(instr, 0xFFA0_0F10, 0xF200_0C10) => ArmInstId::ASIMD_VFMA,
        // VFMS:   111100100D1znnnndddd1100NQM1mmmm
        _ if matches_arm(instr, 0xFFA0_0F10, 0xF220_0C10) => ArmInstId::ASIMD_VFMS,
        // VADD.F32:  111100100D0znnnndddd1101NQM0mmmm
        _ if matches_arm(instr, 0xFFA0_0F10, 0xF200_0D00) => ArmInstId::ASIMD_VADD_float,
        // VSUB.F32:  111100100D1znnnndddd1101NQM0mmmm
        _ if matches_arm(instr, 0xFFA0_0F10, 0xF220_0D00) => ArmInstId::ASIMD_VSUB_float,
        // VMLA.F32:  111100100D0znnnndddd1101NQM1mmmm
        _ if matches_arm(instr, 0xFFA0_0F10, 0xF200_0D10) => ArmInstId::ASIMD_VMLA_float,
        // VMLS.F32:  111100100D1znnnndddd1101NQM1mmmm
        _ if matches_arm(instr, 0xFFA0_0F10, 0xF220_0D10) => ArmInstId::ASIMD_VMLS_float,
        // VPADD.F32: 111100110D0znnnndddd1101NQM0mmmm
        _ if matches_arm(instr, 0xFFA0_0F10, 0xF300_0D00) => ArmInstId::ASIMD_VPADD_float,
        // VABD.F32: 111100110D1znnnndddd1101NQM0mmmm
        _ if matches_arm(instr, 0xFFA0_0F10, 0xF320_0D00) => ArmInstId::ASIMD_VABD_float,
        // VMLS.F32 (neg): 111100100D1znnnndddd1101NQM1mmmm (already above)
        // VMUL.F32:  111100110D0znnnndddd1101NQM1mmmm
        _ if matches_arm(instr, 0xFFB0_0F10, 0xF300_0D10) => ArmInstId::ASIMD_VMUL_float,
        // VCEQ.F32:  111100100D0znnnndddd1110NQM0mmmm
        _ if matches_arm(instr, 0xFFA0_0F10, 0xF200_0E00) => ArmInstId::ASIMD_VCEQ_reg_float,
        // VCGE.F32:  111100110D0znnnndddd1110NQM0mmmm
        _ if matches_arm(instr, 0xFFA0_0F10, 0xF300_0E00) => ArmInstId::ASIMD_VCGE_reg_float,
        // VCGT.F32:  111100110D1znnnndddd1110NQM0mmmm
        _ if matches_arm(instr, 0xFFA0_0F10, 0xF320_0E00) => ArmInstId::ASIMD_VCGT_reg_float,
        // VACGE:  111100110Doznnnndddd1110NQM1mmmm
        _ if matches_arm(instr, 0xFF80_0F10, 0xF300_0E10) => ArmInstId::ASIMD_VACGE,
        // VMAX.F32:  111100100D0znnnndddd1111NQM0mmmm
        _ if matches_arm(instr, 0xFFA0_0F10, 0xF200_0F00) => ArmInstId::ASIMD_VMAX_float,
        // VMIN.F32:  111100100D1znnnndddd1111NQM0mmmm
        _ if matches_arm(instr, 0xFFA0_0F10, 0xF220_0F00) => ArmInstId::ASIMD_VMIN_float,
        // VPMAX.F32: 111100110D0znnnndddd1111NQM0mmmm
        _ if matches_arm(instr, 0xFFA0_0F10, 0xF300_0F00) => ArmInstId::ASIMD_VPMAX_float,
        // VPMIN.F32: 111100110D1znnnndddd1111NQM0mmmm
        _ if matches_arm(instr, 0xFFA0_0F10, 0xF320_0F00) => ArmInstId::ASIMD_VPMIN_float,
        // VRECPS:  111100100D0znnnndddd1111NQM1mmmm
        _ if matches_arm(instr, 0xFFA0_0F10, 0xF200_0F10) => ArmInstId::ASIMD_VRECPS,
        // VRSQRTS: 111100100D1znnnndddd1111NQM1mmmm
        _ if matches_arm(instr, 0xFFA0_0F10, 0xF220_0F10) => ArmInstId::ASIMD_VRSQRTS,
        // VORR (register): 111100100D10nnnndddd0001NQM1mmmm
        _ if matches_arm(instr, 0xFFB0_0F10, 0xF220_0110) => ArmInstId::ASIMD_VORR_reg,
        // VBSL: 111100110D01nnnndddd0001NQM1mmmm
        _ if matches_arm(instr, 0xFFB0_0F10, 0xF310_0110) => ArmInstId::ASIMD_VBSL,
        // ASIMD three registers of different length (bit23=1, bit4=0)
        // Must be checked before VMLA_scalar/VMUL_scalar.
        // sz=11 is UNDEFINED for all these — reject it to avoid stealing VDUP_scalar.
        // VABAL: 1111001U1Dzznnnndddd0101N0M0mmmm
        _ if matches_arm(instr, 0xFE80_0F50, 0xF280_0500) && (instr >> 20) & 3 != 3 => {
            ArmInstId::ASIMD_VABAL
        }
        // VABDL: 1111001U1Dzznnnndddd0111N0M0mmmm
        _ if matches_arm(instr, 0xFE80_0F50, 0xF280_0700) && (instr >> 20) & 3 != 3 => {
            ArmInstId::ASIMD_VABDL
        }
        // VADDL/VADDW: 1111001U1Dzznnnndddd000oN0M0mmmm
        _ if matches_arm(instr, 0xFE80_0E50, 0xF280_0000) && (instr >> 20) & 3 != 3 => {
            ArmInstId::ASIMD_VADDL
        }
        // VSUBL/VSUBW: 1111001U1Dzznnnndddd001oN0M0mmmm
        _ if matches_arm(instr, 0xFE80_0E50, 0xF280_0200) && (instr >> 20) & 3 != 3 => {
            ArmInstId::ASIMD_VSUBL
        }
        // VMLAL/VMLSL: 1111001U1Dzznnnndddd10o0N0M0mmmm
        _ if matches_arm(instr, 0xFE80_0D50, 0xF280_0800) && (instr >> 20) & 3 != 3 => {
            ArmInstId::ASIMD_VMLAL
        }
        // VMULL: 1111001U1Dzznnnndddd11P0N0M0mmmm
        _ if matches_arm(instr, 0xFE80_0D50, 0xF280_0C00) && (instr >> 20) & 3 != 3 => {
            ArmInstId::ASIMD_VMULL
        }
        // VTRN:         111100111D11zz10dddd00001QM0mmmm
        _ if matches_arm(instr, 0xFFB3_0F90, 0xF3B2_0080) => ArmInstId::ASIMD_VTRN,
        // VUZP:         111100111D11zz10dddd00010QM0mmmm
        _ if matches_arm(instr, 0xFFB3_0F90, 0xF3B2_0100) => ArmInstId::ASIMD_VUZP,
        // VZIP:         111100111D11zz10dddd00011QM0mmmm
        _ if matches_arm(instr, 0xFFB3_0F90, 0xF3B2_0180) => ArmInstId::ASIMD_VZIP,
        // VMOVN:        111100111D11zz10dddd001000M0mmmm
        _ if matches_arm(instr, 0xFFB3_0FD0, 0xF3B2_0200) => ArmInstId::ASIMD_VMOVN,
        // VCGT (zero):  111100111D11zz01dddd0F000QM0mmmm
        _ if matches_arm(instr, 0xFFB3_0B90, 0xF3B1_0000) => ArmInstId::ASIMD_VCGT_zero,
        // VCGE (zero):  111100111D11zz01dddd0F001QM0mmmm
        _ if matches_arm(instr, 0xFFB3_0B90, 0xF3B1_0080) => ArmInstId::ASIMD_VCGE_zero,
        // VCEQ (zero):  111100111D11zz01dddd0F010QM0mmmm
        _ if matches_arm(instr, 0xFFB3_0B90, 0xF3B1_0100) => ArmInstId::ASIMD_VCEQ_zero,
        // VCLE (zero):  111100111D11zz01dddd0F011QM0mmmm
        _ if matches_arm(instr, 0xFFB3_0B90, 0xF3B1_0180) => ArmInstId::ASIMD_VCLE_zero,
        // VCLT (zero):  111100111D11zz01dddd0F100QM0mmmm
        _ if matches_arm(instr, 0xFFB3_0B90, 0xF3B1_0200) => ArmInstId::ASIMD_VCLT_zero,
        // VRECPE:       111100111D11zz11dddd010F0QM0mmmm
        _ if matches_arm(instr, 0xFFB3_0E90, 0xF3B3_0400) => ArmInstId::ASIMD_VRECPE,
        // VRSQRTE:      111100111D11zz11dddd010F1QM0mmmm
        _ if matches_arm(instr, 0xFFB3_0E90, 0xF3B3_0480) => ArmInstId::ASIMD_VRSQRTE,
        // VMLA_scalar: 1111001Q1Dzznnnndddd0o0FN1M0mmmm
        _ if matches_arm(instr, 0xFE80_0A50, 0xF280_0040) => ArmInstId::ASIMD_VMLA_scalar,
        // VMUL_scalar: 1111001Q1Dzznnnndddd100FN1M0mmmm
        _ if matches_arm(instr, 0xFE80_0E50, 0xF280_0840) => ArmInstId::ASIMD_VMUL_scalar,
        // VDUP_scalar: 111100111D11iiiidddd11000QM0mmmm
        _ if matches_arm(instr, 0xFFB0_0F90, 0xF3B0_0C00) => ArmInstId::ASIMD_VDUP_scalar,
        // VNEG:         111100111D11zz01dddd001F1QM0mmmm
        _ if matches_arm(instr, 0xFFB3_0F90, 0xF3B1_0780) => ArmInstId::ASIMD_VNEG_int,
        // VNEG (int):   111100111D11zz01dddd00101QM0mmmm
        _ if matches_arm(instr, 0xFFB3_0F90, 0xF3B1_0380) => ArmInstId::ASIMD_VNEG_int,
        // VABS:         111100111D11zz01dddd001F0QM0mmmm
        _ if matches_arm(instr, 0xFFB3_0F90, 0xF3B1_0700) => ArmInstId::ASIMD_VABS_int,
        // VABS (int):   111100111D11zz01dddd00100QM0mmmm
        _ if matches_arm(instr, 0xFFB3_0F90, 0xF3B1_0300) => ArmInstId::ASIMD_VABS_int,
        // VCVT_integer: 111100111D11zz11dddd011oUQM0mmmm
        _ if matches_arm(instr, 0xFFB3_0E10, 0xF3B3_0600) => ArmInstId::ASIMD_VCVT_integer,
        // Generic coprocessor instructions in the unconditional encoding space.
        // Specific VFP/ASIMD encodings above retain priority, matching Eden's
        // DecodeVFP/DecodeASIMD-before-DecodeArm dispatch order.
        _ if matches_arm(instr, 0x0FF0_0000, 0x0C40_0000) => ArmInstId::MCRR,
        _ if matches_arm(instr, 0x0FF0_0000, 0x0C50_0000) => ArmInstId::MRRC,
        _ if matches_arm(instr, 0x0F10_0010, 0x0E00_0010) => ArmInstId::MCR,
        _ if matches_arm(instr, 0x0F10_0010, 0x0E10_0010) => ArmInstId::MRC,
        _ if matches_arm(instr, 0x0F00_0010, 0x0E00_0000) => ArmInstId::CDP,
        _ if matches_arm(instr, 0x0E10_0000, 0x0C10_0000) => ArmInstId::LDC,
        _ if matches_arm(instr, 0x0E10_0000, 0x0C00_0000) => ArmInstId::STC,
        // Barriers
        _ if instr & 0xFFFF_FFF0 == 0xF57F_F040 => ArmInstId::DSB,
        _ if instr & 0xFFFF_FFF0 == 0xF57F_F050 => ArmInstId::DMB,
        _ if instr & 0xFFFF_FFF0 == 0xF57F_F060 => ArmInstId::ISB,
        // PLD/PLDW immediate: 11110101uz01nnnn1111iiiiiiiiiiii.
        _ if instr & 0xFF30_F000 == 0xF510_F000 => ArmInstId::PLD_imm,
        // PLD/PLDW register: 11110111uz01nnnn1111iiiiitt0mmmm.
        _ if instr & 0xFF30_F010 == 0xF710_F000 => ArmInstId::PLD_reg,
        // BLX immediate
        _ if instr & 0xFE00_0000 == 0xFA00_0000 => ArmInstId::BLX_imm,
        // CLREX
        _ if instr == 0xF57F_F01F => ArmInstId::CLREX,
        _ => ArmInstId::Unknown,
    }
}

fn decode_arm_dp_misc(instr: u32) -> ArmInstId {
    let op = (instr >> 20) & 0x1F;
    let bit7 = (instr >> 7) & 1;
    let bit4 = (instr >> 4) & 1;

    // Multiply (Halfword) instructions. Upstream decodes these in this top-level
    // space before the generic misc bucket.
    if matches_arm(instr, 0x0FF0_0090, 0x0140_0080) {
        return ArmInstId::SMLALxy;
    }
    if matches_arm(instr, 0x0FF0_0090, 0x0100_0080) {
        return ArmInstId::SMLAxy;
    }
    if matches_arm(instr, 0x0FF0_0090, 0x0160_0080) {
        return ArmInstId::SMULxy;
    }
    if matches_arm(instr, 0x0FF0_00B0, 0x0120_0080) {
        return ArmInstId::SMLAWy;
    }
    if matches_arm(instr, 0x0FF0_00B0, 0x0120_00A0) {
        return ArmInstId::SMULWy;
    }

    // Misc instructions: op[24:23]=10, S=0 (op matches 10xx0)
    // Must be checked before multiply/RSR since they share bit4=1
    if op & 0b11001 == 0b10000 {
        if bit7 == 1 && bit4 == 1 {
            // Halfword multiply or extra load/store
            return decode_arm_multiply_misc(instr);
        }
        if bit4 == 1 {
            // Misc instructions with op2 encoding (BX, BLX, CLZ, etc.)
            return decode_arm_misc(instr);
        }
        return decode_arm_misc(instr);
    }

    // Multiply instructions: bit7=1, bit4=1
    if bit7 == 1 && bit4 == 1 {
        return decode_arm_multiply_misc(instr);
    }

    // Register-shifted register: bit4=1, bit7=0
    if bit4 == 1 && bit7 == 0 {
        return decode_arm_dp_rsr(op);
    }

    // Register: bit4=0
    match op {
        0b00000 | 0b00001 => ArmInstId::AND_reg,
        0b00010 | 0b00011 => ArmInstId::EOR_reg,
        0b00100 | 0b00101 => ArmInstId::SUB_reg,
        0b00110 | 0b00111 => ArmInstId::RSB_reg,
        0b01000 | 0b01001 => ArmInstId::ADD_reg,
        0b01010 | 0b01011 => ArmInstId::ADC_reg,
        0b01100 | 0b01101 => ArmInstId::SBC_reg,
        0b01110 | 0b01111 => ArmInstId::RSC_reg,
        0b10001 => ArmInstId::TST_reg,
        0b10011 => ArmInstId::TEQ_reg,
        0b10101 => ArmInstId::CMP_reg,
        0b10111 => ArmInstId::CMN_reg,
        0b10000 | 0b10010 | 0b10100 | 0b10110 => {
            // Misc instructions (op[24:23]=10, S=0)
            decode_arm_misc(instr)
        }
        0b11000 | 0b11001 => ArmInstId::ORR_reg,
        0b11010 | 0b11011 => ArmInstId::MOV_reg,
        0b11100 | 0b11101 => ArmInstId::BIC_reg,
        0b11110 | 0b11111 => ArmInstId::MVN_reg,
        _ => ArmInstId::Unknown,
    }
}

fn decode_arm_dp_rsr(op: u32) -> ArmInstId {
    match op >> 1 {
        0b0000 => ArmInstId::AND_rsr,
        0b0001 => ArmInstId::EOR_rsr,
        0b0010 => ArmInstId::SUB_rsr,
        0b0011 => ArmInstId::RSB_rsr,
        0b0100 => ArmInstId::ADD_rsr,
        0b0101 => ArmInstId::ADC_rsr,
        0b0110 => ArmInstId::SBC_rsr,
        0b0111 => ArmInstId::RSC_rsr,
        0b1000 if op & 1 == 1 => ArmInstId::TST_rsr,
        0b1001 if op & 1 == 1 => ArmInstId::TEQ_rsr,
        0b1010 if op & 1 == 1 => ArmInstId::CMP_rsr,
        0b1011 if op & 1 == 1 => ArmInstId::CMN_rsr,
        0b1100 => ArmInstId::ORR_rsr,
        0b1101 => ArmInstId::MOV_rsr,
        0b1110 => ArmInstId::BIC_rsr,
        0b1111 => ArmInstId::MVN_rsr,
        _ => ArmInstId::Unknown,
    }
}

fn decode_arm_misc(instr: u32) -> ArmInstId {
    let op2 = (instr >> 4) & 0xF;
    match op2 {
        0b0001 => {
            let op = (instr >> 21) & 3;
            match op {
                0b01 => ArmInstId::BX,
                0b11 => ArmInstId::CLZ,
                _ => ArmInstId::Unknown,
            }
        }
        0b0011 => ArmInstId::BLX_reg,
        0b0101 => {
            let op = (instr >> 21) & 3;
            match op {
                0b00 => ArmInstId::QADD,
                0b01 => ArmInstId::QSUB,
                0b10 => ArmInstId::QDADD,
                0b11 => ArmInstId::QDSUB,
                _ => ArmInstId::Unknown,
            }
        }
        0b0111 => ArmInstId::BKPT,
        _ => {
            // MRS/MSR
            let bit20 = (instr >> 20) & 1;
            if op2 == 0 && bit20 == 0 {
                if (instr >> 21) & 1 == 0 {
                    ArmInstId::MRS
                } else {
                    ArmInstId::MSR_reg
                }
            } else {
                ArmInstId::Unknown
            }
        }
    }
}

fn decode_arm_multiply_misc(instr: u32) -> ArmInstId {
    let op = (instr >> 20) & 0xF;
    let op2 = (instr >> 4) & 0xF;

    if op2 == 0b1001 {
        // Distinguish synchronization primitives from multiplies.
        // Sync primitives have bits[27:24] = 0001, multiplies have 0000.
        // At this point we have bits[27:25] = 000, so check bit[24].
        let bit24 = (instr >> 24) & 1;
        if bit24 == 1 {
            // Synchronization primitives: LDREX, STREX and variants
            // Encoding: cond 0001 1op0 Rn Rd/Rt 1111 1001 Rt/Rm
            return decode_arm_sync(instr);
        }

        // Standard multiplies (bits[27:24] = 0000)
        match op {
            0b0000 => ArmInstId::MUL,
            0b0001 => ArmInstId::MUL, // with S flag
            0b0010 => ArmInstId::MLA,
            0b0011 => ArmInstId::MLA, // with S flag
            0b0100 => ArmInstId::UMAAL,
            0b0110 => ArmInstId::MLS,
            0b1000 => ArmInstId::UMULL,
            0b1001 => ArmInstId::UMULL,
            0b1010 => ArmInstId::UMLAL,
            0b1011 => ArmInstId::UMLAL,
            0b1100 => ArmInstId::SMULL,
            0b1101 => ArmInstId::SMULL,
            0b1110 => ArmInstId::SMLAL,
            0b1111 => ArmInstId::SMLAL,
            _ => ArmInstId::Unknown,
        }
    } else if op2 == 0b1011 || op2 == 0b1101 || op2 == 0b1111 {
        // Extra load/store
        decode_arm_extra_ls(instr)
    } else {
        ArmInstId::Unknown
    }
}

/// Decode ARM synchronization primitives (SWP, LDREX/STREX family).
/// SWP/SWPB: cond 0001 0x00 Rn Rt 0000 1001 Rt2
/// LDREX/STREX: cond 0001 1xx0 Rn Rd/Rt 1111 1001 xxxx
fn decode_arm_sync(instr: u32) -> ArmInstId {
    if matches_arm(instr, 0x0FF0_FFF0, 0x0180_FC90) {
        return ArmInstId::STL;
    }
    if matches_arm(instr, 0x0FF0_0FF0, 0x0180_0E90) {
        return ArmInstId::STLEX;
    }
    if matches_arm(instr, 0x0FF0_0FFF, 0x0190_0C9F) {
        return ArmInstId::LDA;
    }
    if matches_arm(instr, 0x0FF0_0FFF, 0x0190_0E9F) {
        return ArmInstId::LDAEX;
    }
    if matches_arm(instr, 0x0FF0_0FF0, 0x01A0_0E90) {
        return ArmInstId::STLEXD;
    }
    if matches_arm(instr, 0x0FF0_0FFF, 0x01B0_0E9F) {
        return ArmInstId::LDAEXD;
    }
    if matches_arm(instr, 0x0FF0_FFF0, 0x01C0_FC90) {
        return ArmInstId::STLB;
    }
    if matches_arm(instr, 0x0FF0_0FF0, 0x01C0_0E90) {
        return ArmInstId::STLEXB;
    }
    if matches_arm(instr, 0x0FF0_0FFF, 0x01D0_0C9F) {
        return ArmInstId::LDAB;
    }
    if matches_arm(instr, 0x0FF0_0FFF, 0x01D0_0E9F) {
        return ArmInstId::LDAEXB;
    }
    if matches_arm(instr, 0x0FF0_FFF0, 0x01E0_FC90) {
        return ArmInstId::STLH;
    }
    if matches_arm(instr, 0x0FF0_0FF0, 0x01E0_0E90) {
        return ArmInstId::STLEXH;
    }
    if matches_arm(instr, 0x0FF0_0FFF, 0x01F0_0C9F) {
        return ArmInstId::LDAH;
    }
    if matches_arm(instr, 0x0FF0_0FFF, 0x01F0_0E9F) {
        return ArmInstId::LDAEXH;
    }

    let op = (instr >> 20) & 0xF;
    match op {
        // SWP: 0001 0000 (deprecated v6)
        0b0000 => ArmInstId::SWP,
        // SWPB: 0001 0100 (deprecated v6)
        0b0100 => ArmInstId::SWPB,
        // STREX: 0001 1000
        0b1000 => ArmInstId::STREX,
        // LDREX: 0001 1001
        0b1001 => ArmInstId::LDREX,
        // STREXD: 0001 1010
        0b1010 => ArmInstId::STREXD,
        // LDREXD: 0001 1011
        0b1011 => ArmInstId::LDREXD,
        // STREXB: 0001 1100
        0b1100 => ArmInstId::STREXB,
        // LDREXB: 0001 1101
        0b1101 => ArmInstId::LDREXB,
        // STREXH: 0001 1110
        0b1110 => ArmInstId::STREXH,
        // LDREXH: 0001 1111
        0b1111 => ArmInstId::LDREXH,
        _ => ArmInstId::Unknown,
    }
}

fn decode_arm_extra_ls(instr: u32) -> ArmInstId {
    let op1 = (instr >> 20) & 0x1F;
    let op2 = (instr >> 5) & 3;
    let load = op1 & 1 != 0;
    let imm = op1 & 0x4 != 0; // bit 22

    match (load, op2) {
        (false, 0b01) if imm => ArmInstId::STRH_imm,
        (false, 0b01) => ArmInstId::STRH_reg,
        (false, 0b10) if imm => ArmInstId::LDRD_imm,
        (false, 0b10) => ArmInstId::LDRD_reg,
        (false, 0b11) if imm => ArmInstId::STRD_imm,
        (false, 0b11) => ArmInstId::STRD_reg,
        (true, 0b01) if imm => ArmInstId::LDRH_imm,
        (true, 0b01) => ArmInstId::LDRH_reg,
        (true, 0b10) if imm => ArmInstId::LDRSB_imm,
        (true, 0b10) => ArmInstId::LDRSB_reg,
        (true, 0b11) if imm => ArmInstId::LDRSH_imm,
        (true, 0b11) => ArmInstId::LDRSH_reg,
        _ => ArmInstId::Unknown,
    }
}

fn decode_arm_dp_imm_misc(instr: u32) -> ArmInstId {
    let op = (instr >> 20) & 0x1F;
    match op {
        0b00000 | 0b00001 => ArmInstId::AND_imm,
        0b00010 | 0b00011 => ArmInstId::EOR_imm,
        0b00100 | 0b00101 => ArmInstId::SUB_imm,
        0b00110 | 0b00111 => ArmInstId::RSB_imm,
        0b01000 | 0b01001 => ArmInstId::ADD_imm,
        0b01010 | 0b01011 => ArmInstId::ADC_imm,
        0b01100 | 0b01101 => ArmInstId::SBC_imm,
        0b01110 | 0b01111 => ArmInstId::RSC_imm,
        0b10001 => ArmInstId::TST_imm,
        0b10011 => ArmInstId::TEQ_imm,
        0b10101 => ArmInstId::CMP_imm,
        0b10111 => ArmInstId::CMN_imm,
        0b10000 => ArmInstId::MOVW,
        0b10100 => ArmInstId::MOVT,
        0b10010 => ArmInstId::MSR_imm,
        0b10110 => ArmInstId::MSR_imm,
        0b11000 | 0b11001 => ArmInstId::ORR_imm,
        0b11010 | 0b11011 => ArmInstId::MOV_imm,
        0b11100 | 0b11101 => ArmInstId::BIC_imm,
        0b11110 | 0b11111 => ArmInstId::MVN_imm,
        _ => ArmInstId::Unknown,
    }
}

fn decode_arm_ls_imm(instr: u32) -> ArmInstId {
    let byte = (instr >> 22) & 1 != 0;
    let load = (instr >> 20) & 1 != 0;
    let rn = (instr >> 16) & 0xF;

    match (load, byte) {
        (true, false) if rn == 15 => ArmInstId::LDR_lit,
        (true, false) => ArmInstId::LDR_imm,
        (true, true) if rn == 15 => ArmInstId::LDRB_lit,
        (true, true) => ArmInstId::LDRB_imm,
        (false, false) => ArmInstId::STR_imm,
        (false, true) => ArmInstId::STRB_imm,
    }
}

fn decode_arm_ls_reg(instr: u32) -> ArmInstId {
    let byte = (instr >> 22) & 1 != 0;
    let load = (instr >> 20) & 1 != 0;

    match (load, byte) {
        (true, false) => ArmInstId::LDR_reg,
        (true, true) => ArmInstId::LDRB_reg,
        (false, false) => ArmInstId::STR_reg,
        (false, true) => ArmInstId::STRB_reg,
    }
}

fn matches_arm(instr: u32, mask: u32, expected: u32) -> bool {
    instr & mask == expected
}

fn decode_arm_media(instr: u32) -> ArmInstId {
    if matches_arm(instr, 0x0FF0_F0D0, 0x0750_F010) {
        return ArmInstId::SMMUL;
    }
    if matches_arm(instr, 0x0FF0_00D0, 0x0750_0010) {
        return ArmInstId::SMMLA;
    }
    if matches_arm(instr, 0x0FF0_00D0, 0x0750_00D0) {
        return ArmInstId::SMMLS;
    }
    if matches_arm(instr, 0x0FE0_007F, 0x07C0_001F) {
        return ArmInstId::BFC;
    }
    if matches_arm(instr, 0x0FE0_0070, 0x07C0_0010) {
        return ArmInstId::BFI;
    }
    if matches_arm(instr, 0x0FE0_0070, 0x07A0_0050) {
        return ArmInstId::SBFX;
    }
    if matches_arm(instr, 0x0FE0_0070, 0x07E0_0050) {
        return ArmInstId::UBFX;
    }
    if matches_arm(instr, 0x0FF0_0070, 0x0680_0010) {
        return ArmInstId::PKHBT;
    }
    if matches_arm(instr, 0x0FF0_0070, 0x0680_0050) {
        return ArmInstId::PKHTB;
    }
    if matches_arm(instr, 0x0FE0_0030, 0x06A0_0010) {
        return ArmInstId::SSAT;
    }
    if matches_arm(instr, 0x0FF0_0FF0, 0x06A0_0F30) {
        return ArmInstId::SSAT16;
    }
    if matches_arm(instr, 0x0FE0_0030, 0x06E0_0010) {
        return ArmInstId::USAT;
    }
    if matches_arm(instr, 0x0FF0_0FF0, 0x06E0_0F30) {
        return ArmInstId::USAT16;
    }
    if matches_arm(instr, 0x0FF0_0FF0, 0x0680_0FB0) {
        return ArmInstId::SEL;
    }
    if matches_arm(instr, 0x0FFF_03F0, 0x068F_0070) {
        return ArmInstId::SXTB16;
    }
    if matches_arm(instr, 0x0FF0_03F0, 0x0680_0070) {
        return ArmInstId::SXTAB16;
    }
    if matches_arm(instr, 0x0FFF_03F0, 0x06AF_0070) {
        return ArmInstId::SXTB;
    }
    if matches_arm(instr, 0x0FF0_03F0, 0x06A0_0070) {
        return ArmInstId::SXTAB;
    }
    if matches_arm(instr, 0x0FFF_03F0, 0x06BF_0070) {
        return ArmInstId::SXTH;
    }
    if matches_arm(instr, 0x0FF0_03F0, 0x06B0_0070) {
        return ArmInstId::SXTAH;
    }
    if matches_arm(instr, 0x0FFF_03F0, 0x06CF_0070) {
        return ArmInstId::UXTB16;
    }
    if matches_arm(instr, 0x0FF0_03F0, 0x06C0_0070) {
        return ArmInstId::UXTAB16;
    }
    if matches_arm(instr, 0x0FFF_03F0, 0x06EF_0070) {
        return ArmInstId::UXTB;
    }
    if matches_arm(instr, 0x0FF0_03F0, 0x06E0_0070) {
        return ArmInstId::UXTAB;
    }
    if matches_arm(instr, 0x0FFF_03F0, 0x06FF_0070) {
        return ArmInstId::UXTH;
    }
    if matches_arm(instr, 0x0FF0_03F0, 0x06F0_0070) {
        return ArmInstId::UXTAH;
    }
    if matches_arm(instr, 0x0FFF_0FF0, 0x06BF_0F30) {
        return ArmInstId::REV;
    }
    if matches_arm(instr, 0x0FFF_0FF0, 0x06BF_0FB0) {
        return ArmInstId::REV16;
    }
    if matches_arm(instr, 0x0FFF_0FF0, 0x06FF_0FB0) {
        return ArmInstId::REVSH;
    }
    if matches_arm(instr, 0x0FFF_0FF0, 0x06FF_0F30) {
        return ArmInstId::RBIT;
    }
    if matches_arm(instr, 0x0FF0_F0F0, 0x0710_F010) {
        return ArmInstId::SDIV;
    }
    if matches_arm(instr, 0x0FF0_F0F0, 0x0730_F010) {
        return ArmInstId::UDIV;
    }

    ArmInstId::Unknown
}

fn decode_arm_ls_multi(instr: u32) -> ArmInstId {
    let pu = (instr >> 23) & 3;
    let load = (instr >> 20) & 1 != 0;

    match (load, pu) {
        (true, 0b00) => ArmInstId::LDMDA,
        (true, 0b01) => ArmInstId::LDM, // LDMIA
        (true, 0b10) => ArmInstId::LDMDB,
        (true, 0b11) => ArmInstId::LDMIB,
        (false, 0b00) => ArmInstId::STMDA,
        (false, 0b01) => ArmInstId::STM, // STMIA
        (false, 0b10) => ArmInstId::STMDB,
        (false, 0b11) => ArmInstId::STMIB,
        _ => unreachable!(),
    }
}

fn decode_arm_branch(instr: u32) -> ArmInstId {
    if instr & (1 << 24) != 0 {
        ArmInstId::BL
    } else {
        ArmInstId::B
    }
}

fn decode_arm_coproc_svc(instr: u32) -> ArmInstId {
    if instr & (1 << 24) != 0 {
        // SVC: bit 24 = 1
        ArmInstId::SVC
    } else if let Some(vfp) = decode_arm_vfp_coproc(instr) {
        vfp
    } else if instr & (1 << 4) != 0 {
        // MCR/MRC: bit 4 = 1
        if instr & (1 << 20) != 0 {
            ArmInstId::MRC // L=1: read from coprocessor
        } else {
            ArmInstId::MCR // L=0: write to coprocessor
        }
    } else {
        ArmInstId::CDP // bit 4 = 0: coprocessor data processing
    }
}

fn decode_arm_vfp_coproc(instr: u32) -> Option<ArmInstId> {
    let coproc = (instr >> 8) & 0xF;
    if coproc != 0b1010 && coproc != 0b1011 {
        return None;
    }

    if matches_arm(instr, 0x0FF0_0FD0, 0x0C40_0A10) {
        return Some(ArmInstId::VMOV_2u32_2f32);
    }
    if matches_arm(instr, 0x0FF0_0FD0, 0x0C50_0A10) {
        return Some(ArmInstId::VMOV_2f32_2u32);
    }
    if matches_arm(instr, 0x0FF0_0FD0, 0x0C40_0B10) {
        return Some(ArmInstId::VMOV_2u32_f64);
    }
    if matches_arm(instr, 0x0FF0_0FD0, 0x0C50_0B10) {
        return Some(ArmInstId::VMOV_f64_2u32);
    }
    if matches_arm(instr, 0x0FFF_0FFF, 0x0EE1_0A10) {
        return Some(ArmInstId::VMSR);
    }
    if matches_arm(instr, 0x0FFF_0FFF, 0x0EF1_0A10) {
        return Some(ArmInstId::VMRS);
    }

    // Upstream decoder/vfp.inc ownership:
    //   VMLA:  cccc11100D00nnnndddd101zN0M0mmmm
    //   VMLS:  cccc11100D00nnnndddd101zN1M0mmmm
    //   VNMLS: cccc11100D01nnnndddd101zN0M0mmmm
    //   VNMLA: cccc11100D01nnnndddd101zN1M0mmmm
    //   VMUL:  cccc11100D10nnnndddd101zN0M0mmmm
    //   VNMUL: cccc11100D10nnnndddd101zN1M0mmmm
    //   VADD:  cccc11100D11nnnndddd101zN0M0mmmm
    //   VSUB:  cccc11100D11nnnndddd101zN1M0mmmm
    //   VDIV:  cccc11101D00nnnndddd101zN0M0mmmm
    if matches_arm(instr, 0x0FB0_0E50, 0x0E00_0A00) {
        return Some(ArmInstId::VMLA_fp);
    }
    if matches_arm(instr, 0x0FB0_0E50, 0x0E00_0A40) {
        return Some(ArmInstId::VMLS_fp);
    }
    if matches_arm(instr, 0x0FB0_0E50, 0x0E10_0A00) {
        return Some(ArmInstId::VNMLS_fp);
    }
    if matches_arm(instr, 0x0FB0_0E50, 0x0E10_0A40) {
        return Some(ArmInstId::VNMLA_fp);
    }
    if matches_arm(instr, 0x0FB0_0E50, 0x0E20_0A00) {
        return Some(ArmInstId::VMUL_fp);
    }
    if matches_arm(instr, 0x0FB0_0E50, 0x0E20_0A40) {
        return Some(ArmInstId::VNMUL_fp);
    }
    if matches_arm(instr, 0x0FB0_0E50, 0x0E30_0A00) {
        return Some(ArmInstId::VADD_fp);
    }
    if matches_arm(instr, 0x0FB0_0E50, 0x0E30_0A40) {
        return Some(ArmInstId::VSUB_fp);
    }
    if matches_arm(instr, 0x0FB0_0E50, 0x0E80_0A00) {
        return Some(ArmInstId::VDIV_fp);
    }
    // Upstream decoder/vfp.inc ownership (VFPv4 fused multiply-accumulate):
    //   VFNMS: cccc11101D01nnnndddd101zN0M0mmmm
    //   VFNMA: cccc11101D01nnnndddd101zN1M0mmmm
    //   VFMA:  cccc11101D10nnnndddd101zN0M0mmmm
    //   VFMS:  cccc11101D10nnnndddd101zN1M0mmmm
    if matches_arm(instr, 0x0FB0_0E50, 0x0E90_0A00) {
        return Some(ArmInstId::VFNMS_fp);
    }
    if matches_arm(instr, 0x0FB0_0E50, 0x0E90_0A40) {
        return Some(ArmInstId::VFNMA_fp);
    }
    if matches_arm(instr, 0x0FB0_0E50, 0x0EA0_0A00) {
        return Some(ArmInstId::VFMA_fp);
    }
    if matches_arm(instr, 0x0FB0_0E50, 0x0EA0_0A40) {
        return Some(ArmInstId::VFMS_fp);
    }
    // Upstream decoder/vfp.inc ownership:
    //   VMOV (reg): cccc11101D110000dddd101z01M0mmmm
    //   VABS:       cccc11101D110000dddd101z11M0mmmm
    //   VNEG:       cccc11101D110001dddd101z01M0mmmm
    //   VSQRT:      cccc11101D110001dddd101z11M0mmmm
    if matches_arm(instr, 0x0FBF_0ED0, 0x0EB0_0A40) {
        return Some(ArmInstId::VMOV_fp_reg);
    }
    if matches_arm(instr, 0x0FBF_0ED0, 0x0EB0_0AC0) {
        return Some(ArmInstId::VABS_fp);
    }
    if matches_arm(instr, 0x0FBF_0ED0, 0x0EB1_0A40) {
        return Some(ArmInstId::VNEG_fp);
    }
    if matches_arm(instr, 0x0FBF_0ED0, 0x0EB1_0AC0) {
        return Some(ArmInstId::VSQRT_fp);
    }
    // Upstream decoder/vfp.inc ownership:
    //   VCMP:        cccc11101D110100dddd101zE1M0mmmm
    //   VCMP (zero): cccc11101D110101dddd101zE1000000
    if matches_arm(instr, 0x0FBF_0E50, 0x0EB4_0A40) {
        return Some(ArmInstId::VCMP_fp);
    }
    if matches_arm(instr, 0x0FBF_0E7F, 0x0EB5_0A40) {
        return Some(ArmInstId::VCMP_zero_fp);
    }
    // Upstream decoder/vfp.inc ownership:
    //   VCVT (f32<->f64): cccc11101D110111dddd101z11M0mmmm
    if matches_arm(instr, 0x0FBF_0ED0, 0x0EB7_0AC0) {
        return Some(ArmInstId::VCVT_f_to_f);
    }
    // Upstream decoder/vfp.inc ownership:
    //   VCVT (from int): cccc11101D111000dddd101zs1M0mmmm
    if matches_arm(instr, 0x0FBF_0E50, 0x0EB8_0A40) {
        return Some(ArmInstId::VCVT_from_int);
    }
    // Upstream decoder/vfp.inc ownership:
    //   VCVT (to u32): cccc11101D111100dddd101zr1M0mmmm
    if matches_arm(instr, 0x0FBF_0E50, 0x0EBC_0A40) {
        return Some(ArmInstId::VCVT_to_u32);
    }
    // Upstream decoder/vfp.inc ownership:
    //   VCVT (to s32): cccc11101D111101dddd101zr1M0mmmm
    if matches_arm(instr, 0x0FBF_0E50, 0x0EBD_0A40) {
        return Some(ArmInstId::VCVT_to_s32);
    }
    // Upstream decoder/vfp.inc ownership:
    //   VMOV (imm): cccc11101D11vvvvdddd101z0000vvvv
    if matches_arm(instr, 0x0FB0_0EF0, 0x0EB0_0A00) {
        return Some(ArmInstId::VMOV_fp_imm);
    }
    // Upstream decoder/vfp.inc ownership:
    //   VMOV (core to f64): cccc11100000ddddtttt1011D0010000
    //   VMOV (f64 to core): cccc11100001nnnntttt1011N0010000
    //   VMOV (core to f32): cccc11100000nnnntttt1010N0010000
    //   VMOV (f32 to core): cccc11100001nnnntttt1010N0010000
    if matches_arm(instr, 0x0FF0_0F7F, 0x0E00_0B10) {
        return Some(ArmInstId::VMOV_u32_f64);
    }
    if matches_arm(instr, 0x0FF0_0F7F, 0x0E10_0B10) {
        return Some(ArmInstId::VMOV_f64_u32);
    }
    if matches_arm(instr, 0x0FF0_0F7F, 0x0E00_0A10) {
        return Some(ArmInstId::VMOV_u32_f32);
    }
    if matches_arm(instr, 0x0FF0_0F7F, 0x0E10_0A10) {
        return Some(ArmInstId::VMOV_f32_u32);
    }
    // Upstream decoder/vfp.inc ownership:
    //   VMOV (core to i32): cccc111000i0nnnntttt1011N0010000
    if matches_arm(instr, 0x0FD0_0F7F, 0x0E00_0B10) {
        return Some(ArmInstId::VMOV_from_i32);
    }
    // Upstream decoder/vfp.inc ownership:
    //   VMOV (i32 to core): cccc111000i1nnnntttt1011N0010000
    if matches_arm(instr, 0x0FD0_0F7F, 0x0E10_0B10) {
        return Some(ArmInstId::VMOV_to_i32);
    }
    // Upstream decoder/vfp.inc ownership:
    //   VDUP (from core): cccc11101BQ0ddddtttt1011D0E10000
    if matches_arm(instr, 0x0F90_0F5F, 0x0E80_0B10) {
        return Some(ArmInstId::VFP_VDUP);
    }

    None
}

fn decode_arm_coproc_ls(instr: u32) -> ArmInstId {
    // Category 0b110: coprocessor load/store and register transfers.
    if let Some(vfp) = decode_arm_vfp_coproc(instr) {
        return vfp;
    }

    // MRRC/MCRR: bits [27:21] = 1100_010, bit[20] = L (1=MRRC, 0=MCRR)
    if instr & 0x0FE0_0000 == 0x0C40_0000 {
        return if instr & (1 << 20) != 0 {
            ArmInstId::MRRC
        } else {
            ArmInstId::MCRR
        };
    }

    // VFP load/store: coprocessor = 10 or 11 (bits [11:8])
    let coproc = (instr >> 8) & 0xF;
    if coproc == 0b1010 || coproc == 0b1011 {
        let p = (instr >> 24) & 1;
        let u = (instr >> 23) & 1;
        let w = (instr >> 21) & 1;
        let load = (instr >> 20) & 1;
        let rn = (instr >> 16) & 0xF;

        // VPUSH: P=1,U=0,W=1,L=0,Rn=SP(13) → VSTMDB SP!, {regs}
        // VPOP:  P=0,U=1,W=1,L=1,Rn=SP(13) → VLDMIA SP!, {regs}
        if p == 1 && u == 0 && w == 1 && load == 0 && rn == 13 {
            return ArmInstId::VPUSH;
        }
        if p == 0 && u == 1 && w == 1 && load == 1 && rn == 13 {
            return ArmInstId::VPOP;
        }

        // VLDR/VSTR: P=1, W=0 (offset addressing, no writeback).
        // Upstream patterns:
        //   VLDR: cccc1101UD01nnnndddd101zvvvvvvvv
        //   VSTR: cccc1101UD00nnnndddd101zvvvvvvvv
        //
        // Do this before VSTM/VLDM because those patterns also allow W=0.
        if p == 1 && w == 0 {
            return if load == 1 {
                ArmInstId::VLDR_fp
            } else {
                ArmInstId::VSTR_fp
            };
        }

        // Upstream vfp.inc lists `arm_UDF "Undefined VSTM/VLDM"`
        // (`----11000-0---------101---------`) immediately before the VSTM/VLDM
        // entries; the VFP matcher is first-match in source order, so the
        // reserved P=0,U=0,W=0 addressing mode is UNDEFINED, not a valid
        // VSTM/VLDM. (VPUSH/VPOP, matched earlier, use different P/U/W.)
        if p == 0 && u == 0 && w == 0 {
            return ArmInstId::UDF;
        }

        // VSTM/VLDM includes non-writeback forms such as
        // `VSTMIA Rn, {S0-S3}` (P=0,U=1,W=0). Matching upstream:
        //   VSTM: cccc110puDw0nnnndddd101zvvvvvvvv
        //   VLDM: cccc110puDw1nnnndddd101zvvvvvvvv
        //
        // The translator performs the remaining undefined/unpredictable
        // checks. Misclassifying this as VSTR/VLDR stores a single register at
        // Rn+imm32 and can corrupt the caller stack frame.
        return if load == 1 {
            ArmInstId::VLDM
        } else {
            ArmInstId::VSTM
        };
    }

    if instr & (1 << 20) != 0 {
        ArmInstId::LDC
    } else {
        ArmInstId::STC
    }
}

/// Expand an ARM immediate: 8-bit value rotated right by 2*rotate.
pub fn arm_expand_imm(rotate: u32, imm8: u32) -> u32 {
    let unrotated = imm8 & 0xFF;
    let shift = (rotate & 0xF) * 2;
    unrotated.rotate_right(shift)
}

/// Expand ARM immediate with carry output.
pub fn arm_expand_imm_c(rotate: u32, imm8: u32, carry_in: bool) -> (u32, bool) {
    let unrotated = imm8 & 0xFF;
    let shift = (rotate & 0xF) * 2;
    if shift == 0 {
        (unrotated, carry_in)
    } else {
        let result = unrotated.rotate_right(shift);
        let carry = result & (1 << 31) != 0;
        (result, carry)
    }
}

/// Sign-extend a value from `bits` width to u32.
pub fn sign_extend(value: u32, bits: u32) -> u32 {
    let shift = 32 - bits;
    ((value as i32) << shift >> shift) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arm_expand_imm() {
        assert_eq!(arm_expand_imm(0, 0xFF), 0xFF);
        assert_eq!(arm_expand_imm(1, 0xFF), 0xFF << 30 | 0xFF >> 2);
        assert_eq!(arm_expand_imm(4, 0xFF), 0xFF00_0000);
    }

    #[test]
    fn test_sign_extend() {
        assert_eq!(sign_extend(0x80, 8), 0xFFFF_FF80);
        assert_eq!(sign_extend(0x7F, 8), 0x7F);
        assert_eq!(sign_extend(0x800000, 24), 0xFF80_0000);
    }

    #[test]
    fn test_decode_add_imm() {
        // ADD R1, R2, #5 (cond=AL, S=0)
        let instr = 0xE282_1005; // cccc 0010 100S nnnn dddd rrrr vvvvvvvv
        let dec = decode_arm(instr);
        assert_eq!(dec.id, ArmInstId::ADD_imm);
        assert_eq!(dec.rn(), Reg::R2);
        assert_eq!(dec.rd(), Reg::R1);
        assert_eq!(dec.imm8(), 5);
    }

    #[test]
    fn test_decode_mov_reg() {
        // MOV R0, R1 (cond=AL)
        let instr = 0xE1A0_0001; // cccc 0001 101S 0000 dddd 00000 000 mmmm
        let dec = decode_arm(instr);
        assert_eq!(dec.id, ArmInstId::MOV_reg);
        assert_eq!(dec.rd(), Reg::R0);
        assert_eq!(dec.rm(), Reg::R1);
    }

    #[test]
    fn test_decode_b() {
        // B +8 (AL)
        let instr = 0xEA00_0000;
        let dec = decode_arm(instr);
        assert_eq!(dec.id, ArmInstId::B);
    }

    #[test]
    fn test_decode_bl() {
        // BL +0 (AL)
        let instr = 0xEB00_0000;
        let dec = decode_arm(instr);
        assert_eq!(dec.id, ArmInstId::BL);
    }

    #[test]
    fn test_decode_ldr_imm() {
        // LDR R0, [R1, #4]
        let instr = 0xE591_0004;
        let dec = decode_arm(instr);
        assert_eq!(dec.id, ArmInstId::LDR_imm);
    }

    #[test]
    fn test_decode_str_imm() {
        // STR R0, [R1, #4]
        let instr = 0xE581_0004;
        let dec = decode_arm(instr);
        assert_eq!(dec.id, ArmInstId::STR_imm);
    }

    #[test]
    fn test_decode_ldm() {
        // LDMIA R13!, {R0-R3}
        let instr = 0xE8BD_000F;
        let dec = decode_arm(instr);
        assert_eq!(dec.id, ArmInstId::LDM);
    }

    #[test]
    fn test_decode_svc() {
        // SVC #0x21
        let instr = 0xEF00_0021;
        let dec = decode_arm(instr);
        assert_eq!(dec.id, ArmInstId::SVC);
    }

    #[test]
    fn test_decode_bx() {
        // BX LR
        let instr = 0xE12F_FF1E;
        let dec = decode_arm(instr);
        assert_eq!(dec.id, ArmInstId::BX);
    }

    #[test]
    fn test_decode_pld_immediate_unconditional_space() {
        // Upstream arm_PLD_imm: 11110101uz01nnnn1111iiiiiiiiiiii.
        // must not decode as Unknown/UndefinedInstruction.
        let dec = decode_arm(0xF595_F100);
        assert_eq!(dec.id, ArmInstId::PLD_imm);
    }

    #[test]
    fn test_decode_pld_register_unconditional_space() {
        // Upstream arm_PLD_reg: 11110111uz01nnnn1111iiiiitt0mmmm.
        let dec = decode_arm(0xF795_F000);
        assert_eq!(dec.id, ArmInstId::PLD_reg);
    }

    #[test]
    fn test_decode_arm_hints_match_upstream_priority() {
        for (raw, expected) in [
            (0xE320_F000, ArmInstId::NOP),
            (0x1320_F001, ArmInstId::YIELD),
            (0x2320_F002, ArmInstId::WFE),
            (0x3320_F003, ArmInstId::WFI),
            (0x4320_F004, ArmInstId::SEV),
            (0x5320_F005, ArmInstId::SEVL),
            (0xE320_F00F, ArmInstId::NOP),
        ] {
            assert_eq!(decode_arm(raw).id, expected);
        }
    }

    #[test]
    fn test_decode_mcr() {
        // MCR p15, 0, R0, c13, c0, 2 (write TPIDR_UPRW)
        // cond=AL(0xE) 1110 opc1=000 L=0 CRn=1101 Rt=0000 cp=1111 opc2=010 1 CRm=0000
        let instr = 0xEE0D_0F50;
        let dec = decode_arm(instr);
        assert_eq!(dec.id, ArmInstId::MCR);
        assert_eq!(dec.coproc_no(), 15);
        assert_eq!(dec.coproc_opc1(), 0);
        assert_eq!(dec.crn(), 13);
        assert_eq!(dec.crm(), 0);
        assert_eq!(dec.coproc_opc2(), 2);
        assert_eq!(dec.rt(), Reg::R0);
    }

    #[test]
    fn test_decode_mrc() {
        // MRC p15, 0, R0, c13, c0, 3 (read TPIDR_URO)
        // cond=AL(0xE) 1110 opc1=000 L=1 CRn=1101 Rt=0000 cp=1111 opc2=011 1 CRm=0000
        let instr = 0xEE1D_0F70;
        let dec = decode_arm(instr);
        assert_eq!(dec.id, ArmInstId::MRC);
        assert_eq!(dec.coproc_no(), 15);
        assert_eq!(dec.coproc_opc1(), 0);
        assert_eq!(dec.crn(), 13);
        assert_eq!(dec.crm(), 0);
        assert_eq!(dec.coproc_opc2(), 3);
        assert_eq!(dec.rt(), Reg::R0);
    }

    #[test]
    fn test_decode_mrrc() {
        // MRRC p15, 0, R0, R1, c14 (read CNTPCT)
        // cond=AL(0xE) 1100 0101 Rt2=0001 Rt=0000 cp=1111 opc=0000 CRm=1110
        let instr = 0xEC51_0F0E;
        let dec = decode_arm(instr);
        assert_eq!(dec.id, ArmInstId::MRRC);
        assert_eq!(dec.coproc_no(), 15);
        assert_eq!(dec.rt(), Reg::R0);
        assert_eq!(dec.rt2(), Reg::R1);
        assert_eq!(dec.mrrc_opc(), 0);
        assert_eq!(dec.crm(), 14);
    }

    #[test]
    fn test_decode_mcrr() {
        // MCRR p15, 0, R2, R3, c14
        // cond=AL(0xE) 1100 0100 Rt2=0011 Rt=0010 cp=1111 opc=0000 CRm=1110
        let instr = 0xEC43_2F0E;
        let dec = decode_arm(instr);
        assert_eq!(dec.id, ArmInstId::MCRR);
        assert_eq!(dec.coproc_no(), 15);
        assert_eq!(dec.rt(), Reg::R2);
        assert_eq!(dec.rt2(), Reg::R3);
        assert_eq!(dec.mrrc_opc(), 0);
        assert_eq!(dec.crm(), 14);
    }

    #[test]
    fn test_decode_generic_coprocessor_load_store() {
        let ldc = decode_arm(0xEDB4_7F22);
        assert_eq!(ldc.id, ArmInstId::LDC);
        assert_eq!(ldc.rn(), Reg::R4);
        assert_eq!(ldc.coproc_no(), 15);
        assert_eq!(ldc.rd(), Reg::R7);
        assert_eq!(ldc.imm8(), 0x22);

        let stc = decode_arm(0xED24_7F22);
        assert_eq!(stc.id, ArmInstId::STC);
    }

    #[test]
    fn test_decode_unconditional_coprocessor_two_forms() {
        assert_eq!(decode_arm(0xFE64_3F51).id, ArmInstId::MCR);
        assert_eq!(decode_arm(0xFE74_3F51).id, ArmInstId::MRC);
        assert_eq!(decode_arm(0xFEF4_3F41).id, ArmInstId::CDP);
        assert_eq!(decode_arm(0xFC42_3F1E).id, ArmInstId::MCRR);
        assert_eq!(decode_arm(0xFC52_3F1E).id, ArmInstId::MRRC);
        assert_eq!(decode_arm(0xFDB4_7F22).id, ArmInstId::LDC);
        assert_eq!(decode_arm(0xFD24_7F22).id, ArmInstId::STC);
        assert_eq!(decode_arm(0xFC42_3A1E).id, ArmInstId::MCRR);
        assert_eq!(decode_arm(0xFC52_3A1E).id, ArmInstId::MRRC);
    }

    #[test]
    fn test_vfp_specific_patterns_precede_generic_coprocessor_patterns() {
        assert_eq!(decode_arm(0xEC42_3A1E).id, ArmInstId::VMOV_2u32_2f32);
        assert_eq!(decode_arm(0xEC52_3A1E).id, ArmInstId::VMOV_2f32_2u32);
        assert_eq!(decode_arm(0xEC42_3B1E).id, ArmInstId::VMOV_2u32_f64);
        assert_eq!(decode_arm(0xEC52_3B1E).id, ArmInstId::VMOV_f64_2u32);
        assert_eq!(decode_arm(0xEEE1_0A10).id, ArmInstId::VMSR);
        assert_eq!(decode_arm(0xEEF1_FA10).id, ArmInstId::VMRS);
    }

    #[test]
    fn test_decode_media_bfc_crash_opcode() {
        let dec = decode_arm(0xE7DF_2F9F);
        assert_eq!(dec.id, ArmInstId::BFC);
        assert_eq!(dec.rd(), Reg::R2);
        assert_eq!(dec.rn(), Reg::R15);
    }

    #[test]
    fn test_decode_media_ubfx() {
        let dec = decode_arm(0xE7E1_2053);
        assert_eq!(dec.id, ArmInstId::UBFX);
    }

    #[test]
    fn test_decode_media_uxtb_crash_opcode() {
        let dec = decode_arm(0xE6EF_1071);
        assert_eq!(dec.id, ArmInstId::UXTB);
        assert_eq!(dec.rd(), Reg::R1);
        assert_eq!(dec.rm(), Reg::R1);
    }

    #[test]
    fn test_decode_asimd_vmov_imm_crash_opcode() {
        let dec = decode_arm(0xF2C0_0050);
        assert_eq!(dec.id, ArmInstId::ASIMD_VMOV_imm);
    }

    #[test]
    fn test_decode_vfp_vmov_imm_crash_opcode() {
        let dec = decode_arm(0xEEB7_1A00);
        assert_eq!(dec.id, ArmInstId::VMOV_fp_imm);
    }

    #[test]
    fn test_decode_vfp_vmov_f32_u32_crash_opcode() {
        let dec = decode_arm(0xEE10_1A10);
        assert_eq!(dec.id, ArmInstId::VMOV_f32_u32);
    }

    #[test]
    fn test_decode_vfp_vmov_u32_f32() {
        let dec = decode_arm(0xEE00_1A10);
        assert_eq!(dec.id, ArmInstId::VMOV_u32_f32);
    }

    #[test]
    fn test_decode_vfp_vcvt_f64_f32_crash_opcode() {
        let dec = decode_arm(0xEEF7_0AC0);
        assert_eq!(dec.id, ArmInstId::VCVT_f_to_f);
    }

    #[test]
    fn test_decode_vfp_vcvt_f32_f64_crash_opcode() {
        let dec = decode_arm(0xEEB7_0BE0);
        assert_eq!(dec.id, ArmInstId::VCVT_f_to_f);
    }

    #[test]
    fn test_decode_vfp_vcvt_f64_s32_crash_opcode() {
        let dec = decode_arm(0xEEB8_1AC1);
        assert_eq!(dec.id, ArmInstId::VCVT_from_int);
    }

    #[test]
    fn test_decode_vfp_vcvt_f64_s32_crash_opcode_pair() {
        let dec = decode_arm(0xEEB8_0AC0);
        assert_eq!(dec.id, ArmInstId::VCVT_from_int);
    }

    #[test]
    fn test_decode_vfp_vcmpe_f32_crash_opcode() {
        let dec = decode_arm(0xEEB4_0AC0);
        assert_eq!(dec.id, ArmInstId::VCMP_fp);
    }

    #[test]
    fn test_decode_vfp_vcmpe_zero_f32_crash_opcode() {
        let dec = decode_arm(0xEEB5_0AC0);
        assert_eq!(dec.id, ArmInstId::VCMP_zero_fp);
    }

    #[test]
    fn test_decode_ldrex() {
        // LDREX R3, [R4]: cond=AL 0001 1001 Rn=0100 Rt=0011 1111 1001 1111
        let dec = decode_arm(0xE194_3F9F);
        assert_eq!(dec.id, ArmInstId::LDREX);
    }

    #[test]
    fn test_decode_strex() {
        // STREX R0, R5, [R4]: cond=AL 0001 1000 Rn=0100 Rd=0000 1111 1001 Rm=0101
        let dec = decode_arm(0xE184_0F95);
        assert_eq!(dec.id, ArmInstId::STREX);
    }

    #[test]
    fn test_decode_ldaex_regression_crash_opcode() {
        // 0xE1941E9F was originally misdecoded as UMULL. The test was
        // first written expecting LDREX, but bits 11:8 = 1110 identify
        // this as LDAEX (ARMv8 ordered exclusive load); LDREX would
        // have bits 11:8 = 1111. Encoding: cond=AL Rn=r4 Rt=r1.
        let dec = decode_arm(0xE194_1E9F);
        assert_eq!(dec.id, ArmInstId::LDAEX);
    }

    #[test]
    fn test_decode_stlex_regression_crash_opcode() {
        // 0xE1840E95 was originally misdecoded as UMULL. The test was
        // first written expecting STREX, but bits 11:8 = 1110 identify
        // this as STLEX (ARMv8 ordered exclusive store); STREX would
        // have bits 11:8 = 1111. Encoding: cond=AL Rn=r4 Rd=r0 Rt=r5.
        let dec = decode_arm(0xE184_0E95);
        assert_eq!(dec.id, ArmInstId::STLEX);
    }

    #[test]
    fn test_decode_asimd_vst_multiple_crash_opcode() {
        let dec = decode_arm(0xF443_0A8D);
        assert_eq!(dec.id, ArmInstId::V8_VST_multiple);
    }

    #[test]
    fn test_decode_asimd_vld_all_lanes_regression_wedge_opcode() {
        // 0xF4E32CBF is VLD1.32 {d[]} (single element to all lanes / broadcast).
        // Its bit23=1 distinguishes it from the multiple-structure forms, but
        // the old VLD_multiple mask (0xFF20_0000) ignored bit23 and swallowed
        // it as a no-op. Once the exception path stopped swallowing faults,
        // VLD_all_lanes now.
        let dec = decode_arm(0xF4E3_2CBF);
        assert_eq!(dec.id, ArmInstId::V8_VLD_all_lanes);
    }

    #[test]
    fn test_decode_asimd_load_store_unallocated_maps_to_udf() {
        // Upstream routes these reserved encodings to arm_UDF (Undefined), where
        // its most-specific matcher beats the VST/VLD handlers' (dead) DecodeError.
        // multiple, type == 0b1011 (VST bit21=0 and VLD bit21=1):
        assert_eq!(decode_arm(0xF400_0B00).id, ArmInstId::UDF);
        assert_eq!(decode_arm(0xF420_0B00).id, ArmInstId::UDF);
        // multiple, type[3:2] == 0b11 (bits[11:10] == 0b11):
        assert_eq!(decode_arm(0xF400_0C00).id, ArmInstId::UDF);
        assert_eq!(decode_arm(0xF420_0F00).id, ArmInstId::UDF);
        // single store, sz == 0b11:
        assert_eq!(decode_arm(0xF480_0C00).id, ArmInstId::UDF);
        // Valid forms remain unaffected:
        assert_eq!(decode_arm(0xF420_0700).id, ArmInstId::V8_VLD_multiple); // type=0111 (VLD1)
        assert_eq!(decode_arm(0xF4A0_0C00).id, ArmInstId::V8_VLD_all_lanes); // bit21=1, [11:10]=11
    }

    #[test]
    fn test_decode_vfp_undefined_vstm_vldm_maps_to_udf() {
        // Reserved VSTM/VLDM addressing mode P=0,U=0,W=0 is UNDEFINED upstream
        // (arm_UDF is listed before VSTM/VLDM and the VFP matcher is first-match
        // in source order). Check across cond and the single/double (z) forms:
        assert_eq!(decode_arm(0xEC00_0A02).id, ArmInstId::UDF); // AL, z=0
        assert_eq!(decode_arm(0x1C00_0A02).id, ArmInstId::UDF); // cond=NE, z=0
        assert_eq!(decode_arm(0xEC00_0B04).id, ArmInstId::UDF); // AL, z=1 (double)
                                                                // Valid neighbours must NOT be swallowed:
        assert_eq!(decode_arm(0xEC80_0A02).id, ArmInstId::VSTM); // P=0,U=1,W=0
        assert_eq!(decode_arm(0xED00_0A02).id, ArmInstId::VSTR_fp); // P=1,W=0
    }

    #[test]
    fn test_decode_asimd_load_store_single_structure_routing() {
        // Tightened masks must route the single-structure forms (bit23=1) to
        // their own handlers rather than the multiple-structure ones, and keep
        // the multiple forms (bit23=0) unchanged.
        assert_eq!(decode_arm(0xF400_0000).id, ArmInstId::V8_VST_multiple); // bit23=0, L=0
        assert_eq!(decode_arm(0xF420_0000).id, ArmInstId::V8_VLD_multiple); // bit23=0, L=1
        assert_eq!(decode_arm(0xF480_0000).id, ArmInstId::V8_VST_single); // bit23=1, L=0
        assert_eq!(decode_arm(0xF4A0_0000).id, ArmInstId::V8_VLD_single); // bit23=1, L=1, [11:10]!=0b11
    }

    #[test]
    fn test_decode_smlabb_crash_opcode() {
        let dec = decode_arm(0xE103_9A8E);
        assert_eq!(dec.id, ArmInstId::SMLAxy);
    }

    #[test]
    fn test_decode_smmul_crash_opcode() {
        let dec = decode_arm(0xE750_F210);
        assert_eq!(dec.id, ArmInstId::SMMUL);
    }

    #[test]
    fn test_decode_vfp_vneg_f32() {
        let dec = decode_arm(0xEEB1_0A40);
        assert_eq!(dec.cond(), Cond::AL);
        assert_eq!(dec.id, ArmInstId::VNEG_fp);
    }

    #[test]
    fn test_decode_vfp_vdiv_f32() {
        let dec = decode_arm(0xEE81_0A00);
        assert_eq!(dec.cond(), Cond::AL);
        assert_eq!(dec.id, ArmInstId::VDIV_fp);
    }

    #[test]
    fn test_decode_vfp_vadd_f32() {
        let dec = decode_arm(0xEE30_0A00);
        assert_eq!(dec.cond(), Cond::AL);
        assert_eq!(dec.id, ArmInstId::VADD_fp);
    }

    #[test]
    fn test_decode_vfp_vadd_f64_n_bit_set_crash_opcode() {
        let dec = decode_arm(0xEE30_0BA1);
        assert_eq!(dec.cond(), Cond::AL);
        assert_eq!(dec.id, ArmInstId::VADD_fp);
    }

    #[test]
    fn test_decode_vfp_vmla_f32() {
        let dec = decode_arm(0xEE00_0A00);
        assert_eq!(dec.cond(), Cond::AL);
        assert_eq!(dec.id, ArmInstId::VMLA_fp);
    }

    fn assert_decodes_to_specific_vfp(instr: u32, expected: ArmInstId) {
        let dec = decode_arm(instr);
        assert_eq!(dec.cond(), Cond::AL);
        assert_eq!(dec.id, expected);
        assert_ne!(dec.id, ArmInstId::CDP, "VFP opcode fell through to CDP");
        assert_ne!(dec.id, ArmInstId::MCR, "VFP opcode fell through to MCR");
        assert_ne!(dec.id, ArmInstId::MRC, "VFP opcode fell through to MRC");
    }

    #[test]
    fn test_decode_vfp_v4_fused_mac_f32() {
        assert_decodes_to_specific_vfp(0xEEA0_0A00, ArmInstId::VFMA_fp);
        assert_decodes_to_specific_vfp(0xEEA0_0A40, ArmInstId::VFMS_fp);
        assert_decodes_to_specific_vfp(0xEE90_0A40, ArmInstId::VFNMA_fp);
        assert_decodes_to_specific_vfp(0xEE90_0A00, ArmInstId::VFNMS_fp);
    }

    #[test]
    fn test_decode_vfp_v4_fused_mac_f64() {
        assert_decodes_to_specific_vfp(0xEEA0_0B00, ArmInstId::VFMA_fp);
        assert_decodes_to_specific_vfp(0xEEA0_0B40, ArmInstId::VFMS_fp);
        assert_decodes_to_specific_vfp(0xEE90_0B40, ArmInstId::VFNMA_fp);
        assert_decodes_to_specific_vfp(0xEE90_0B00, ArmInstId::VFNMS_fp);
    }

    #[test]
    fn test_decode_vfp_vmaxnm_f32_crash_opcode() {
        let dec = decode_arm(0xFE82_2A01);
        assert_eq!(dec.id, ArmInstId::VMAXNM_fp);
    }

    #[test]
    fn test_decode_vfp_vminnm_f32_crash_opcode() {
        let dec = decode_arm(0xFE82_2A40);
        assert_eq!(dec.id, ArmInstId::VMINNM_fp);
    }

    #[test]
    fn test_decode_vfp_vsel_f32_crash_opcode() {
        let dec = decode_arm(0xFE32_1A01);
        assert_eq!(dec.id, ArmInstId::VSEL_fp);
    }

    #[test]
    fn test_decode_asimd_vmin_f32_crash_opcode() {
        let dec = decode_arm(0xF260_0F01);
        assert_eq!(dec.id, ArmInstId::ASIMD_VMIN_float);
    }

    #[test]
    fn test_decode_asimd_vmul_scalar_crash_opcode() {
        let dec = decode_arm(0xF3E0_29C0);
        assert_eq!(dec.id, ArmInstId::ASIMD_VMUL_scalar);
    }

    #[test]
    fn test_decode_asimd_vcvt_integer_crash_opcode() {
        let dec = decode_arm(0xF3FB_5622);
        assert_eq!(dec.id, ArmInstId::ASIMD_VCVT_integer);
    }

    #[test]
    fn test_decode_asimd_vmls_float_crash_opcode() {
        let dec = decode_arm(0xF262_0DF4);
        assert_eq!(dec.id, ArmInstId::ASIMD_VMLS_float);
    }

    #[test]
    fn test_decode_asimd_vcgt_reg_float_crash_opcode() {
        let dec = decode_arm(0xF360_2EE2);
        assert_eq!(dec.id, ArmInstId::ASIMD_VCGT_reg_float);
    }

    #[test]
    fn test_decode_asimd_vdup_scalar_crash_opcode() {
        let dec = decode_arm(0xF3F4_6C40);
        assert_eq!(dec.id, ArmInstId::ASIMD_VDUP_scalar);
    }

    #[test]
    fn test_decode_asimd_vtbl_regression_opcode() {
        let dec = decode_arm(0xF3F8_19AC);
        assert_eq!(dec.id, ArmInstId::ASIMD_VTBL);
    }

    #[test]
    fn test_decode_asimd_vtbx() {
        let dec = decode_arm(0xF3F8_19EC);
        assert_eq!(dec.id, ArmInstId::ASIMD_VTBX);
    }

    #[test]
    fn test_decode_asimd_vzip_regression_opcodes() {
        assert_eq!(decode_arm(0xF3FA_21E0).id, ArmInstId::ASIMD_VZIP);
        assert_eq!(decode_arm(0xF3FA_41E6).id, ArmInstId::ASIMD_VZIP);
    }

    #[test]
    fn test_decode_observed_narrowing_instructions() {
        assert_eq!(decode_arm(0xF2E0_3830).id, ArmInstId::ASIMD_VSHRN);
        assert_eq!(decode_arm(0xF3FA_2220).id, ArmInstId::ASIMD_VMOVN);
    }

    #[test]
    fn test_decode_asimd_vrsqrte_regression_opcodes() {
        assert_eq!(decode_arm(0xF3FB_05E2).id, ArmInstId::ASIMD_VRSQRTE);
        assert_eq!(decode_arm(0xF3FB_85E4).id, ArmInstId::ASIMD_VRSQRTE);
        assert_eq!(decode_arm(0xF3FB_E5E6).id, ArmInstId::ASIMD_VRSQRTE);
    }

    #[test]
    fn test_decode_asimd_vrecpe() {
        assert_eq!(decode_arm(0xF3FB_0562).id, ArmInstId::ASIMD_VRECPE);
    }

    #[test]
    fn test_decode_asimd_vceq_zero_regression_opcodes() {
        assert_eq!(decode_arm(0xF3B9_6562).id, ArmInstId::ASIMD_VCEQ_zero);
        assert_eq!(decode_arm(0xF3B9_4564).id, ArmInstId::ASIMD_VCEQ_zero);
        assert_eq!(decode_arm(0xF3B9_2566).id, ArmInstId::ASIMD_VCEQ_zero);
    }

    #[test]
    fn test_decode_vfp_vmov_from_i32_crash_opcode() {
        let dec = decode_arm(0xEE22_4B90);
        assert_eq!(dec.id, ArmInstId::VMOV_from_i32);
    }

    #[test]
    fn test_decode_vfp_vmov_from_i32_second_crash_opcode() {
        let dec = decode_arm(0xEE23_0B90);
        assert_eq!(dec.id, ArmInstId::VMOV_from_i32);
    }

    #[test]
    fn test_decode_vfp_vdup_crash_opcode() {
        let dec = decode_arm(0xEEA0_4B90);
        assert_eq!(dec.id, ArmInstId::VFP_VDUP);
    }

    #[test]
    fn test_decode_vfp_vsel_f32_eq_variant() {
        let dec = decode_arm(0xFE01_1A00);
        assert_eq!(dec.id, ArmInstId::VSEL_fp);
    }

    #[test]
    fn test_decode_dmb_ish() {
        let dec = decode_arm(0xF57F_F05B);
        assert_eq!(dec.id, ArmInstId::DMB);
    }

    #[test]
    fn test_decode_ldrexb() {
        // LDREXB R0, [R1]
        let dec = decode_arm(0xE1D1_0F9F);
        assert_eq!(dec.id, ArmInstId::LDREXB);
    }

    #[test]
    fn test_decode_strexb() {
        // STREXB R0, R2, [R1]
        let dec = decode_arm(0xE1C1_0F92);
        assert_eq!(dec.id, ArmInstId::STREXB);
    }

    #[test]
    fn test_decode_stlb() {
        // STLB R1, [R7]
        let dec = decode_arm(0xE1C7_FC91);
        assert_eq!(dec.id, ArmInstId::STLB);
    }

    #[test]
    fn test_decode_stlh() {
        // STLH R1, [R0]
        let dec = decode_arm(0xE1E0_FC91);
        assert_eq!(dec.id, ArmInstId::STLH);
    }

    #[test]
    fn test_decode_swp() {
        // SWP R1, R2, [R3]: cond=AL 0001 0000 Rn=0011 Rt=0001 0000 1001 Rt2=0010
        let dec = decode_arm(0xE103_1092);
        assert_eq!(dec.id, ArmInstId::SWP);
    }

    #[test]
    fn test_decode_swpb() {
        // SWPB R1, R2, [R3]: cond=AL 0001 0100 Rn=0011 Rt=0001 0000 1001 Rt2=0010
        let dec = decode_arm(0xE143_1092);
        assert_eq!(dec.id, ArmInstId::SWPB);
    }

    #[test]
    fn test_umull_not_confused_with_ldrex() {
        // UMULL R4, R3, R2, R1: cond=AL 0000 1000 RdHi=0011 RdLo=0100 Rm=0010 1001 Rn=0001
        // bits[27:24] = 0000, not 0001 — should be UMULL, not LDREX
        let dec = decode_arm(0xE083_4291);
        assert_eq!(dec.id, ArmInstId::UMULL);
    }
}
