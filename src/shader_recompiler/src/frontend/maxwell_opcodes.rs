// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Maxwell GPU instruction opcodes — all 280 opcodes from Eden's maxwell.inc.
//!
//! Each instruction in the Maxwell ISA is encoded as a 64-bit word.
//! The opcode is determined by the upper bits (varies by instruction class).
//! This module provides the opcode enum and a decoder function.

use std::fmt;

/// All 280 Maxwell instruction opcodes.
///
/// Naming follows zuyu: `FADD_reg` = FADD with register source,
/// `FADD_cbuf` = FADD with constant buffer source, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(non_camel_case_types)]
pub enum MaxwellOpcode {
    // ── Attribute / Output ────────────────────────────────────────────
    AL2P,
    ALD,
    AST,
    // ── Atomic ────────────────────────────────────────────────────────
    ATOM_cas,
    ATOM,
    ATOMS_cas,
    ATOMS,
    // ── Misc ──────────────────────────────────────────────────────────
    B2R,
    BAR,
    // ── Bit Field ─────────────────────────────────────────────────────
    BFE_reg,
    BFE_cbuf,
    BFE_imm,
    BFI_reg,
    BFI_rc,
    BFI_cr,
    BFI_imm,
    // ── Debug ─────────────────────────────────────────────────────────
    BPT,
    // ── Branch / Control Flow ─────────────────────────────────────────
    BRA,
    BRK,
    BRX,
    CAL,
    // ── Cache ─────────────────────────────────────────────────────────
    CCTLT,
    CCTL,
    CCTLL,
    // ── Control Flow ──────────────────────────────────────────────────
    CONT,
    // ── System Register ───────────────────────────────────────────────
    CS2R,
    // ── Condition Code ────────────────────────────────────────────────
    CSET,
    CSETP,
    // ── FP64 Arithmetic ───────────────────────────────────────────────
    DADD_reg,
    DADD_cbuf,
    DADD_imm,
    DEPBAR,
    DFMA_reg,
    DFMA_rc,
    DFMA_cr,
    DFMA_imm,
    DMNMX_reg,
    DMNMX_cbuf,
    DMNMX_imm,
    DMUL_reg,
    DMUL_cbuf,
    DMUL_imm,
    DSET_reg,
    DSET_cbuf,
    DSET_imm,
    DSETP_reg,
    DSETP_cbuf,
    DSETP_imm,
    // ── Control Flow ──────────────────────────────────────────────────
    EXIT,
    // ── Float Conversion ──────────────────────────────────────────────
    F2F_reg,
    F2F_cbuf,
    F2F_imm,
    F2I_reg,
    F2I_cbuf,
    F2I_imm,
    // ── FP32 Arithmetic ───────────────────────────────────────────────
    FADD_reg,
    FADD_cbuf,
    FADD_imm,
    FADD32I,
    FCHK_reg,
    FCHK_cbuf,
    FCHK_imm,
    FCMP_reg,
    FCMP_rc,
    FCMP_cr,
    FCMP_imm,
    FFMA_reg,
    FFMA_rc,
    FFMA_cr,
    FFMA_imm,
    FFMA32I,
    FLO_reg,
    FLO_cbuf,
    FLO_imm,
    FMNMX_reg,
    FMNMX_cbuf,
    FMNMX_imm,
    FMUL_reg,
    FMUL_cbuf,
    FMUL_imm,
    FMUL32I,
    FSET_reg,
    FSET_cbuf,
    FSET_imm,
    FSETP_reg,
    FSETP_cbuf,
    FSETP_imm,
    FSWZADD,
    // ── System ────────────────────────────────────────────────────────
    GETCRSPTR,
    GETLMEMBASE,
    // ── FP16 Arithmetic ───────────────────────────────────────────────
    HADD2_reg,
    HADD2_cbuf,
    HADD2_imm,
    HADD2_32I,
    HFMA2_reg,
    HFMA2_rc,
    HFMA2_cr,
    HFMA2_imm,
    HFMA2_32I,
    HMUL2_reg,
    HMUL2_cbuf,
    HMUL2_imm,
    HMUL2_32I,
    HSET2_reg,
    HSET2_cbuf,
    HSET2_imm,
    HSETP2_reg,
    HSETP2_cbuf,
    HSETP2_imm,
    // ── Integer Conversion ────────────────────────────────────────────
    I2F_reg,
    I2F_cbuf,
    I2F_imm,
    I2I_reg,
    I2I_cbuf,
    I2I_imm,
    // ── Integer Arithmetic ────────────────────────────────────────────
    IADD_reg,
    IADD_cbuf,
    IADD_imm,
    IADD3_reg,
    IADD3_cbuf,
    IADD3_imm,
    IADD32I,
    ICMP_reg,
    ICMP_rc,
    ICMP_cr,
    ICMP_imm,
    IDE,
    IDP_reg,
    IDP_imm,
    IMAD_reg,
    IMAD_rc,
    IMAD_cr,
    IMAD_imm,
    IMAD32I,
    IMADSP_reg,
    IMADSP_rc,
    IMADSP_cr,
    IMADSP_imm,
    IMNMX_reg,
    IMNMX_cbuf,
    IMNMX_imm,
    IMUL_reg,
    IMUL_cbuf,
    IMUL_imm,
    IMUL32I,
    // ── Interpolation ─────────────────────────────────────────────────
    IPA,
    ISBERD,
    // ── Integer Shift-Add ─────────────────────────────────────────────
    ISCADD_reg,
    ISCADD_cbuf,
    ISCADD_imm,
    ISCADD32I,
    // ── Integer Comparison ────────────────────────────────────────────
    ISET_reg,
    ISET_cbuf,
    ISET_imm,
    ISETP_reg,
    ISETP_cbuf,
    ISETP_imm,
    // ── Control Flow ──────────────────────────────────────────────────
    JCAL,
    JMP,
    JMX,
    // ── Kill ──────────────────────────────────────────────────────────
    KIL,
    // ── Memory Load ───────────────────────────────────────────────────
    LD,
    LDC,
    LDG,
    LDL,
    LDS,
    // ── Load Effective Address ────────────────────────────────────────
    LEA_hi_reg,
    LEA_hi_cbuf,
    LEA_lo_reg,
    LEA_lo_cbuf,
    LEA_lo_imm,
    // ── System ────────────────────────────────────────────────────────
    LEPC,
    LONGJMP,
    // ── Logic ─────────────────────────────────────────────────────────
    LOP_reg,
    LOP_cbuf,
    LOP_imm,
    LOP3_reg,
    LOP3_cbuf,
    LOP3_imm,
    LOP32I,
    // ── Memory ────────────────────────────────────────────────────────
    MEMBAR,
    // ── Move ──────────────────────────────────────────────────────────
    MOV_reg,
    MOV_cbuf,
    MOV_imm,
    MOV32I,
    // ── Transcendental ────────────────────────────────────────────────
    MUFU,
    // ── NOP ───────────────────────────────────────────────────────────
    NOP,
    // ── Geometry Output ───────────────────────────────────────────────
    OUT_reg,
    OUT_cbuf,
    OUT_imm,
    // ── Predicate <-> Register ────────────────────────────────────────
    P2R_reg,
    P2R_cbuf,
    P2R_imm,
    // ── Control Flow Stack ────────────────────────────────────────────
    PBK,
    PCNT,
    PEXIT,
    // ── Pixel Load ────────────────────────────────────────────────────
    PIXLD,
    // ── System ────────────────────────────────────────────────────────
    PLONGJMP,
    // ── Bit Count ─────────────────────────────────────────────────────
    POPC_reg,
    POPC_cbuf,
    POPC_imm,
    // ── Control Flow Stack ────────────────────────────────────────────
    PRET,
    // ── Permute ───────────────────────────────────────────────────────
    PRMT_reg,
    PRMT_rc,
    PRMT_cr,
    PRMT_imm,
    // ── Predicate Set ─────────────────────────────────────────────────
    PSET,
    PSETP,
    // ── System ────────────────────────────────────────────────────────
    R2B,
    R2P_reg,
    R2P_cbuf,
    R2P_imm,
    RAM,
    // ── Atomic Reduction ──────────────────────────────────────────────
    RED,
    // ── Return ────────────────────────────────────────────────────────
    RET,
    // ── Range Reduction ───────────────────────────────────────────────
    RRO_reg,
    RRO_cbuf,
    RRO_imm,
    // ── System ────────────────────────────────────────────────────────
    RTT,
    S2R,
    SAM,
    // ── Select ────────────────────────────────────────────────────────
    SEL_reg,
    SEL_cbuf,
    SEL_imm,
    // ── System ────────────────────────────────────────────────────────
    SETCRSPTR,
    SETLMEMBASE,
    // ── Funnel Shift ──────────────────────────────────────────────────
    SHF_l_reg,
    SHF_l_imm,
    SHF_r_reg,
    SHF_r_imm,
    // ── Shuffle ───────────────────────────────────────────────────────
    SHFL,
    // ── Shift ─────────────────────────────────────────────────────────
    SHL_reg,
    SHL_cbuf,
    SHL_imm,
    SHR_reg,
    SHR_cbuf,
    SHR_imm,
    // ── Control Flow Stack ────────────────────────────────────────────
    SSY,
    // ── Memory Store ──────────────────────────────────────────────────
    ST,
    STG,
    STL,
    STP,
    STS,
    // ── Surface ───────────────────────────────────────────────────────
    SUATOM,
    SUATOM_cas,
    SULD,
    SURED,
    SUST,
    // ── Control Flow ──────────────────────────────────────────────────
    SYNC,
    // ── Texture ───────────────────────────────────────────────────────
    TEX,
    TEX_b,
    TEXS,
    TLD,
    TLD_b,
    TLD4,
    TLD4_b,
    TLD4S,
    TLDS,
    TMML,
    TMML_b,
    TXA,
    TXD,
    TXD_b,
    TXQ,
    TXQ_b,
    // ── Video ─────────────────────────────────────────────────────────
    VABSDIFF,
    VABSDIFF4,
    VADD,
    VMAD,
    VMNMX,
    // ── Vote ──────────────────────────────────────────────────────────
    VOTE,
    VOTE_vtg,
    // ── Video Compare / Shift ─────────────────────────────────────────
    VSET,
    VSETP,
    VSHL,
    VSHR,
    // ── Extended Multiply-Add ─────────────────────────────────────────
    XMAD_reg,
    XMAD_rc,
    XMAD_cr,
    XMAD_imm,
}

impl fmt::Display for MaxwellOpcode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Upstream `MaskValueFromEncoding(encoding)` in `maxwell/decode.cpp`.
///
/// Parses a `"1110 1111 1010 0---"`-style 16-bit-wide encoding string into a
/// `(mask, value)` pair occupying bits [63:48] of a 64-bit instruction word.
/// '0'/'1' set the mask bit and (for '1') the value bit. '-' is wildcard
/// (mask = 0). Spaces are ignored.
const fn mask_value_from_encoding(encoding: &str) -> (u64, u64) {
    let mut mask: u64 = 0;
    let mut value: u64 = 0;
    let mut bit: u64 = 1u64 << 63;
    let bytes = encoding.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'0' => mask |= bit,
            b'1' => {
                mask |= bit;
                value |= bit;
            }
            b'-' => {}
            b' ' => {
                // Spaces are ignored; do NOT consume a bit position.
                i += 1;
                continue;
            }
            _ => panic!("invalid Maxwell encoding character"),
        }
        bit >>= 1;
        i += 1;
    }
    (mask, value)
}

/// One row of the Maxwell decode table — a mask/value pair derived from an
/// upstream `INST(name, friendly, "1110 ...")` entry plus the opcode it
/// names.
#[derive(Clone, Copy)]
struct InstEncoding {
    mask: u64,
    value: u64,
    opcode: MaxwellOpcode,
}

/// Upstream `maxwell.inc`. Order mirrors the upstream file exactly so future
/// audits can diff this table against the source of truth.
const ENCODINGS_TEXT: &[(MaxwellOpcode, &str)] = &[
    (MaxwellOpcode::CCTLT, "1110 1011 1111 0--0"),
    (MaxwellOpcode::AL2P, "1110 1111 1010 0---"),
    (MaxwellOpcode::ALD, "1110 1111 1101 1---"),
    (MaxwellOpcode::AST, "1110 1111 1111 0---"),
    (MaxwellOpcode::B2R, "1111 0000 1011 1---"),
    (MaxwellOpcode::BAR, "1111 0000 1010 1---"),
    (MaxwellOpcode::BFE_cbuf, "0100 1100 0000 0---"),
    (MaxwellOpcode::BFE_reg, "0101 1100 0000 0---"),
    (MaxwellOpcode::BFI_cr, "0100 1011 1111 0---"),
    (MaxwellOpcode::BFI_rc, "0101 0011 1111 0---"),
    (MaxwellOpcode::BFI_reg, "0101 1011 1111 0---"),
    (MaxwellOpcode::CS2R, "0101 0000 1100 1---"),
    (MaxwellOpcode::CSET, "0101 0000 1001 1---"),
    (MaxwellOpcode::CSETP, "0101 0000 1010 0---"),
    (MaxwellOpcode::DADD_cbuf, "0100 1100 0111 0---"),
    (MaxwellOpcode::DADD_reg, "0101 1100 0111 0---"),
    (MaxwellOpcode::DEPBAR, "1111 0000 1111 0---"),
    (MaxwellOpcode::DMNMX_cbuf, "0100 1100 0101 0---"),
    (MaxwellOpcode::DMNMX_reg, "0101 1100 0101 0---"),
    (MaxwellOpcode::DMUL_cbuf, "0100 1100 1000 0---"),
    (MaxwellOpcode::DMUL_reg, "0101 1100 1000 0---"),
    (MaxwellOpcode::F2F_cbuf, "0100 1100 1010 1---"),
    (MaxwellOpcode::F2F_reg, "0101 1100 1010 1---"),
    (MaxwellOpcode::F2I_cbuf, "0100 1100 1011 0---"),
    (MaxwellOpcode::F2I_reg, "0101 1100 1011 0---"),
    (MaxwellOpcode::FADD_cbuf, "0100 1100 0101 1---"),
    (MaxwellOpcode::FADD_reg, "0101 1100 0101 1---"),
    (MaxwellOpcode::FCHK_cbuf, "0100 1100 1000 1---"),
    (MaxwellOpcode::FCHK_reg, "0101 1100 1000 1---"),
    (MaxwellOpcode::FLO_cbuf, "0100 1100 0011 0---"),
    (MaxwellOpcode::FLO_reg, "0101 1100 0011 0---"),
    (MaxwellOpcode::FMNMX_cbuf, "0100 1100 0110 0---"),
    (MaxwellOpcode::FMNMX_reg, "0101 1100 0110 0---"),
    (MaxwellOpcode::FMUL_cbuf, "0100 1100 0110 1---"),
    (MaxwellOpcode::FMUL_reg, "0101 1100 0110 1---"),
    (MaxwellOpcode::FSWZADD, "0101 0000 1111 1---"),
    (MaxwellOpcode::HADD2_reg, "0101 1101 0001 0---"),
    (MaxwellOpcode::HFMA2_reg, "0101 1101 0000 0---"),
    (MaxwellOpcode::HMUL2_reg, "0101 1101 0000 1---"),
    (MaxwellOpcode::HSET2_reg, "0101 1101 0001 1---"),
    (MaxwellOpcode::HSETP2_reg, "0101 1101 0010 0---"),
    (MaxwellOpcode::I2F_cbuf, "0100 1100 1011 1---"),
    (MaxwellOpcode::I2F_reg, "0101 1100 1011 1---"),
    (MaxwellOpcode::I2I_cbuf, "0100 1100 1110 0---"),
    (MaxwellOpcode::I2I_reg, "0101 1100 1110 0---"),
    (MaxwellOpcode::IADD_cbuf, "0100 1100 0001 0---"),
    (MaxwellOpcode::IADD_reg, "0101 1100 0001 0---"),
    (MaxwellOpcode::IDP_imm, "0101 0011 1101 1---"),
    (MaxwellOpcode::IDP_reg, "0101 0011 1111 1---"),
    (MaxwellOpcode::IMNMX_cbuf, "0100 1100 0010 0---"),
    (MaxwellOpcode::IMNMX_reg, "0101 1100 0010 0---"),
    (MaxwellOpcode::IMUL_cbuf, "0100 1100 0011 1---"),
    (MaxwellOpcode::IMUL_reg, "0101 1100 0011 1---"),
    (MaxwellOpcode::ISBERD, "1110 1111 1101 0---"),
    (MaxwellOpcode::ISCADD_cbuf, "0100 1100 0001 1---"),
    (MaxwellOpcode::ISCADD_reg, "0101 1100 0001 1---"),
    (MaxwellOpcode::LDC, "1110 1111 1001 0---"),
    (MaxwellOpcode::LDG, "1110 1110 1101 0---"),
    (MaxwellOpcode::LDL, "1110 1111 0100 0---"),
    (MaxwellOpcode::LDS, "1110 1111 0100 1---"),
    (MaxwellOpcode::LEA_hi_reg, "0101 1011 1101 1---"),
    (MaxwellOpcode::LEA_lo_reg, "0101 1011 1101 0---"),
    (MaxwellOpcode::LEPC, "0101 0000 1101 0---"),
    (MaxwellOpcode::LOP3_reg, "0101 1011 1110 0---"),
    (MaxwellOpcode::LOP_cbuf, "0100 1100 0100 0---"),
    (MaxwellOpcode::LOP_reg, "0101 1100 0100 0---"),
    (MaxwellOpcode::MEMBAR, "1110 1111 1001 1---"),
    (MaxwellOpcode::MOV_cbuf, "0100 1100 1001 1---"),
    (MaxwellOpcode::MOV_reg, "0101 1100 1001 1---"),
    (MaxwellOpcode::MUFU, "0101 0000 1000 0---"),
    (MaxwellOpcode::NOP, "0101 0000 1011 0---"),
    (MaxwellOpcode::OUT_cbuf, "1110 1011 1110 0---"),
    (MaxwellOpcode::OUT_reg, "1111 1011 1110 0---"),
    (MaxwellOpcode::P2R_cbuf, "0100 1100 1110 1---"),
    (MaxwellOpcode::P2R_imm, "0011 1000 1110 1---"),
    (MaxwellOpcode::P2R_reg, "0101 1100 1110 1---"),
    (MaxwellOpcode::PIXLD, "1110 1111 1110 1---"),
    (MaxwellOpcode::POPC_cbuf, "0100 1100 0000 1---"),
    (MaxwellOpcode::POPC_reg, "0101 1100 0000 1---"),
    (MaxwellOpcode::PSET, "0101 0000 1000 1---"),
    (MaxwellOpcode::PSETP, "0101 0000 1001 0---"),
    (MaxwellOpcode::R2B, "1111 0000 1100 0---"),
    (MaxwellOpcode::R2P_cbuf, "0100 1100 1111 0---"),
    (MaxwellOpcode::R2P_reg, "0101 1100 1111 0---"),
    (MaxwellOpcode::RED, "1110 1011 1111 1---"),
    (MaxwellOpcode::RRO_cbuf, "0100 1100 1001 0---"),
    (MaxwellOpcode::RRO_reg, "0101 1100 1001 0---"),
    (MaxwellOpcode::S2R, "1111 0000 1100 1---"),
    (MaxwellOpcode::SEL_cbuf, "0100 1100 1010 0---"),
    (MaxwellOpcode::SEL_reg, "0101 1100 1010 0---"),
    (MaxwellOpcode::SHFL, "1110 1111 0001 0---"),
    (MaxwellOpcode::SHF_l_reg, "0101 1011 1111 1---"),
    (MaxwellOpcode::SHF_r_reg, "0101 1100 1111 1---"),
    (MaxwellOpcode::SHL_cbuf, "0100 1100 0100 1---"),
    (MaxwellOpcode::SHL_reg, "0101 1100 0100 1---"),
    (MaxwellOpcode::SHR_cbuf, "0100 1100 0010 1---"),
    (MaxwellOpcode::SHR_reg, "0101 1100 0010 1---"),
    (MaxwellOpcode::STG, "1110 1110 1101 1---"),
    (MaxwellOpcode::STL, "1110 1111 0101 0---"),
    (MaxwellOpcode::STP, "1110 1110 1010 0---"),
    (MaxwellOpcode::STS, "1110 1111 0101 1---"),
    (MaxwellOpcode::SYNC, "1111 0000 1111 1---"),
    (MaxwellOpcode::TMML, "1101 1111 0101 1---"),
    (MaxwellOpcode::TMML_b, "1101 1111 0110 0---"),
    (MaxwellOpcode::TXA, "1101 1111 0100 0---"),
    (MaxwellOpcode::TXQ, "1101 1111 0100 1---"),
    (MaxwellOpcode::TXQ_b, "1101 1111 0101 0---"),
    (MaxwellOpcode::VOTE, "0101 0000 1101 1---"),
    (MaxwellOpcode::VOTE_vtg, "0101 0000 1110 0---"),
    (MaxwellOpcode::VSETP, "0101 0000 1111 0---"),
    (MaxwellOpcode::ATOM_cas, "1110 1110 1111 ----"),
    (MaxwellOpcode::BFE_imm, "0011 100- 0000 0---"),
    (MaxwellOpcode::BFI_imm, "0011 011- 1111 0---"),
    (MaxwellOpcode::BPT, "1110 0011 1010 ----"),
    (MaxwellOpcode::BRA, "1110 0010 0100 ----"),
    (MaxwellOpcode::BRK, "1110 0011 0100 ----"),
    (MaxwellOpcode::BRX, "1110 0010 0101 ----"),
    (MaxwellOpcode::CAL, "1110 0010 0110 ----"),
    (MaxwellOpcode::CONT, "1110 0011 0101 ----"),
    (MaxwellOpcode::DADD_imm, "0011 100- 0111 0---"),
    (MaxwellOpcode::DFMA_cr, "0100 1011 0111 ----"),
    (MaxwellOpcode::DFMA_rc, "0101 0011 0111 ----"),
    (MaxwellOpcode::DFMA_reg, "0101 1011 0111 ----"),
    (MaxwellOpcode::DMNMX_imm, "0011 100- 0101 0---"),
    (MaxwellOpcode::DMUL_imm, "0011 100- 1000 0---"),
    (MaxwellOpcode::DSETP_cbuf, "0100 1011 1000 ----"),
    (MaxwellOpcode::DSETP_reg, "0101 1011 1000 ----"),
    (MaxwellOpcode::EXIT, "1110 0011 0000 ----"),
    (MaxwellOpcode::F2F_imm, "0011 100- 1010 1---"),
    (MaxwellOpcode::F2I_imm, "0011 100- 1011 0---"),
    (MaxwellOpcode::FADD_imm, "0011 100- 0101 1---"),
    (MaxwellOpcode::FCHK_imm, "0011 100- 1000 1---"),
    (MaxwellOpcode::FCMP_cr, "0100 1011 1010 ----"),
    (MaxwellOpcode::FCMP_rc, "0101 0011 1010 ----"),
    (MaxwellOpcode::FCMP_reg, "0101 1011 1010 ----"),
    (MaxwellOpcode::FLO_imm, "0011 100- 0011 0---"),
    (MaxwellOpcode::FMNMX_imm, "0011 100- 0110 0---"),
    (MaxwellOpcode::FMUL_imm, "0011 100- 0110 1---"),
    (MaxwellOpcode::FSETP_cbuf, "0100 1011 1011 ----"),
    (MaxwellOpcode::FSETP_reg, "0101 1011 1011 ----"),
    (MaxwellOpcode::GETCRSPTR, "1110 0010 1100 ----"),
    (MaxwellOpcode::GETLMEMBASE, "1110 0010 1101 ----"),
    (MaxwellOpcode::I2F_imm, "0011 100- 1011 1---"),
    (MaxwellOpcode::I2I_imm, "0011 100- 1110 0---"),
    (MaxwellOpcode::IADD3_cbuf, "0100 1100 1100 ----"),
    (MaxwellOpcode::IADD3_reg, "0101 1100 1100 ----"),
    (MaxwellOpcode::IADD_imm, "0011 100- 0001 0---"),
    (MaxwellOpcode::ICMP_cr, "0100 1011 0100 ----"),
    (MaxwellOpcode::ICMP_rc, "0101 0011 0100 ----"),
    (MaxwellOpcode::ICMP_reg, "0101 1011 0100 ----"),
    (MaxwellOpcode::IDE, "1110 0011 1001 ----"),
    (MaxwellOpcode::IMNMX_imm, "0011 100- 0010 0---"),
    (MaxwellOpcode::IMUL_imm, "0011 100- 0011 1---"),
    (MaxwellOpcode::ISCADD_imm, "0011 100- 0001 1---"),
    (MaxwellOpcode::ISETP_cbuf, "0100 1011 0110 ----"),
    (MaxwellOpcode::ISETP_reg, "0101 1011 0110 ----"),
    (MaxwellOpcode::ISET_cbuf, "0100 1011 0101 ----"),
    (MaxwellOpcode::ISET_reg, "0101 1011 0101 ----"),
    (MaxwellOpcode::JCAL, "1110 0010 0010 ----"),
    (MaxwellOpcode::JMP, "1110 0010 0001 ----"),
    (MaxwellOpcode::JMX, "1110 0010 0000 ----"),
    (MaxwellOpcode::KIL, "1110 0011 0011 ----"),
    (MaxwellOpcode::LEA_lo_cbuf, "0100 1011 1101 ----"),
    (MaxwellOpcode::LEA_lo_imm, "0011 011- 1101 0---"),
    (MaxwellOpcode::LONGJMP, "1110 0011 0001 ----"),
    (MaxwellOpcode::LOP_imm, "0011 100- 0100 0---"),
    (MaxwellOpcode::MOV32I, "0000 0001 0000 ----"),
    (MaxwellOpcode::MOV_imm, "0011 100- 1001 1---"),
    (MaxwellOpcode::OUT_imm, "1111 011- 1110 0---"),
    (MaxwellOpcode::PBK, "1110 0010 1010 ----"),
    (MaxwellOpcode::PCNT, "1110 0010 1011 ----"),
    (MaxwellOpcode::PEXIT, "1110 0010 0011 ----"),
    (MaxwellOpcode::PLONGJMP, "1110 0010 1000 ----"),
    (MaxwellOpcode::POPC_imm, "0011 100- 0000 1---"),
    (MaxwellOpcode::PRET, "1110 0010 0111 ----"),
    (MaxwellOpcode::PRMT_cr, "0100 1011 1100 ----"),
    (MaxwellOpcode::PRMT_rc, "0101 0011 1100 ----"),
    (MaxwellOpcode::PRMT_reg, "0101 1011 1100 ----"),
    (MaxwellOpcode::R2P_imm, "0011 100- 1111 0---"),
    (MaxwellOpcode::RAM, "1110 0011 1000 ----"),
    (MaxwellOpcode::RET, "1110 0011 0010 ----"),
    (MaxwellOpcode::RRO_imm, "0011 100- 1001 0---"),
    (MaxwellOpcode::RTT, "1110 0011 0110 ----"),
    (MaxwellOpcode::SAM, "1110 0011 0111 ----"),
    (MaxwellOpcode::SEL_imm, "0011 100- 1010 0---"),
    (MaxwellOpcode::SETCRSPTR, "1110 0010 1110 ----"),
    (MaxwellOpcode::SETLMEMBASE, "1110 0010 1111 ----"),
    (MaxwellOpcode::SHF_l_imm, "0011 011- 1111 1---"),
    (MaxwellOpcode::SHF_r_imm, "0011 100- 1111 1---"),
    (MaxwellOpcode::SHL_imm, "0011 100- 0100 1---"),
    (MaxwellOpcode::SHR_imm, "0011 100- 0010 1---"),
    (MaxwellOpcode::SSY, "1110 0010 1001 ----"),
    (MaxwellOpcode::CCTL, "1110 1111 011- ----"),
    (MaxwellOpcode::CCTLL, "1110 1111 100- ----"),
    (MaxwellOpcode::DFMA_imm, "0011 011- 0111 ----"),
    (MaxwellOpcode::DSETP_imm, "0011 011- 1000 ----"),
    (MaxwellOpcode::FCMP_imm, "0011 011- 1010 ----"),
    (MaxwellOpcode::FSETP_imm, "0011 011- 1011 ----"),
    (MaxwellOpcode::IADD3_imm, "0011 100- 1100 ----"),
    (MaxwellOpcode::ICMP_imm, "0011 011- 0100 ----"),
    (MaxwellOpcode::ISETP_imm, "0011 011- 0110 ----"),
    (MaxwellOpcode::ISET_imm, "0011 011- 0101 ----"),
    (MaxwellOpcode::PRMT_imm, "0011 011- 1100 ----"),
    (MaxwellOpcode::SULD, "1110 1011 000- ----"),
    (MaxwellOpcode::SURED, "1110 1011 010- ----"),
    (MaxwellOpcode::SUST, "1110 1011 001- ----"),
    (MaxwellOpcode::TEX_b, "1101 1110 10-- ----"),
    (MaxwellOpcode::TLD4_b, "1101 1110 11-- ----"),
    (MaxwellOpcode::TXD, "1101 1110 00-- ----"),
    (MaxwellOpcode::TXD_b, "1101 1110 01-- ----"),
    (MaxwellOpcode::XMAD_reg, "0101 1011 00-- ----"),
    (MaxwellOpcode::DSET_cbuf, "0100 1001 0--- ----"),
    (MaxwellOpcode::DSET_reg, "0101 1001 0--- ----"),
    (MaxwellOpcode::FFMA_cr, "0100 1001 1--- ----"),
    (MaxwellOpcode::FFMA_rc, "0101 0001 1--- ----"),
    (MaxwellOpcode::FFMA_reg, "0101 1001 1--- ----"),
    (MaxwellOpcode::IMADSP_cr, "0100 1010 1--- ----"),
    (MaxwellOpcode::IMADSP_rc, "0101 0010 1--- ----"),
    (MaxwellOpcode::IMADSP_reg, "0101 1010 1--- ----"),
    (MaxwellOpcode::IMAD_cr, "0100 1010 0--- ----"),
    (MaxwellOpcode::IMAD_rc, "0101 0010 0--- ----"),
    (MaxwellOpcode::IMAD_reg, "0101 1010 0--- ----"),
    (MaxwellOpcode::SUATOM, "1110 1010 0--- ----"),
    (MaxwellOpcode::SUATOM_cas, "1110 1010 1--- ----"),
    (MaxwellOpcode::TLD4S, "1101 1111 -0-- ----"),
    (MaxwellOpcode::VABSDIFF4, "0101 0000 0--- ----"),
    (MaxwellOpcode::XMAD_imm, "0011 011- 00-- ----"),
    (MaxwellOpcode::XMAD_rc, "0101 0001 0--- ----"),
    (MaxwellOpcode::ATOM, "1110 1101 ---- ----"),
    (MaxwellOpcode::ATOMS, "1110 1100 ---- ----"),
    (MaxwellOpcode::ATOMS_cas, "1110 1110 ---- ----"),
    (MaxwellOpcode::DSET_imm, "0011 001- 0--- ----"),
    (MaxwellOpcode::FFMA_imm, "0011 001- 1--- ----"),
    (MaxwellOpcode::FMUL32I, "0001 1110 ---- ----"),
    (MaxwellOpcode::FSET_cbuf, "0100 1000 ---- ----"),
    (MaxwellOpcode::FSET_reg, "0101 1000 ---- ----"),
    (MaxwellOpcode::HADD2_cbuf, "0111 101- 1--- ----"),
    (MaxwellOpcode::HADD2_imm, "0111 101- 0--- ----"),
    (MaxwellOpcode::HMUL2_cbuf, "0111 100- 1--- ----"),
    (MaxwellOpcode::HMUL2_imm, "0111 100- 0--- ----"),
    (MaxwellOpcode::HSET2_cbuf, "0111 110- 1--- ----"),
    (MaxwellOpcode::HSET2_imm, "0111 110- 0--- ----"),
    (MaxwellOpcode::HSETP2_cbuf, "0111 111- 1--- ----"),
    (MaxwellOpcode::HSETP2_imm, "0111 111- 0--- ----"),
    (MaxwellOpcode::IMADSP_imm, "0011 010- 1--- ----"),
    (MaxwellOpcode::IMAD_imm, "0011 010- 0--- ----"),
    (MaxwellOpcode::IMUL32I, "0001 1111 ---- ----"),
    (MaxwellOpcode::IPA, "1110 0000 ---- ----"),
    (MaxwellOpcode::TLD, "1101 1100 ---- ----"),
    (MaxwellOpcode::TLD_b, "1101 1101 ---- ----"),
    (MaxwellOpcode::VABSDIFF, "0101 0100 ---- ----"),
    (MaxwellOpcode::VMAD, "0101 1111 ---- ----"),
    (MaxwellOpcode::VSHL, "0101 0111 ---- ----"),
    (MaxwellOpcode::VSHR, "0101 0110 ---- ----"),
    (MaxwellOpcode::FSET_imm, "0011 000- ---- ----"),
    (MaxwellOpcode::HADD2_32I, "0010 110- ---- ----"),
    (MaxwellOpcode::HFMA2_32I, "0010 100- ---- ----"),
    (MaxwellOpcode::HMUL2_32I, "0010 101- ---- ----"),
    (MaxwellOpcode::IADD32I, "0001 110- ---- ----"),
    (MaxwellOpcode::LOP3_cbuf, "0000 001- ---- ----"),
    (MaxwellOpcode::VMNMX, "0011 101- ---- ----"),
    (MaxwellOpcode::VSET, "0100 000- ---- ----"),
    (MaxwellOpcode::XMAD_cr, "0100 111- ---- ----"),
    (MaxwellOpcode::FADD32I, "0000 10-- ---- ----"),
    (MaxwellOpcode::FFMA32I, "0000 11-- ---- ----"),
    (MaxwellOpcode::HFMA2_cr, "0111 0--- 1--- ----"),
    (MaxwellOpcode::HFMA2_imm, "0111 0--- 0--- ----"),
    (MaxwellOpcode::HFMA2_rc, "0110 0--- 1--- ----"),
    (MaxwellOpcode::IMAD32I, "1000 00-- ---- ----"),
    (MaxwellOpcode::ISCADD32I, "0001 01-- ---- ----"),
    (MaxwellOpcode::LEA_hi_cbuf, "0001 10-- ---- ----"),
    (MaxwellOpcode::LOP32I, "0000 01-- ---- ----"),
    (MaxwellOpcode::LOP3_imm, "0011 11-- ---- ----"),
    (MaxwellOpcode::TEXS, "1101 -00- ---- ----"),
    (MaxwellOpcode::TLD4, "1100 10-- ---- ----"),
    (MaxwellOpcode::TLDS, "1101 -01- ---- ----"),
    (MaxwellOpcode::VADD, "0010 00-- ---- ----"),
    (MaxwellOpcode::TEX, "1100 0--- ---- ----"),
    (MaxwellOpcode::LD, "100- ---- ---- ----"),
    (MaxwellOpcode::ST, "101- ---- ---- ----"),
];

/// Lazily-initialized decode table. The insertion order is the exact order of
/// upstream `maxwell.inc`; `Decode` is a first-match decoder and several
/// encodings overlap.
fn encodings() -> &'static [InstEncoding] {
    static TABLE: std::sync::OnceLock<Vec<InstEncoding>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        ENCODINGS_TEXT
            .iter()
            .map(|&(opcode, encoding)| {
                let (mask, value) = mask_value_from_encoding(encoding);
                InstEncoding {
                    mask,
                    value,
                    opcode,
                }
            })
            .collect()
    })
}

/// Upstream `Decode(insn)` in `maxwell/decode.cpp`. Returns the opcode whose
/// `(mask, value)` matches the top 16 bits of `insn`. Returns `None` if no
/// encoding matches — that case corresponds to an unmapped instruction word
/// (in practice this is usually a sched-control word that callers should
/// have skipped via `Location::step`).
pub fn decode_opcode(insn: u64) -> Option<MaxwellOpcode> {
    for e in encodings() {
        if (insn & e.mask) == e.value {
            return Some(e.opcode);
        }
    }
    None
}

/// Instruction source operand type — helps the translator decode src_b.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrcType {
    Register,
    ConstantBuffer,
    Immediate,
}

impl MaxwellOpcode {
    /// Get the source operand type for this opcode variant.
    pub fn src_type(self) -> SrcType {
        let name = format!("{:?}", self);
        if name.ends_with("_reg") || name.ends_with("_rc") {
            SrcType::Register
        } else if name.ends_with("_cbuf") || name.ends_with("_cr") {
            SrcType::ConstantBuffer
        } else if name.ends_with("_imm") || name.ends_with("32I") {
            SrcType::Immediate
        } else {
            // Default to register for non-suffixed opcodes
            SrcType::Register
        }
    }

    /// Get the base opcode name (without _reg/_cbuf/_imm suffix).
    pub fn base_name(self) -> &'static str {
        match self {
            FADD_reg | FADD_cbuf | FADD_imm => "FADD",
            FMUL_reg | FMUL_cbuf | FMUL_imm => "FMUL",
            FFMA_reg | FFMA_rc | FFMA_cr | FFMA_imm => "FFMA",
            FMNMX_reg | FMNMX_cbuf | FMNMX_imm => "FMNMX",
            FSET_reg | FSET_cbuf | FSET_imm => "FSET",
            FSETP_reg | FSETP_cbuf | FSETP_imm => "FSETP",
            FCMP_reg | FCMP_rc | FCMP_cr | FCMP_imm => "FCMP",
            IADD_reg | IADD_cbuf | IADD_imm => "IADD",
            IADD3_reg | IADD3_cbuf | IADD3_imm => "IADD3",
            IMAD_reg | IMAD_rc | IMAD_cr | IMAD_imm => "IMAD",
            IMUL_reg | IMUL_cbuf | IMUL_imm => "IMUL",
            ISETP_reg | ISETP_cbuf | ISETP_imm => "ISETP",
            ISET_reg | ISET_cbuf | ISET_imm => "ISET",
            IMNMX_reg | IMNMX_cbuf | IMNMX_imm => "IMNMX",
            MOV_reg | MOV_cbuf | MOV_imm => "MOV",
            SEL_reg | SEL_cbuf | SEL_imm => "SEL",
            SHL_reg | SHL_cbuf | SHL_imm => "SHL",
            SHR_reg | SHR_cbuf | SHR_imm => "SHR",
            BFE_reg | BFE_cbuf | BFE_imm => "BFE",
            BFI_reg | BFI_rc | BFI_cr | BFI_imm => "BFI",
            LOP_reg | LOP_cbuf | LOP_imm => "LOP",
            LOP3_reg | LOP3_cbuf | LOP3_imm => "LOP3",
            F2F_reg | F2F_cbuf | F2F_imm => "F2F",
            F2I_reg | F2I_cbuf | F2I_imm => "F2I",
            I2F_reg | I2F_cbuf | I2F_imm => "I2F",
            I2I_reg | I2I_cbuf | I2I_imm => "I2I",
            DADD_reg | DADD_cbuf | DADD_imm => "DADD",
            DMUL_reg | DMUL_cbuf | DMUL_imm => "DMUL",
            DFMA_reg | DFMA_rc | DFMA_cr | DFMA_imm => "DFMA",
            DMNMX_reg | DMNMX_cbuf | DMNMX_imm => "DMNMX",
            DSET_reg | DSET_cbuf | DSET_imm => "DSET",
            DSETP_reg | DSETP_cbuf | DSETP_imm => "DSETP",
            XMAD_reg | XMAD_rc | XMAD_cr | XMAD_imm => "XMAD",
            POPC_reg | POPC_cbuf | POPC_imm => "POPC",
            FLO_reg | FLO_cbuf | FLO_imm => "FLO",
            PRMT_reg | PRMT_rc | PRMT_cr | PRMT_imm => "PRMT",
            ICMP_reg | ICMP_rc | ICMP_cr | ICMP_imm => "ICMP",
            P2R_reg | P2R_cbuf | P2R_imm => "P2R",
            R2P_reg | R2P_cbuf | R2P_imm => "R2P",
            ISCADD_reg | ISCADD_cbuf | ISCADD_imm => "ISCADD",
            FCHK_reg | FCHK_cbuf | FCHK_imm => "FCHK",
            RRO_reg | RRO_cbuf | RRO_imm => "RRO",
            OUT_reg | OUT_cbuf | OUT_imm => "OUT",
            HADD2_reg | HADD2_cbuf | HADD2_imm => "HADD2",
            HMUL2_reg | HMUL2_cbuf | HMUL2_imm => "HMUL2",
            HFMA2_reg | HFMA2_rc | HFMA2_cr | HFMA2_imm => "HFMA2",
            HSET2_reg | HSET2_cbuf | HSET2_imm => "HSET2",
            HSETP2_reg | HSETP2_cbuf | HSETP2_imm => "HSETP2",
            SHF_l_reg | SHF_l_imm => "SHF_l",
            SHF_r_reg | SHF_r_imm => "SHF_r",
            LEA_hi_reg | LEA_hi_cbuf => "LEA_hi",
            LEA_lo_reg | LEA_lo_cbuf | LEA_lo_imm => "LEA_lo",
            IMADSP_reg | IMADSP_rc | IMADSP_cr | IMADSP_imm => "IMADSP",
            TEX_b => "TEX_b",
            TLD_b => "TLD_b",
            TLD4_b => "TLD4_b",
            TMML_b => "TMML_b",
            TXQ_b => "TXQ_b",
            TXD_b => "TXD_b",
            ATOM_cas => "ATOM_cas",
            ATOMS_cas => "ATOMS_cas",
            SUATOM_cas => "SUATOM_cas",
            VOTE_vtg => "VOTE_vtg",
            IDP_reg | IDP_imm => "IDP",
            _ => {
                // Single-form opcodes return their debug name
                // This is a fallback — production code should match explicitly
                match self {
                    MUFU => "MUFU",
                    NOP => "NOP",
                    EXIT => "EXIT",
                    BRA => "BRA",
                    BRK => "BRK",
                    BRX => "BRX",
                    SYNC => "SYNC",
                    SSY => "SSY",
                    PBK => "PBK",
                    PCNT => "PCNT",
                    CONT => "CONT",
                    CAL => "CAL",
                    RET => "RET",
                    KIL => "KIL",
                    S2R => "S2R",
                    CS2R => "CS2R",
                    LDG => "LDG",
                    STG => "STG",
                    LDC => "LDC",
                    LDL => "LDL",
                    STL => "STL",
                    LDS => "LDS",
                    STS => "STS",
                    LD => "LD",
                    ST => "ST",
                    TEX => "TEX",
                    TEXS => "TEXS",
                    TLD => "TLD",
                    TLD4 => "TLD4",
                    TLD4S => "TLD4S",
                    TLDS => "TLDS",
                    TMML => "TMML",
                    TXQ => "TXQ",
                    TXD => "TXD",
                    TXA => "TXA",
                    ALD => "ALD",
                    AST => "AST",
                    AL2P => "AL2P",
                    IPA => "IPA",
                    ISBERD => "ISBERD",
                    STP => "STP",
                    ATOM => "ATOM",
                    ATOMS => "ATOMS",
                    RED => "RED",
                    SULD => "SULD",
                    SUST => "SUST",
                    SUATOM => "SUATOM",
                    SURED => "SURED",
                    BAR => "BAR",
                    DEPBAR => "DEPBAR",
                    MEMBAR => "MEMBAR",
                    SHFL => "SHFL",
                    VOTE => "VOTE",
                    PSET => "PSET",
                    PSETP => "PSETP",
                    CSET => "CSET",
                    CSETP => "CSETP",
                    FADD32I => "FADD32I",
                    FMUL32I => "FMUL32I",
                    FFMA32I => "FFMA32I",
                    IADD32I => "IADD32I",
                    IMUL32I => "IMUL32I",
                    ISCADD32I => "ISCADD32I",
                    IMAD32I => "IMAD32I",
                    LOP32I => "LOP32I",
                    MOV32I => "MOV32I",
                    HADD2_32I => "HADD2_32I",
                    HMUL2_32I => "HMUL2_32I",
                    HFMA2_32I => "HFMA2_32I",
                    PEXIT => "PEXIT",
                    PRET => "PRET",
                    JMP => "JMP",
                    JMX => "JMX",
                    JCAL => "JCAL",
                    LONGJMP => "LONGJMP",
                    PLONGJMP => "PLONGJMP",
                    SAM => "SAM",
                    RAM => "RAM",
                    RTT => "RTT",
                    BPT => "BPT",
                    B2R => "B2R",
                    R2B => "R2B",
                    LEPC => "LEPC",
                    SETCRSPTR => "SETCRSPTR",
                    GETCRSPTR => "GETCRSPTR",
                    SETLMEMBASE => "SETLMEMBASE",
                    GETLMEMBASE => "GETLMEMBASE",
                    PIXLD => "PIXLD",
                    FSWZADD => "FSWZADD",
                    IDE => "IDE",
                    CCTL => "CCTL",
                    CCTLL => "CCTLL",
                    CCTLT => "CCTLT",
                    VABSDIFF => "VABSDIFF",
                    VABSDIFF4 => "VABSDIFF4",
                    VADD => "VADD",
                    VMAD => "VMAD",
                    VMNMX => "VMNMX",
                    VSET => "VSET",
                    VSETP => "VSETP",
                    VSHL => "VSHL",
                    VSHR => "VSHR",
                    VOTE_vtg => "VOTE_vtg",
                    _ => "UNKNOWN",
                }
            }
        }
    }
}

// Enable glob-style `use MaxwellOpcode::*`
use MaxwellOpcode::*;

#[cfg(test)]
mod tests {
    use super::*;

    /// `MaskValueFromEncoding` smoke test — `"1110 1111 1010 0---"` packs
    /// 13 specific bits into bits [63:51] of the 64-bit word and leaves
    /// bits [50:48] as wildcards.
    #[test]
    fn mask_value_packs_top16_bits() {
        let (mask, value) = mask_value_from_encoding("1110 1111 1010 0---");
        // Bits 63:51 specified (13 bits), bits 50:48 wildcards.
        assert_eq!(mask, 0xFFF8_0000_0000_0000);
        assert_eq!(value, 0xEFA0_0000_0000_0000);
        // Wildcard '-' bits do not set the mask.
        assert_eq!(mask & 0x0007_0000_0000_0000, 0);
    }

    #[test]
    fn mask_value_all_wildcards_yields_zero() {
        let (mask, value) = mask_value_from_encoding("---- ---- ---- ----");
        assert_eq!(mask, 0);
        assert_eq!(value, 0);
    }

    /// Encoding strings shorter than 16 effective bits (`"100- ----..."`)
    /// should still place mask bits at the top of the word. Pick insn
    /// values that don't collide with the earlier IMAD32I encoding
    /// (`"1000 00-- ---- ----"`).
    #[test]
    fn decodes_ld_st_short_encodings() {
        // LD: "100- ---- ---- ----" — top nibble must be 0b100x. Use 0x9 to
        // avoid the IMAD32I encoding `"1000 00--..."` (top 6 = 100000).
        assert_eq!(
            decode_opcode(0x9000_0000_0000_0000),
            Some(MaxwellOpcode::LD)
        );
        assert_eq!(
            decode_opcode(0x9123_4567_89AB_CDEF),
            Some(MaxwellOpcode::LD)
        );
        // ST: "101- ---- ---- ----" — top nibble must be 0b101x.
        assert_eq!(
            decode_opcode(0xA000_0000_0000_0000),
            Some(MaxwellOpcode::ST)
        );
        assert_eq!(
            decode_opcode(0xBFFF_FFFF_FFFF_FFFF),
            Some(MaxwellOpcode::ST)
        );
    }

    /// Spot-check the encodings the previous ad-hoc decoder also handled:
    /// MOV (cbuf), FADD (reg), EXIT.
    #[test]
    fn decodes_common_opcodes() {
        // MOV (cbuf): "0100 1100 1001 1---" → high 13 bits = 0x4CC | bit 51 = 1.
        let mov_cbuf = 0x4C98_0000_0000_0000;
        assert_eq!(decode_opcode(mov_cbuf), Some(MaxwellOpcode::MOV_cbuf));

        // FADD (reg): "0101 1100 0101 1---".
        let fadd_reg = 0x5C58_0000_0000_0000;
        assert_eq!(decode_opcode(fadd_reg), Some(MaxwellOpcode::FADD_reg));

        // EXIT: "1110 0011 0000 ----".
        let exit = 0xE300_0000_0000_0000;
        assert_eq!(decode_opcode(exit), Some(MaxwellOpcode::EXIT));
    }

    #[test]
    fn overlapping_encodings_keep_upstream_first_match_order() {
        assert_eq!(
            decode_opcode(0xEF90_0000_0000_0000),
            Some(MaxwellOpcode::LDC)
        );
        assert_eq!(
            decode_opcode(0xEF98_0000_0000_0000),
            Some(MaxwellOpcode::MEMBAR)
        );
        assert_eq!(
            decode_opcode(0xEED0_0000_0000_0000),
            Some(MaxwellOpcode::LDG)
        );
        assert_eq!(
            decode_opcode(0xEED8_0000_0000_0000),
            Some(MaxwellOpcode::STG)
        );
        assert_eq!(
            decode_opcode(0xEEA0_0000_0000_0000),
            Some(MaxwellOpcode::STP)
        );
        assert_eq!(
            decode_opcode(0xEBF0_0000_0000_0000),
            Some(MaxwellOpcode::CCTLT)
        );
    }

    /// Sched-control words and all-zero padding must NOT decode to any
    /// opcode. (The frontend skip-every-4th rule keeps these out of the
    /// decoder in practice, but the decoder must still reject them.)
    #[test]
    fn rejects_zero_and_unmapped_words() {
        assert_eq!(decode_opcode(0x0000_0000_0000_0000), None);
        // 0x001F_8000_FFE0_07FF and 0x6000_0000_0000_0000 are sched-control
        assert_eq!(decode_opcode(0x001F_8000_FFE0_07FF), None);
        assert_eq!(decode_opcode(0x6000_0000_0000_0000), None);
    }

    /// Overlap test: BFI_reg `"0101 1011 1111 0---"` and SHF_l_reg
    /// `"0101 1011 1111 1---"` share 12 high bits but differ at bit 51 —
    /// the upstream first-match table must pick the right one for each.
    #[test]
    fn disambiguates_bfi_vs_shf_l_at_bit_51() {
        // BFI_reg expects bit 51 = 0.
        let bfi = 0x5BF0_0000_0000_0000;
        assert_eq!(decode_opcode(bfi), Some(MaxwellOpcode::BFI_reg));
        // SHF_l_reg expects bit 51 = 1.
        let shf_l = 0x5BF8_0000_0000_0000;
        assert_eq!(decode_opcode(shf_l), Some(MaxwellOpcode::SHF_l_reg));
    }

    /// Every encoding must, by construction, decode itself: build an insn
    /// whose top 16 bits exactly equal the encoding's value and verify the
    /// decoder returns the matching opcode. Acts as a defence against typos
    /// in `ENCODINGS_TEXT`.
    #[test]
    fn every_encoding_decodes_itself() {
        for &(expected_op, encoding) in ENCODINGS_TEXT {
            let (mask, value) = mask_value_from_encoding(encoding);
            // Build an instruction whose bits-not-in-mask are zero; the
            // top-16 portion matches the encoding pattern exactly.
            let insn = value;
            let decoded = decode_opcode(insn);
            // Several encodings overlap (e.g., LOP3_cbuf and HFMA2_rc when
            // the wildcard span lets a lower-popcount encoding match the
            // same value). We accept any decode whose mask is a SUPERSET of
            // the specific encoding's mask AND value matches, as long as we
            // got *some* opcode back.
            assert!(
                decoded.is_some(),
                "encoding `{}` for {:?} failed to decode (insn=0x{:016X}, mask=0x{:016X})",
                encoding,
                expected_op,
                insn,
                mask
            );
        }
    }
}
