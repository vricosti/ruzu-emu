// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Maxwell instruction decoder.
//!
//! Decodes 64-bit instruction words into an `Instruction` enum using the top
//! bits to identify opcodes, matching the patterns in zuyu's `maxwell.inc`.

/// Floating-point comparison operation (4 bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FpCompareOp {
    F = 0,
    LT = 1,
    EQ = 2,
    LE = 3,
    GT = 4,
    NE = 5,
    GE = 6,
    NUM = 7,
    NaN = 8,
    LTU = 9,
    EQU = 10,
    LEU = 11,
    GTU = 12,
    NEU = 13,
    GEU = 14,
    T = 15,
}

impl FpCompareOp {
    pub fn from_bits(v: u8) -> Self {
        match v & 0xF {
            0 => Self::F,
            1 => Self::LT,
            2 => Self::EQ,
            3 => Self::LE,
            4 => Self::GT,
            5 => Self::NE,
            6 => Self::GE,
            7 => Self::NUM,
            8 => Self::NaN,
            9 => Self::LTU,
            10 => Self::EQU,
            11 => Self::LEU,
            12 => Self::GTU,
            13 => Self::NEU,
            14 => Self::GEU,
            15 => Self::T,
            _ => unreachable!(),
        }
    }

    /// Evaluate comparison on two f32 values.
    pub fn eval(self, a: f32, b: f32) -> bool {
        match self {
            Self::F => false,
            Self::LT => a < b,
            Self::EQ => a == b,
            Self::LE => a <= b,
            Self::GT => a > b,
            Self::NE => a != b,
            Self::GE => a >= b,
            Self::NUM => !a.is_nan() && !b.is_nan(),
            Self::NaN => a.is_nan() || b.is_nan(),
            Self::LTU => a.is_nan() || b.is_nan() || a < b,
            Self::EQU => a.is_nan() || b.is_nan() || a == b,
            Self::LEU => a.is_nan() || b.is_nan() || a <= b,
            Self::GTU => a.is_nan() || b.is_nan() || a > b,
            Self::NEU => a.is_nan() || b.is_nan() || a != b,
            Self::GEU => a.is_nan() || b.is_nan() || a >= b,
            Self::T => true,
        }
    }
}

/// Integer comparison operation (3 bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntCompareOp {
    False = 0,
    LT = 1,
    EQ = 2,
    LE = 3,
    GT = 4,
    NE = 5,
    GE = 6,
    True = 7,
}

impl IntCompareOp {
    pub fn from_bits(v: u8) -> Self {
        match v & 7 {
            0 => Self::False,
            1 => Self::LT,
            2 => Self::EQ,
            3 => Self::LE,
            4 => Self::GT,
            5 => Self::NE,
            6 => Self::GE,
            7 => Self::True,
            _ => unreachable!(),
        }
    }

    pub fn eval_unsigned(self, a: u32, b: u32) -> bool {
        match self {
            Self::False => false,
            Self::LT => a < b,
            Self::EQ => a == b,
            Self::LE => a <= b,
            Self::GT => a > b,
            Self::NE => a != b,
            Self::GE => a >= b,
            Self::True => true,
        }
    }

    pub fn eval_signed(self, a: i32, b: i32) -> bool {
        match self {
            Self::False => false,
            Self::LT => a < b,
            Self::EQ => a == b,
            Self::LE => a <= b,
            Self::GT => a > b,
            Self::NE => a != b,
            Self::GE => a >= b,
            Self::True => true,
        }
    }
}

/// Boolean operation for predicate combination (2 bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoolOp {
    And = 0,
    Or = 1,
    Xor = 2,
}

impl BoolOp {
    pub fn from_bits(v: u8) -> Self {
        match v & 3 {
            0 => Self::And,
            1 => Self::Or,
            2 => Self::Xor,
            _ => Self::And, // 3 = undefined, default AND
        }
    }

    pub fn eval(self, a: bool, b: bool) -> bool {
        match self {
            Self::And => a && b,
            Self::Or => a || b,
            Self::Xor => a ^ b,
        }
    }
}

/// MUFU sub-operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MufuOp {
    Cos = 0,
    Sin = 1,
    Ex2 = 2,
    Lg2 = 3,
    Rcp = 4,
    Rsq = 5,
    Rcp64H = 6,
    Rsq64H = 7,
    Sqrt = 8,
}

impl MufuOp {
    pub fn from_bits(v: u8) -> Self {
        match v & 0xF {
            0 => Self::Cos,
            1 => Self::Sin,
            2 => Self::Ex2,
            3 => Self::Lg2,
            4 => Self::Rcp,
            5 => Self::Rsq,
            6 => Self::Rcp64H,
            7 => Self::Rsq64H,
            8 => Self::Sqrt,
            _ => Self::Rcp, // Unknown, default to RCP
        }
    }
}

/// S2R system register IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemReg {
    LaneId,
    Clock,
    VirtId,
    InvocationId,
    ThreadKill,
    ShaderType,
    InvocationInfo,
    WScaleFactorXY,
    WScaleFactorZ,
    Tid,
    TidX,
    TidY,
    TidZ,
    CtaParam,
    CtaidX,
    CtaidY,
    CtaidZ,
    NTid,
    EqMask,
    LtMask,
    LeMask,
    GtMask,
    GeMask,
    Unknown(u8),
}

impl SystemReg {
    pub fn from_bits(v: u8) -> Self {
        match v {
            0 => Self::LaneId,
            1 => Self::Clock,
            3 => Self::VirtId,
            17 => Self::InvocationId,
            19 => Self::ThreadKill,
            20 => Self::ShaderType,
            29 => Self::InvocationInfo,
            30 => Self::WScaleFactorXY,
            31 => Self::WScaleFactorZ,
            32 => Self::Tid,
            33 => Self::TidX,
            34 => Self::TidY,
            35 => Self::TidZ,
            36 => Self::CtaParam,
            37 => Self::CtaidX,
            38 => Self::CtaidY,
            39 => Self::CtaidZ,
            40 => Self::NTid,
            56 => Self::EqMask,
            57 => Self::LtMask,
            58 => Self::LeMask,
            59 => Self::GtMask,
            60 => Self::GeMask,
            other => Self::Unknown(other),
        }
    }
}

/// IPA interpolation mode (2 bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpaInterpMode {
    Pass = 0,
    Multiply = 1,
    Constant = 2,
    Sc = 3,
}

/// IPA sample mode (2 bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpaSampleMode {
    Default = 0,
    Centroid = 1,
    Offset = 2,
}

/// Source operand B encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrcB {
    /// Register: bits [27:20].
    Reg(u8),
    /// Constant buffer: index bits [38:34], offset bits [33:20] (×4).
    Cbuf { index: u8, offset: u16 },
    /// 20-bit immediate (sign-extended from bit 56).
    Imm20(i32),
}

/// Source operand C encoding (for FMA-type 3-operand instructions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrcC {
    Reg(u8),
    Cbuf { index: u8, offset: u16 },
}

/// Predicate guard on an instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PredicateGuard {
    /// Predicate register index (0-6, or 7 = PT always true).
    pub index: u8,
    /// Whether the predicate is negated.
    pub negated: bool,
}

impl PredicateGuard {
    /// Extract from instruction bits [19:16].
    pub fn from_insn(insn: u64) -> Self {
        let bits = ((insn >> 16) & 0xF) as u8;
        Self {
            index: bits & 7,
            negated: (bits >> 3) & 1 != 0,
        }
    }

    /// Evaluate the guard given predicate register state.
    pub fn eval(&self, preds: &[bool; 8]) -> bool {
        let val = if self.index == 7 {
            true // PT = always true
        } else {
            preds[self.index as usize]
        };
        if self.negated {
            !val
        } else {
            val
        }
    }
}

/// Decoded Maxwell instruction.
#[derive(Debug, Clone)]
pub enum Instruction {
    // ── Floating point arithmetic ────────────────────────────────────
    Fadd {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
        neg_a: bool,
        neg_b: bool,
        abs_a: bool,
        abs_b: bool,
        sat: bool,
    },
    Fmul {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
        neg_a: bool,
        sat: bool,
    },
    Ffma {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
        src_c: SrcC,
        neg_b: bool,
        neg_c: bool,
        sat: bool,
    },
    Mufu {
        dst: u8,
        src_a: u8,
        op: MufuOp,
    },

    // ── Data movement ────────────────────────────────────────────────
    Mov {
        dst: u8,
        src_b: SrcB,
    },
    Mov32i {
        dst: u8,
        imm32: u32,
    },
    Sel {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
        pred: u8,
        neg_pred: bool,
    },
    Ldc {
        dst: u8,
        src_reg: u8,
        cb_index: u8,
        cb_offset: i32,
    },

    // ── Integer arithmetic ───────────────────────────────────────────
    Iadd {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
        neg_a: bool,
        neg_b: bool,
    },
    Iadd32i {
        dst: u8,
        src_a: u8,
        imm32: u32,
    },
    Iscadd {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
        shift: u8,
    },
    Shl {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
    },
    Shr {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
        is_signed: bool,
    },
    Lop {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
        op: u8, // 0=AND, 1=OR, 2=XOR, 3=PASS_B
        invert_a: bool,
        invert_b: bool,
    },
    Lop32i {
        dst: u8,
        src_a: u8,
        imm32: u32,
        op: u8,
        invert_a: bool,
    },

    // ── Predicate set ────────────────────────────────────────────────
    Fsetp {
        pred_a: u8,
        pred_b: u8,
        src_a: u8,
        src_b: SrcB,
        compare: FpCompareOp,
        bop: BoolOp,
        bop_pred: u8,
        neg_bop_pred: bool,
        neg_a: bool,
        neg_b: bool,
        abs_a: bool,
        abs_b: bool,
        ftz: bool,
    },
    Isetp {
        pred_a: u8,
        pred_b: u8,
        src_a: u8,
        src_b: SrcB,
        compare: IntCompareOp,
        bop: BoolOp,
        bop_pred: u8,
        neg_bop_pred: bool,
        is_signed: bool,
    },

    // ── Conversion ───────────────────────────────────────────────────
    F2i {
        dst: u8,
        src_b: SrcB,
        dst_signed: bool,
    },
    I2f {
        dst: u8,
        src_b: SrcB,
        src_signed: bool,
    },

    // ── Attribute access ─────────────────────────────────────────────
    Ald {
        dst: u8,
        index_reg: u8,
        offset: u16,
        count: u8, // 1-4 components
    },
    Ast {
        src: u8,
        index_reg: u8,
        offset: u16,
        count: u8,
    },
    Ipa {
        dst: u8,
        attr_offset: u8,
        interp_mode: IpaInterpMode,
        sample_mode: IpaSampleMode,
        multiplier_reg: u8,
        sat: bool,
    },

    // ── Texture ──────────────────────────────────────────────────────
    Tex {
        dst: u8,
        coord_reg: u8,
        mask: u8,
        cbuf_offset: u16,
    },
    Texs {
        dst_a: u8,
        dst_b: u8,
        coord_reg: u8,
        tex_type: u8,
    },

    // ── System ───────────────────────────────────────────────────────
    S2r {
        dst: u8,
        sys_reg: SystemReg,
    },

    // ── Integer multiply ──────────────────────────────────────────────
    Xmad {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
        src_c: SrcC,
        /// XMAD mode: 0=CLO, 1=CHI, 2=CSFU, 3=CBCC
        mode: u8,
        /// Whether to use high 16 bits of src_a
        hi_a: bool,
        /// Whether to use high 16 bits of src_b
        hi_b: bool,
        /// Product shift left 16 (PSL)
        psl: bool,
    },

    // ── Integer/Float min-max ───────────────────────────────────────
    Imnmx {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
        pred: u8,
        neg_pred: bool,
        is_signed: bool,
    },
    Fmnmx {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
        pred: u8,
        neg_pred: bool,
        abs_a: bool,
        abs_b: bool,
        neg_a: bool,
        neg_b: bool,
    },

    // ── Bit field operations ────────────────────────────────────────
    Bfe {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
        is_signed: bool,
    },
    Bfi {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
        src_c: SrcC,
    },
    Popc {
        dst: u8,
        src_b: SrcB,
    },
    Flo {
        dst: u8,
        src_b: SrcB,
        is_signed: bool,
        invert: bool,
    },

    // ── Funnel shift ────────────────────────────────────────────────
    Shf {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
        src_c: SrcC,
        direction_right: bool,
        data_type_u64: bool,
        /// Max shift clamp flag
        hi: bool,
    },

    // ── Integer/Float set ───────────────────────────────────────────
    Iset {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
        compare: IntCompareOp,
        bop: BoolOp,
        bop_pred: u8,
        neg_bop_pred: bool,
        is_signed: bool,
        bf: bool,
    },
    Fset {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
        compare: FpCompareOp,
        bop: BoolOp,
        bop_pred: u8,
        neg_bop_pred: bool,
        bf: bool,
    },

    // ── 3-input logic ───────────────────────────────────────────────
    Lop3 {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
        src_c: SrcC,
        lut: u8,
        pred_out: u8,
    },

    // ── Predicate ↔ register ────────────────────────────────────────
    P2r {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
    },
    R2p {
        src_a: u8,
        src_b: SrcB,
        src_c: u8,
    },

    // ── Range reduction ─────────────────────────────────────────────
    Rro {
        dst: u8,
        src_b: SrcB,
    },

    // ── Float-to-float conversion ───────────────────────────────────
    F2f {
        dst: u8,
        src_b: SrcB,
        abs_b: bool,
        neg_b: bool,
        sat: bool,
        /// Destination size: 0=F16, 1=F32, 2=F64
        dst_size: u8,
        /// Source size: 0=F16, 1=F32, 2=F64
        src_size: u8,
        /// Rounding mode: 0=RN, 1=RM, 2=RP, 3=RZ
        rounding: u8,
    },
    /// Integer-to-integer conversion.
    I2i {
        dst: u8,
        src_b: SrcB,
        abs_b: bool,
        sat: bool,
        dst_signed: bool,
        src_signed: bool,
        /// Destination size: 0=U8/S8, 1=U16/S16, 2=U32/S32
        dst_size: u8,
        /// Source size: 0=U8/S8, 1=U16/S16, 2=U32/S32
        src_size: u8,
    },

    // ── Global memory ───────────────────────────────────────────────
    Ldg {
        dst: u8,
        src_a: u8,
        offset: i32,
        /// Data size: 0=U8, 1=S8, 2=U16, 3=S16, 4=32, 5=64, 6=128
        size: u8,
    },
    Stg {
        src: u8,
        src_a: u8,
        offset: i32,
        /// Data size: same encoding as LDG
        size: u8,
    },

    // ── 3-operand integer add ───────────────────────────────────────
    Iadd3 {
        dst: u8,
        src_a: u8,
        src_b: SrcB,
        src_c: SrcC,
        neg_a: bool,
        neg_b: bool,
        neg_c: bool,
    },

    // ── Control flow ─────────────────────────────────────────────────
    Bra {
        offset: i32,
    },
    Exit,
    Nop,

    /// Instruction not yet implemented — treated as NOP.
    Unknown {
        raw: u64,
    },
}

// ── Bit extraction helpers ──────────────────────────────────────────────

/// Extract bits [hi:lo] inclusive from a u64 value.
#[inline]
fn bits(val: u64, lo: u32, hi: u32) -> u64 {
    let mask = (1u64 << (hi - lo + 1)) - 1;
    (val >> lo) & mask
}

/// Extract destination register (bits [7:0]).
#[inline]
fn dst_reg(insn: u64) -> u8 {
    bits(insn, 0, 7) as u8
}

/// Extract source A register (bits [15:8]).
#[inline]
fn src_a_reg(insn: u64) -> u8 {
    bits(insn, 8, 15) as u8
}

/// Extract source B register (bits [27:20]).
#[inline]
fn src_b_reg(insn: u64) -> u8 {
    bits(insn, 20, 27) as u8
}

/// Extract source C register (bits [46:39]).
#[inline]
fn src_c_reg(insn: u64) -> u8 {
    bits(insn, 39, 46) as u8
}

/// Extract constant buffer operand: index=bits[38:34], offset=bits[33:20].
fn cbuf_operand(insn: u64) -> SrcB {
    let index = bits(insn, 34, 38) as u8;
    let offset = bits(insn, 20, 33) as u16;
    SrcB::Cbuf { index, offset }
}

/// Extract 20-bit signed immediate: bits [38:20] + sign at bit 56.
fn imm20_operand(insn: u64) -> SrcB {
    let raw = bits(insn, 20, 38) as i32;
    let neg = bits(insn, 56, 56) != 0;
    let val = if neg { -raw } else { raw };
    SrcB::Imm20(val)
}

/// Decode a 64-bit Maxwell instruction word.
pub fn decode(insn: u64) -> (Instruction, PredicateGuard) {
    let guard = PredicateGuard::from_insn(insn);
    let top16 = ((insn >> 48) & 0xFFFF) as u16;

    let instruction = decode_opcode(insn, top16);
    (instruction, guard)
}

fn decode_opcode(insn: u64, top16: u16) -> Instruction {
    // Match instructions by top bits, from most specific to least specific.
    // The patterns below are derived from zuyu's maxwell.inc.

    // ── EXIT: "1110 0011 0000 ----" ─────────────────────────────────
    if top16 & 0xFFF0 == 0xE300 {
        return Instruction::Exit;
    }

    // ── NOP: "0101 0000 1011 0---" ──────────────────────────────────
    if top16 & 0xFFF8 == 0x50B0 {
        return Instruction::Nop;
    }

    // ── BRA: "1110 0010 0100 ----" ──────────────────────────────────
    if top16 & 0xFFF0 == 0xE240 {
        let raw_offset = bits(insn, 20, 43) as u32;
        // Sign-extend 24 bits.
        let offset = if raw_offset & (1 << 23) != 0 {
            (raw_offset | 0xFF00_0000) as i32
        } else {
            raw_offset as i32
        };
        return Instruction::Bra { offset };
    }

    // ── S2R: "1111 0000 1100 1---" ──────────────────────────────────
    if top16 & 0xFFF8 == 0xF0C8 {
        return Instruction::S2r {
            dst: dst_reg(insn),
            sys_reg: SystemReg::from_bits(bits(insn, 20, 27) as u8),
        };
    }

    // ── MUFU: "0101 0000 1000 0---" ─────────────────────────────────
    if top16 & 0xFFF8 == 0x5080 {
        return Instruction::Mufu {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            op: MufuOp::from_bits(bits(insn, 20, 23) as u8),
        };
    }

    // ── MOV32I: "0000 0001 0000 ----" ───────────────────────────────
    if top16 & 0xFFF0 == 0x0100 {
        let imm32 = bits(insn, 20, 51) as u32;
        return Instruction::Mov32i {
            dst: dst_reg(insn),
            imm32,
        };
    }

    // ── LDC: "1110 1111 1001 0---" ──────────────────────────────────
    if top16 & 0xFFF8 == 0xEF90 {
        let cb_index = bits(insn, 36, 40) as u8;
        let offset_raw = bits(insn, 20, 35) as u16;
        let offset = (offset_raw as i16) as i32;
        return Instruction::Ldc {
            dst: dst_reg(insn),
            src_reg: src_a_reg(insn),
            cb_index,
            cb_offset: offset,
        };
    }

    // ── ALD: "1110 1111 1101 1---" ──────────────────────────────────
    if top16 & 0xFFF8 == 0xEFD8 {
        let offset = bits(insn, 20, 29) as u16;
        let size = bits(insn, 47, 48) as u8;
        let count = match size {
            0 => 1,
            1 => 2,
            2 => 3,
            3 => 4,
            _ => 1,
        };
        return Instruction::Ald {
            dst: dst_reg(insn),
            index_reg: src_a_reg(insn),
            offset,
            count,
        };
    }

    // ── AST: "1110 1111 1111 0---" ──────────────────────────────────
    if top16 & 0xFFF8 == 0xEFF0 {
        let offset = bits(insn, 20, 29) as u16;
        let size = bits(insn, 47, 48) as u8;
        let count = match size {
            0 => 1,
            1 => 2,
            2 => 3,
            3 => 4,
            _ => 1,
        };
        return Instruction::Ast {
            src: dst_reg(insn), // src register at bits [0:7] for AST
            index_reg: src_a_reg(insn),
            offset,
            count,
        };
    }

    // ── IPA: "1110 0000 ---- ----" ──────────────────────────────────
    if top16 & 0xFF00 == 0xE000 {
        return Instruction::Ipa {
            dst: dst_reg(insn),
            attr_offset: bits(insn, 28, 37) as u8,
            interp_mode: match bits(insn, 54, 55) {
                0 => IpaInterpMode::Pass,
                1 => IpaInterpMode::Multiply,
                2 => IpaInterpMode::Constant,
                _ => IpaInterpMode::Sc,
            },
            sample_mode: match bits(insn, 52, 53) {
                0 => IpaSampleMode::Default,
                1 => IpaSampleMode::Centroid,
                _ => IpaSampleMode::Offset,
            },
            multiplier_reg: bits(insn, 20, 27) as u8,
            sat: bits(insn, 51, 51) != 0,
        };
    }

    // ── TEX: "1100 0--- ---- ----" ──────────────────────────────────
    if top16 & 0xF800 == 0xC000 {
        return Instruction::Tex {
            dst: dst_reg(insn),
            coord_reg: src_a_reg(insn),
            mask: bits(insn, 31, 34) as u8,
            cbuf_offset: bits(insn, 36, 48) as u16,
        };
    }

    // ── TEXS: "1101 -00- ---- ----" — mask 0xF600, value 0xD000
    if top16 & 0xF600 == 0xD000 {
        return Instruction::Texs {
            dst_a: dst_reg(insn),
            dst_b: bits(insn, 28, 35) as u8,
            coord_reg: src_a_reg(insn),
            tex_type: bits(insn, 52, 55) as u8,
        };
    }

    // ── FFMA variants ───────────────────────────────────────────────
    // FFMA_reg: "0101 1001 1--- ----" — 9 fixed bits → mask 0xFF80
    if top16 & 0xFF80 == 0x5980 {
        return decode_ffma(insn, SrcB::Reg(src_b_reg(insn)), SrcC::Reg(src_c_reg(insn)));
    }
    // FFMA_rc: "0101 0001 1--- ----" — 9 fixed bits → mask 0xFF80
    if top16 & 0xFF80 == 0x5180 {
        return decode_ffma(insn, SrcB::Reg(src_b_reg(insn)), cbuf_src_c(insn));
    }
    // FFMA_cr: "0100 1001 1--- ----" — 9 fixed bits → mask 0xFF80
    if top16 & 0xFF80 == 0x4980 {
        return decode_ffma(insn, cbuf_operand(insn), SrcC::Reg(src_c_reg(insn)));
    }
    // FFMA_imm: "0011 001- 1--- ----" — bits 15-9 + bit 7 → mask 0xFE80
    if top16 & 0xFE80 == 0x3280 {
        return decode_ffma(insn, imm20_operand(insn), SrcC::Reg(src_c_reg(insn)));
    }

    // ── FADD variants ───────────────────────────────────────────────
    // FADD_reg: "0101 1100 0101 1---"
    if top16 & 0xFFF8 == 0x5C58 {
        return decode_fadd(insn, SrcB::Reg(src_b_reg(insn)));
    }
    // FADD_cbuf: "0100 1100 0101 1---"
    if top16 & 0xFFF8 == 0x4C58 {
        return decode_fadd(insn, cbuf_operand(insn));
    }
    // FADD_imm: "0011 100- 0101 1---" — bit 8 don't-care → mask 0xFEF8
    if top16 & 0xFEF8 == 0x3858 {
        return decode_fadd(insn, imm20_operand(insn));
    }

    // ── FMUL variants ───────────────────────────────────────────────
    // FMUL_reg: "0101 1100 0110 1---"
    if top16 & 0xFFF8 == 0x5C68 {
        return decode_fmul(insn, SrcB::Reg(src_b_reg(insn)));
    }
    // FMUL_cbuf: "0100 1100 0110 1---"
    if top16 & 0xFFF8 == 0x4C68 {
        return decode_fmul(insn, cbuf_operand(insn));
    }
    // FMUL_imm: "0011 100- 0110 1---" — bit 8 don't-care → mask 0xFEF8
    if top16 & 0xFEF8 == 0x3868 {
        return decode_fmul(insn, imm20_operand(insn));
    }

    // ── MOV variants ────────────────────────────────────────────────
    // MOV_reg: "0101 1100 1001 1---"
    if top16 & 0xFFF8 == 0x5C98 {
        return Instruction::Mov {
            dst: dst_reg(insn),
            src_b: SrcB::Reg(src_b_reg(insn)),
        };
    }
    // MOV_cbuf: "0100 1100 1001 1---"
    if top16 & 0xFFF8 == 0x4C98 {
        return Instruction::Mov {
            dst: dst_reg(insn),
            src_b: cbuf_operand(insn),
        };
    }
    // MOV_imm: "0011 100- 1001 1---" — bit 8 don't-care → mask 0xFEF8
    if top16 & 0xFEF8 == 0x3898 {
        return Instruction::Mov {
            dst: dst_reg(insn),
            src_b: imm20_operand(insn),
        };
    }

    // ── SEL variants ────────────────────────────────────────────────
    // SEL_reg: "0101 1100 1010 0---"
    if top16 & 0xFFF8 == 0x5CA0 {
        return decode_sel(insn, SrcB::Reg(src_b_reg(insn)));
    }
    // SEL_cbuf: "0100 1100 1010 0---"
    if top16 & 0xFFF8 == 0x4CA0 {
        return decode_sel(insn, cbuf_operand(insn));
    }
    // SEL_imm: "0011 100- 1010 0---" — bit 8 don't-care → mask 0xFEF8
    if top16 & 0xFEF8 == 0x38A0 {
        return decode_sel(insn, imm20_operand(insn));
    }

    // ── FSETP variants ──────────────────────────────────────────────
    // FSETP_reg: "0101 1011 1011 ----"
    if top16 & 0xFFF0 == 0x5BB0 {
        return decode_fsetp(insn, SrcB::Reg(src_b_reg(insn)));
    }
    // FSETP_cbuf: "0100 1011 1011 ----"
    if top16 & 0xFFF0 == 0x4BB0 {
        return decode_fsetp(insn, cbuf_operand(insn));
    }
    // FSETP_imm: "0011 011- 1011 ----" — bit 8 don't-care → mask 0xFEF0
    if top16 & 0xFEF0 == 0x36B0 {
        return decode_fsetp(insn, imm20_operand(insn));
    }

    // ── ISETP variants ──────────────────────────────────────────────
    // ISETP_reg: "0101 1011 0110 ----"
    if top16 & 0xFFF0 == 0x5B60 {
        return decode_isetp(insn, SrcB::Reg(src_b_reg(insn)));
    }
    // ISETP_cbuf: "0100 1011 0110 ----"
    if top16 & 0xFFF0 == 0x4B60 {
        return decode_isetp(insn, cbuf_operand(insn));
    }
    // ISETP_imm: "0011 011- 0110 ----" — bit 8 don't-care → mask 0xFEF0
    if top16 & 0xFEF0 == 0x3660 {
        return decode_isetp(insn, imm20_operand(insn));
    }

    // ── IADD variants ───────────────────────────────────────────────
    // IADD_reg: "0101 1100 0001 0---"
    if top16 & 0xFFF8 == 0x5C10 {
        return decode_iadd(insn, SrcB::Reg(src_b_reg(insn)));
    }
    // IADD_cbuf: "0100 1100 0001 0---"
    if top16 & 0xFFF8 == 0x4C10 {
        return decode_iadd(insn, cbuf_operand(insn));
    }
    // IADD_imm: "0011 100- 0001 0---" — bit 8 don't-care → mask 0xFEF8
    if top16 & 0xFEF8 == 0x3810 {
        return decode_iadd(insn, imm20_operand(insn));
    }

    // ── IADD32I: "0001 110- ---- ----" ──────────────────────────────
    if top16 & 0xFE00 == 0x1C00 {
        return Instruction::Iadd32i {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            imm32: bits(insn, 20, 51) as u32,
        };
    }

    // ── ISCADD variants ─────────────────────────────────────────────
    // ISCADD_reg: "0101 1100 0001 1---"
    if top16 & 0xFFF8 == 0x5C18 {
        return Instruction::Iscadd {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: SrcB::Reg(src_b_reg(insn)),
            shift: bits(insn, 39, 43) as u8,
        };
    }
    // ISCADD_cbuf: "0100 1100 0001 1---"
    if top16 & 0xFFF8 == 0x4C18 {
        return Instruction::Iscadd {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: cbuf_operand(insn),
            shift: bits(insn, 39, 43) as u8,
        };
    }
    // ISCADD_imm: "0011 100- 0001 1---" — bit 8 don't-care → mask 0xFEF8
    if top16 & 0xFEF8 == 0x3818 {
        return Instruction::Iscadd {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: imm20_operand(insn),
            shift: bits(insn, 39, 43) as u8,
        };
    }

    // ── SHL variants ────────────────────────────────────────────────
    // SHL_reg: "0101 1100 0100 1---"
    if top16 & 0xFFF8 == 0x5C48 {
        return Instruction::Shl {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: SrcB::Reg(src_b_reg(insn)),
        };
    }
    // SHL_cbuf: "0100 1100 0100 1---"
    if top16 & 0xFFF8 == 0x4C48 {
        return Instruction::Shl {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: cbuf_operand(insn),
        };
    }
    // SHL_imm: "0011 100- 0100 1---" — bit 8 don't-care → mask 0xFEF8
    if top16 & 0xFEF8 == 0x3848 {
        return Instruction::Shl {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: imm20_operand(insn),
        };
    }

    // ── SHR variants ────────────────────────────────────────────────
    // SHR_reg: "0101 1100 0010 1---"
    if top16 & 0xFFF8 == 0x5C28 {
        return Instruction::Shr {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: SrcB::Reg(src_b_reg(insn)),
            is_signed: bits(insn, 48, 48) != 0,
        };
    }
    // SHR_cbuf: "0100 1100 0010 1---"
    if top16 & 0xFFF8 == 0x4C28 {
        return Instruction::Shr {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: cbuf_operand(insn),
            is_signed: bits(insn, 48, 48) != 0,
        };
    }
    // SHR_imm: "0011 100- 0010 1---" — bit 8 don't-care → mask 0xFEF8
    if top16 & 0xFEF8 == 0x3828 {
        return Instruction::Shr {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: imm20_operand(insn),
            is_signed: bits(insn, 48, 48) != 0,
        };
    }

    // ── LOP variants ────────────────────────────────────────────────
    // LOP_reg: "0101 1100 0100 0---"
    if top16 & 0xFFF8 == 0x5C40 {
        return Instruction::Lop {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: SrcB::Reg(src_b_reg(insn)),
            op: bits(insn, 41, 42) as u8,
            invert_a: bits(insn, 39, 39) != 0,
            invert_b: bits(insn, 40, 40) != 0,
        };
    }
    // LOP_cbuf: "0100 1100 0100 0---"
    if top16 & 0xFFF8 == 0x4C40 {
        return Instruction::Lop {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: cbuf_operand(insn),
            op: bits(insn, 41, 42) as u8,
            invert_a: bits(insn, 39, 39) != 0,
            invert_b: bits(insn, 40, 40) != 0,
        };
    }
    // LOP_imm: "0011 100- 0100 0---" — bit 8 don't-care → mask 0xFEF8
    if top16 & 0xFEF8 == 0x3840 {
        return Instruction::Lop {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: imm20_operand(insn),
            op: bits(insn, 41, 42) as u8,
            invert_a: bits(insn, 39, 39) != 0,
            invert_b: bits(insn, 40, 40) != 0,
        };
    }

    // ── LOP32I: "0000 01-- ---- ----" ───────────────────────────────
    if top16 & 0xFC00 == 0x0400 {
        return Instruction::Lop32i {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            imm32: bits(insn, 20, 51) as u32,
            op: bits(insn, 53, 54) as u8,
            invert_a: bits(insn, 55, 55) != 0,
        };
    }

    // ── F2I variants ────────────────────────────────────────────────
    // F2I_reg: "0101 1100 1011 0---"
    if top16 & 0xFFF8 == 0x5CB0 {
        return Instruction::F2i {
            dst: dst_reg(insn),
            src_b: SrcB::Reg(src_b_reg(insn)),
            dst_signed: bits(insn, 12, 12) != 0,
        };
    }
    // F2I_cbuf: "0100 1100 1011 0---"
    if top16 & 0xFFF8 == 0x4CB0 {
        return Instruction::F2i {
            dst: dst_reg(insn),
            src_b: cbuf_operand(insn),
            dst_signed: bits(insn, 12, 12) != 0,
        };
    }
    // F2I_imm: "0011 100- 1011 0---" — bit 8 don't-care → mask 0xFEF8
    if top16 & 0xFEF8 == 0x38B0 {
        return Instruction::F2i {
            dst: dst_reg(insn),
            src_b: imm20_operand(insn),
            dst_signed: bits(insn, 12, 12) != 0,
        };
    }

    // ── I2F variants ────────────────────────────────────────────────
    // I2F_reg: "0101 1100 1011 1---"
    if top16 & 0xFFF8 == 0x5CB8 {
        return Instruction::I2f {
            dst: dst_reg(insn),
            src_b: SrcB::Reg(src_b_reg(insn)),
            src_signed: bits(insn, 13, 13) != 0,
        };
    }
    // I2F_cbuf: "0100 1100 1011 1---"
    if top16 & 0xFFF8 == 0x4CB8 {
        return Instruction::I2f {
            dst: dst_reg(insn),
            src_b: cbuf_operand(insn),
            src_signed: bits(insn, 13, 13) != 0,
        };
    }
    // I2F_imm: "0011 100- 1011 1---" — bit 8 don't-care → mask 0xFEF8
    if top16 & 0xFEF8 == 0x38B8 {
        return Instruction::I2f {
            dst: dst_reg(insn),
            src_b: imm20_operand(insn),
            src_signed: bits(insn, 13, 13) != 0,
        };
    }

    // ── FADD32I: "0000 10-- ---- ----" ──────────────────────────────
    if top16 & 0xFC00 == 0x0800 {
        let imm32 = bits(insn, 20, 51) as u32;
        return Instruction::Fadd {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: SrcB::Imm20(imm32 as i32), // Actually a 32-bit float immediate
            neg_a: bits(insn, 56, 56) != 0,
            neg_b: bits(insn, 53, 53) != 0,
            abs_a: bits(insn, 54, 54) != 0,
            abs_b: false,
            sat: false,
        };
    }

    // ── FMUL32I: "0001 1110 ---- ----" ──────────────────────────────
    if top16 & 0xFF00 == 0x1E00 {
        let imm32 = bits(insn, 20, 51) as u32;
        return Instruction::Fmul {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: SrcB::Imm20(imm32 as i32),
            neg_a: false,
            sat: bits(insn, 55, 55) != 0,
        };
    }

    // ── FFMA32I: "0000 11-- ---- ----" ──────────────────────────────
    if top16 & 0xFC00 == 0x0C00 {
        let imm32 = bits(insn, 20, 51) as u32;
        return Instruction::Ffma {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: SrcB::Imm20(imm32 as i32),
            src_c: SrcC::Reg(dst_reg(insn)), // src_c reuses dest in 32I form
            neg_b: false,
            neg_c: bits(insn, 57, 57) != 0,
            sat: bits(insn, 55, 55) != 0,
        };
    }

    // ── KIL: "1110 0011 0011 ----" ──────────────────────────────────
    if top16 & 0xFFF0 == 0xE330 {
        // Treat KIL as EXIT with killed flag (handled in interpreter)
        return Instruction::Exit;
    }

    // ── DEPBAR: "1111 0000 1111 0---" — treat as NOP ────────────────
    if top16 & 0xFFF8 == 0xF0F0 {
        return Instruction::Nop;
    }

    // ── BAR: "1111 0000 1010 1---" — treat as NOP (barrier sync) ──
    if top16 & 0xFFF8 == 0xF0A8 {
        return Instruction::Nop;
    }

    // ── RRO: range reduction (pass-through for interpreter) ───────
    // RRO_reg: "0101 1001 0000 ----"
    if top16 & 0xFFF0 == 0x5900 {
        return Instruction::Rro {
            dst: dst_reg(insn),
            src_b: SrcB::Reg(src_b_reg(insn)),
        };
    }
    // RRO_cbuf: "0100 1001 0000 ----"
    if top16 & 0xFFF0 == 0x4900 {
        return Instruction::Rro {
            dst: dst_reg(insn),
            src_b: cbuf_operand(insn),
        };
    }
    // RRO_imm: "0011 001- 0000 ----"
    if top16 & 0xFEF0 == 0x3200 {
        return Instruction::Rro {
            dst: dst_reg(insn),
            src_b: imm20_operand(insn),
        };
    }

    // ── P2R: "0011 1000 1100 1---" ────────────────────────────────
    if top16 & 0xFFF8 == 0x38C8 {
        return Instruction::P2r {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: imm20_operand(insn),
        };
    }

    // ── R2P: "0011 1000 1100 0---" ────────────────────────────────
    if top16 & 0xFFF8 == 0x38C0 {
        return Instruction::R2p {
            src_a: src_a_reg(insn),
            src_b: imm20_operand(insn),
            src_c: dst_reg(insn),
        };
    }

    // ── LDG: "1110 1110 1101 ----" ────────────────────────────────
    if top16 & 0xFFF0 == 0xEED0 {
        let raw_offset = bits(insn, 20, 43) as u32;
        let offset = if raw_offset & (1 << 23) != 0 {
            (raw_offset | 0xFF00_0000) as i32
        } else {
            raw_offset as i32
        };
        return Instruction::Ldg {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            offset,
            size: bits(insn, 48, 50) as u8,
        };
    }

    // ── STG: "1110 1110 1111 ----" ────────────────────────────────
    if top16 & 0xFFF0 == 0xEEF0 {
        let raw_offset = bits(insn, 20, 43) as u32;
        let offset = if raw_offset & (1 << 23) != 0 {
            (raw_offset | 0xFF00_0000) as i32
        } else {
            raw_offset as i32
        };
        return Instruction::Stg {
            src: dst_reg(insn),
            src_a: src_a_reg(insn),
            offset,
            size: bits(insn, 48, 50) as u8,
        };
    }

    // ── XMAD variants ────────────────────────────────────────────
    // XMAD_reg: "0101 1011 0000 ----"
    if top16 & 0xFFF0 == 0x5B00 {
        return decode_xmad(insn, SrcB::Reg(src_b_reg(insn)), SrcC::Reg(src_c_reg(insn)));
    }
    // XMAD_rc: "0101 0001 0000 ----" (reg + cbuf_c)
    if top16 & 0xFFF0 == 0x5100 {
        return decode_xmad(insn, SrcB::Reg(src_b_reg(insn)), cbuf_src_c(insn));
    }
    // XMAD_cr: "0100 1110 0000 ----" (cbuf + reg_c)
    if top16 & 0xFFF0 == 0x4E00 {
        return decode_xmad(insn, cbuf_operand(insn), SrcC::Reg(src_c_reg(insn)));
    }
    // XMAD_imm: "0011 011- 0000 ----"
    if top16 & 0xFEF0 == 0x3600 {
        return decode_xmad(insn, imm20_operand(insn), SrcC::Reg(src_c_reg(insn)));
    }

    // ── IMNMX variants ──────────────────────────────────────────
    // IMNMX_reg: "0101 1100 0010 0---"
    if top16 & 0xFFF8 == 0x5C20 {
        return decode_imnmx(insn, SrcB::Reg(src_b_reg(insn)));
    }
    // IMNMX_cbuf: "0100 1100 0010 0---"
    if top16 & 0xFFF8 == 0x4C20 {
        return decode_imnmx(insn, cbuf_operand(insn));
    }
    // IMNMX_imm: "0011 100- 0010 0---"
    if top16 & 0xFEF8 == 0x3820 {
        return decode_imnmx(insn, imm20_operand(insn));
    }

    // ── FMNMX variants ──────────────────────────────────────────
    // FMNMX_reg: "0101 1100 0110 0---"
    if top16 & 0xFFF8 == 0x5C60 {
        return decode_fmnmx(insn, SrcB::Reg(src_b_reg(insn)));
    }
    // FMNMX_cbuf: "0100 1100 0110 0---"
    if top16 & 0xFFF8 == 0x4C60 {
        return decode_fmnmx(insn, cbuf_operand(insn));
    }
    // FMNMX_imm: "0011 100- 0110 0---"
    if top16 & 0xFEF8 == 0x3860 {
        return decode_fmnmx(insn, imm20_operand(insn));
    }

    // ── BFE variants ────────────────────────────────────────────
    // BFE_reg: "0101 1100 0000 0---"
    if top16 & 0xFFF8 == 0x5C00 {
        return Instruction::Bfe {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: SrcB::Reg(src_b_reg(insn)),
            is_signed: bits(insn, 48, 48) != 0,
        };
    }
    // BFE_cbuf: "0100 1100 0000 0---"
    if top16 & 0xFFF8 == 0x4C00 {
        return Instruction::Bfe {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: cbuf_operand(insn),
            is_signed: bits(insn, 48, 48) != 0,
        };
    }
    // BFE_imm: "0011 100- 0000 0---"
    if top16 & 0xFEF8 == 0x3800 {
        return Instruction::Bfe {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: imm20_operand(insn),
            is_signed: bits(insn, 48, 48) != 0,
        };
    }

    // ── BFI variants ────────────────────────────────────────────
    // BFI_reg: "0101 1011 1111 ----"
    if top16 & 0xFFF0 == 0x5BF0 {
        return Instruction::Bfi {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: SrcB::Reg(src_b_reg(insn)),
            src_c: SrcC::Reg(src_c_reg(insn)),
        };
    }
    // BFI_rc: "0011 0110 1111 ----"
    if top16 & 0xFFF0 == 0x36F0 {
        return Instruction::Bfi {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: imm20_operand(insn),
            src_c: SrcC::Reg(src_c_reg(insn)),
        };
    }
    // BFI_cr: "0100 1011 1111 ----"
    if top16 & 0xFFF0 == 0x4BF0 {
        return Instruction::Bfi {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: cbuf_operand(insn),
            src_c: SrcC::Reg(src_c_reg(insn)),
        };
    }

    // ── POPC variants ───────────────────────────────────────────
    // POPC_reg: "0101 1100 0000 1---"
    if top16 & 0xFFF8 == 0x5C08 {
        return Instruction::Popc {
            dst: dst_reg(insn),
            src_b: SrcB::Reg(src_b_reg(insn)),
        };
    }
    // POPC_cbuf: "0100 1100 0000 1---"
    if top16 & 0xFFF8 == 0x4C08 {
        return Instruction::Popc {
            dst: dst_reg(insn),
            src_b: cbuf_operand(insn),
        };
    }
    // POPC_imm: "0011 100- 0000 1---"
    if top16 & 0xFEF8 == 0x3808 {
        return Instruction::Popc {
            dst: dst_reg(insn),
            src_b: imm20_operand(insn),
        };
    }

    // ── FLO variants ────────────────────────────────────────────
    // FLO_reg: "0101 1100 0011 0---"
    if top16 & 0xFFF8 == 0x5C30 {
        return Instruction::Flo {
            dst: dst_reg(insn),
            src_b: SrcB::Reg(src_b_reg(insn)),
            is_signed: bits(insn, 48, 48) != 0,
            invert: bits(insn, 40, 40) != 0,
        };
    }
    // FLO_cbuf: "0100 1100 0011 0---"
    if top16 & 0xFFF8 == 0x4C30 {
        return Instruction::Flo {
            dst: dst_reg(insn),
            src_b: cbuf_operand(insn),
            is_signed: bits(insn, 48, 48) != 0,
            invert: bits(insn, 40, 40) != 0,
        };
    }
    // FLO_imm: "0011 100- 0011 0---"
    if top16 & 0xFEF8 == 0x3830 {
        return Instruction::Flo {
            dst: dst_reg(insn),
            src_b: imm20_operand(insn),
            is_signed: bits(insn, 48, 48) != 0,
            invert: bits(insn, 40, 40) != 0,
        };
    }

    // ── SHF variants (funnel shift) ─────────────────────────────
    // SHF_right_reg: "0101 1100 1111 1---"
    if top16 & 0xFFF8 == 0x5CF8 {
        return decode_shf(insn, SrcB::Reg(src_b_reg(insn)), true);
    }
    // SHF_right_imm: "0011 100- 1111 1---"
    if top16 & 0xFEF8 == 0x38F8 {
        return decode_shf(insn, imm20_operand(insn), true);
    }
    // SHF_left_reg: "0101 1011 1111 1---"
    if top16 & 0xFFF8 == 0x5BF8 {
        return decode_shf(insn, SrcB::Reg(src_b_reg(insn)), false);
    }
    // SHF_left_imm: "0011 011- 1111 1---"
    if top16 & 0xFEF8 == 0x36F8 {
        return decode_shf(insn, imm20_operand(insn), false);
    }

    // ── ISET variants ───────────────────────────────────────────
    // ISET_reg: "0101 1011 0101 0---"
    if top16 & 0xFFF8 == 0x5B50 {
        return decode_iset(insn, SrcB::Reg(src_b_reg(insn)));
    }
    // ISET_cbuf: "0100 1011 0101 0---"
    if top16 & 0xFFF8 == 0x4B50 {
        return decode_iset(insn, cbuf_operand(insn));
    }
    // ISET_imm: "0011 011- 0101 0---"
    if top16 & 0xFEF8 == 0x3650 {
        return decode_iset(insn, imm20_operand(insn));
    }

    // ── FSET variants ───────────────────────────────────────────
    // FSET_reg: "0101 1000 0000 ----"
    if top16 & 0xFFF0 == 0x5800 {
        return decode_fset(insn, SrcB::Reg(src_b_reg(insn)));
    }
    // FSET_cbuf: "0100 1000 0000 ----"
    if top16 & 0xFFF0 == 0x4800 {
        return decode_fset(insn, cbuf_operand(insn));
    }
    // FSET_imm: "0011 000- 0000 ----"
    if top16 & 0xFEF0 == 0x3000 {
        return decode_fset(insn, imm20_operand(insn));
    }

    // ── LOP3: "0101 1011 1110 0---" (imm LUT) ────────────────────
    if top16 & 0xFFF8 == 0x5BE0 {
        return Instruction::Lop3 {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: SrcB::Reg(src_b_reg(insn)),
            src_c: SrcC::Reg(src_c_reg(insn)),
            lut: bits(insn, 28, 35) as u8,
            pred_out: bits(insn, 48, 50) as u8,
        };
    }
    // LOP3_cbuf: "0100 1011 1110 0---"
    if top16 & 0xFFF8 == 0x4BE0 {
        return Instruction::Lop3 {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: cbuf_operand(insn),
            src_c: SrcC::Reg(src_c_reg(insn)),
            lut: bits(insn, 28, 35) as u8,
            pred_out: bits(insn, 48, 50) as u8,
        };
    }
    // LOP3_imm: "0011 011- 1110 0---"
    if top16 & 0xFEF8 == 0x36E0 {
        return Instruction::Lop3 {
            dst: dst_reg(insn),
            src_a: src_a_reg(insn),
            src_b: imm20_operand(insn),
            src_c: SrcC::Reg(src_c_reg(insn)),
            lut: bits(insn, 28, 35) as u8,
            pred_out: bits(insn, 48, 50) as u8,
        };
    }

    // ── F2F variants ────────────────────────────────────────────
    // F2F_reg: "0101 1100 1010 1---"
    if top16 & 0xFFF8 == 0x5CA8 {
        return decode_f2f(insn, SrcB::Reg(src_b_reg(insn)));
    }
    // F2F_cbuf: "0100 1100 1010 1---"
    if top16 & 0xFFF8 == 0x4CA8 {
        return decode_f2f(insn, cbuf_operand(insn));
    }
    // F2F_imm: "0011 100- 1010 1---"
    if top16 & 0xFEF8 == 0x38A8 {
        return decode_f2f(insn, imm20_operand(insn));
    }

    // ── I2I variants ────────────────────────────────────────────
    // I2I_reg: "0101 1100 1110 0---"
    if top16 & 0xFFF8 == 0x5CE0 {
        return decode_i2i(insn, SrcB::Reg(src_b_reg(insn)));
    }
    // I2I_cbuf: "0100 1100 1110 0---"
    if top16 & 0xFFF8 == 0x4CE0 {
        return decode_i2i(insn, cbuf_operand(insn));
    }
    // I2I_imm: "0011 100- 1110 0---"
    if top16 & 0xFEF8 == 0x38E0 {
        return decode_i2i(insn, imm20_operand(insn));
    }

    // ── IADD3 variants ──────────────────────────────────────────
    // IADD3_reg: "0101 1100 1100 0---"
    if top16 & 0xFFF8 == 0x5CC0 {
        return decode_iadd3(insn, SrcB::Reg(src_b_reg(insn)));
    }
    // IADD3_cbuf: "0100 1100 1100 0---"
    if top16 & 0xFFF8 == 0x4CC0 {
        return decode_iadd3(insn, cbuf_operand(insn));
    }
    // IADD3_imm: "0011 100- 1100 0---"
    if top16 & 0xFEF8 == 0x38C0 {
        return decode_iadd3(insn, imm20_operand(insn));
    }

    // ── Unknown instruction ─────────────────────────────────────────
    Instruction::Unknown { raw: insn }
}

// ── Instruction-specific decoders ───────────────────────────────────────

fn decode_fadd(insn: u64, src_b: SrcB) -> Instruction {
    Instruction::Fadd {
        dst: dst_reg(insn),
        src_a: src_a_reg(insn),
        src_b,
        neg_a: bits(insn, 48, 48) != 0,
        neg_b: bits(insn, 45, 45) != 0,
        abs_a: bits(insn, 46, 46) != 0,
        abs_b: bits(insn, 49, 49) != 0,
        sat: bits(insn, 50, 50) != 0,
    }
}

fn decode_fmul(insn: u64, src_b: SrcB) -> Instruction {
    Instruction::Fmul {
        dst: dst_reg(insn),
        src_a: src_a_reg(insn),
        src_b,
        neg_a: bits(insn, 48, 48) != 0,
        sat: bits(insn, 50, 50) != 0,
    }
}

fn decode_ffma(insn: u64, src_b: SrcB, src_c: SrcC) -> Instruction {
    Instruction::Ffma {
        dst: dst_reg(insn),
        src_a: src_a_reg(insn),
        src_b,
        src_c,
        neg_b: bits(insn, 48, 48) != 0,
        neg_c: bits(insn, 49, 49) != 0,
        sat: bits(insn, 50, 50) != 0,
    }
}

fn decode_sel(insn: u64, src_b: SrcB) -> Instruction {
    Instruction::Sel {
        dst: dst_reg(insn),
        src_a: src_a_reg(insn),
        src_b,
        pred: bits(insn, 39, 41) as u8,
        neg_pred: bits(insn, 42, 42) != 0,
    }
}

fn decode_fsetp(insn: u64, src_b: SrcB) -> Instruction {
    Instruction::Fsetp {
        pred_a: bits(insn, 3, 5) as u8,
        pred_b: bits(insn, 0, 2) as u8,
        src_a: src_a_reg(insn),
        src_b,
        compare: FpCompareOp::from_bits(bits(insn, 48, 51) as u8),
        bop: BoolOp::from_bits(bits(insn, 45, 46) as u8),
        bop_pred: bits(insn, 39, 41) as u8,
        neg_bop_pred: bits(insn, 42, 42) != 0,
        neg_a: bits(insn, 43, 43) != 0,
        neg_b: bits(insn, 6, 6) != 0,
        abs_a: bits(insn, 7, 7) != 0,
        abs_b: bits(insn, 44, 44) != 0,
        ftz: bits(insn, 47, 47) != 0,
    }
}

fn decode_isetp(insn: u64, src_b: SrcB) -> Instruction {
    Instruction::Isetp {
        pred_a: bits(insn, 3, 5) as u8,
        pred_b: bits(insn, 0, 2) as u8,
        src_a: src_a_reg(insn),
        src_b,
        compare: IntCompareOp::from_bits(bits(insn, 49, 51) as u8),
        bop: BoolOp::from_bits(bits(insn, 45, 46) as u8),
        bop_pred: bits(insn, 39, 41) as u8,
        neg_bop_pred: bits(insn, 42, 42) != 0,
        is_signed: bits(insn, 48, 48) != 0,
    }
}

fn decode_iadd(insn: u64, src_b: SrcB) -> Instruction {
    Instruction::Iadd {
        dst: dst_reg(insn),
        src_a: src_a_reg(insn),
        src_b,
        neg_a: bits(insn, 49, 49) != 0,
        neg_b: bits(insn, 48, 48) != 0,
    }
}

/// Extract cbuf as SrcC for FFMA_rc variant.
fn cbuf_src_c(insn: u64) -> SrcC {
    let index = bits(insn, 34, 38) as u8;
    let offset = bits(insn, 20, 33) as u16;
    SrcC::Cbuf { index, offset }
}

fn decode_xmad(insn: u64, src_b: SrcB, src_c: SrcC) -> Instruction {
    Instruction::Xmad {
        dst: dst_reg(insn),
        src_a: src_a_reg(insn),
        src_b,
        src_c,
        mode: bits(insn, 50, 51) as u8,
        hi_a: bits(insn, 53, 53) != 0,
        hi_b: bits(insn, 52, 52) != 0,
        psl: bits(insn, 36, 36) != 0,
    }
}

fn decode_imnmx(insn: u64, src_b: SrcB) -> Instruction {
    Instruction::Imnmx {
        dst: dst_reg(insn),
        src_a: src_a_reg(insn),
        src_b,
        pred: bits(insn, 39, 41) as u8,
        neg_pred: bits(insn, 42, 42) != 0,
        is_signed: bits(insn, 48, 48) != 0,
    }
}

fn decode_fmnmx(insn: u64, src_b: SrcB) -> Instruction {
    Instruction::Fmnmx {
        dst: dst_reg(insn),
        src_a: src_a_reg(insn),
        src_b,
        pred: bits(insn, 39, 41) as u8,
        neg_pred: bits(insn, 42, 42) != 0,
        abs_a: bits(insn, 46, 46) != 0,
        abs_b: bits(insn, 49, 49) != 0,
        neg_a: bits(insn, 48, 48) != 0,
        neg_b: bits(insn, 45, 45) != 0,
    }
}

fn decode_shf(insn: u64, src_b: SrcB, direction_right: bool) -> Instruction {
    Instruction::Shf {
        dst: dst_reg(insn),
        src_a: src_a_reg(insn),
        src_b,
        src_c: SrcC::Reg(src_c_reg(insn)),
        direction_right,
        data_type_u64: bits(insn, 48, 48) != 0,
        hi: bits(insn, 49, 49) != 0,
    }
}

fn decode_iset(insn: u64, src_b: SrcB) -> Instruction {
    Instruction::Iset {
        dst: dst_reg(insn),
        src_a: src_a_reg(insn),
        src_b,
        compare: IntCompareOp::from_bits(bits(insn, 49, 51) as u8),
        bop: BoolOp::from_bits(bits(insn, 45, 46) as u8),
        bop_pred: bits(insn, 39, 41) as u8,
        neg_bop_pred: bits(insn, 42, 42) != 0,
        is_signed: bits(insn, 48, 48) != 0,
        bf: bits(insn, 44, 44) != 0,
    }
}

fn decode_fset(insn: u64, src_b: SrcB) -> Instruction {
    Instruction::Fset {
        dst: dst_reg(insn),
        src_a: src_a_reg(insn),
        src_b,
        compare: FpCompareOp::from_bits(bits(insn, 48, 51) as u8),
        bop: BoolOp::from_bits(bits(insn, 45, 46) as u8),
        bop_pred: bits(insn, 39, 41) as u8,
        neg_bop_pred: bits(insn, 42, 42) != 0,
        bf: bits(insn, 52, 52) != 0,
    }
}

fn decode_f2f(insn: u64, src_b: SrcB) -> Instruction {
    Instruction::F2f {
        dst: dst_reg(insn),
        src_b,
        abs_b: bits(insn, 49, 49) != 0,
        neg_b: bits(insn, 45, 45) != 0,
        sat: bits(insn, 50, 50) != 0,
        dst_size: bits(insn, 8, 9) as u8,
        src_size: bits(insn, 10, 11) as u8,
        rounding: bits(insn, 39, 40) as u8,
    }
}

fn decode_i2i(insn: u64, src_b: SrcB) -> Instruction {
    Instruction::I2i {
        dst: dst_reg(insn),
        src_b,
        abs_b: bits(insn, 49, 49) != 0,
        sat: bits(insn, 50, 50) != 0,
        dst_signed: bits(insn, 12, 12) != 0,
        src_signed: bits(insn, 13, 13) != 0,
        dst_size: bits(insn, 8, 9) as u8,
        src_size: bits(insn, 10, 11) as u8,
    }
}

fn decode_iadd3(insn: u64, src_b: SrcB) -> Instruction {
    Instruction::Iadd3 {
        dst: dst_reg(insn),
        src_a: src_a_reg(insn),
        src_b,
        src_c: SrcC::Reg(src_c_reg(insn)),
        neg_a: bits(insn, 51, 51) != 0,
        neg_b: bits(insn, 50, 50) != 0,
        neg_c: bits(insn, 49, 49) != 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bits_extraction() {
        assert_eq!(bits(0xFF00_0000_0000_0000, 56, 63), 0xFF);
        assert_eq!(bits(0x0000_0000_0000_00FF, 0, 7), 0xFF);
        assert_eq!(bits(0x0000_0000_000F_0000, 16, 19), 0xF);
    }

    #[test]
    fn test_predicate_guard() {
        // PT (index=7, no negate)
        let insn: u64 = 0x0000_0000_0007_0000;
        let guard = PredicateGuard::from_insn(insn);
        assert_eq!(guard.index, 7);
        assert!(!guard.negated);
        assert!(guard.eval(&[false; 8])); // PT always true

        // P0 negated
        let insn2: u64 = 0x0000_0000_0008_0000; // bit 19 set = negate, index=0
        let guard2 = PredicateGuard::from_insn(insn2);
        assert_eq!(guard2.index, 0);
        assert!(guard2.negated);
        let mut preds = [false; 8];
        assert!(guard2.eval(&preds)); // !false = true
        preds[0] = true;
        assert!(!guard2.eval(&preds)); // !true = false
    }

    #[test]
    fn test_decode_exit() {
        let exit: u64 = 0xE300_0000_0007_0000; // EXIT with PT guard
        let (insn, guard) = decode(exit);
        assert!(matches!(insn, Instruction::Exit));
        assert_eq!(guard.index, 7);
    }

    #[test]
    fn test_decode_nop() {
        let nop: u64 = 0x50B0_0000_0007_0000;
        let (insn, _) = decode(nop);
        assert!(matches!(insn, Instruction::Nop));
    }

    #[test]
    fn test_decode_mov_reg() {
        // MOV R0, R1 — MOV_reg pattern: 0x5C98, dst=R0, src_b=R1
        let insn: u64 = 0x5C98_0000_0017_0100; // Simplified encoding
        let top16 = (insn >> 48) as u16;
        assert_eq!(top16 & 0xFFF8, 0x5C98);
        let (decoded, _) = decode(insn);
        match decoded {
            Instruction::Mov {
                dst,
                src_b: SrcB::Reg(r),
            } => {
                assert_eq!(dst, 0); // R0 at bits [7:0]
                assert_eq!(r, 1); // src_b reg at bits [27:20]
            }
            _ => panic!("Expected Mov, got {:?}", decoded),
        }
    }

    #[test]
    fn test_decode_mov32i() {
        // MOV32I R5, 0x3F800000 (1.0f)
        // Pattern: "0000 0001 0000 ----" => top16 = 0x010x
        let imm: u64 = 0x3F80_0000; // 1.0f
        let insn: u64 = (0x0100u64 << 48) | (imm << 20) | 0x0007_0005; // dst=R5, PT guard
        let (decoded, _) = decode(insn);
        match decoded {
            Instruction::Mov32i { dst, imm32 } => {
                assert_eq!(dst, 5);
                assert_eq!(imm32, 0x3F80_0000);
            }
            _ => panic!("Expected Mov32i, got {:?}", decoded),
        }
    }

    #[test]
    fn test_decode_fadd_reg() {
        // FADD_reg top16: 0x5C58
        let insn: u64 = 0x5C58_0000_0007_0201; // dst=R1, src_a=R2, PT
        let (decoded, _) = decode(insn);
        match decoded {
            Instruction::Fadd { dst, src_a, .. } => {
                assert_eq!(dst, 1);
                assert_eq!(src_a, 2);
            }
            _ => panic!("Expected Fadd, got {:?}", decoded),
        }
    }

    #[test]
    fn test_decode_s2r() {
        // S2R R0, SR_TID_X (33)
        // Pattern: 0xF0C8
        let insn: u64 = (0xF0C8u64 << 48) | ((33u64) << 20) | 0x0007_0000;
        let (decoded, _) = decode(insn);
        match decoded {
            Instruction::S2r { dst, sys_reg } => {
                assert_eq!(dst, 0);
                assert_eq!(sys_reg, SystemReg::TidX);
            }
            _ => panic!("Expected S2r, got {:?}", decoded),
        }
    }

    #[test]
    fn test_decode_bra() {
        // BRA +4 (offset=4)
        let insn: u64 = (0xE240u64 << 48) | (4u64 << 20) | 0x0007_0000;
        let (decoded, _) = decode(insn);
        match decoded {
            Instruction::Bra { offset } => {
                assert_eq!(offset, 4);
            }
            _ => panic!("Expected Bra, got {:?}", decoded),
        }

        // BRA -2 (negative offset — 24-bit sign-extended)
        let neg_offset: u32 = (-2i32 as u32) & 0x00FF_FFFF;
        let insn2: u64 = (0xE240u64 << 48) | ((neg_offset as u64) << 20) | 0x0007_0000;
        let (decoded2, _) = decode(insn2);
        match decoded2 {
            Instruction::Bra { offset } => {
                assert_eq!(offset, -2);
            }
            _ => panic!("Expected Bra, got {:?}", decoded2),
        }
    }

    #[test]
    fn test_fp_compare_ops() {
        assert!(FpCompareOp::LT.eval(1.0, 2.0));
        assert!(!FpCompareOp::LT.eval(2.0, 1.0));
        assert!(FpCompareOp::EQ.eval(1.0, 1.0));
        assert!(FpCompareOp::NE.eval(1.0, 2.0));
        assert!(FpCompareOp::NaN.eval(f32::NAN, 0.0));
        assert!(FpCompareOp::NUM.eval(1.0, 2.0));
        assert!(!FpCompareOp::NUM.eval(f32::NAN, 2.0));
    }

    #[test]
    fn test_int_compare_ops() {
        assert!(IntCompareOp::LT.eval_unsigned(1, 2));
        assert!(!IntCompareOp::LT.eval_unsigned(2, 1));
        assert!(IntCompareOp::EQ.eval_unsigned(5, 5));
        assert!(IntCompareOp::LT.eval_signed(-1, 0));
        assert!(!IntCompareOp::LT.eval_signed(0, -1));
    }

    #[test]
    fn test_decode_xmad_reg() {
        // XMAD_reg: top16 pattern 0x5B00
        let insn: u64 = (0x5B00u64 << 48) | (3u64 << 39) | (2u64 << 20) | 0x0007_0100;
        let (decoded, _) = decode(insn);
        match decoded {
            Instruction::Xmad {
                dst,
                src_a,
                src_b: SrcB::Reg(_),
                ..
            } => {
                assert_eq!(dst, 0);
                assert_eq!(src_a, 1);
            }
            _ => panic!("Expected Xmad, got {:?}", decoded),
        }
    }

    #[test]
    fn test_decode_imnmx_reg() {
        // IMNMX_reg: top16 pattern 0x5C20
        let insn: u64 = (0x5C20u64 << 48) | (2u64 << 20) | 0x0007_0100;
        let (decoded, _) = decode(insn);
        assert!(matches!(decoded, Instruction::Imnmx { .. }));
    }

    #[test]
    fn test_decode_fmnmx_reg() {
        // FMNMX_reg: top16 pattern 0x5C60
        let insn: u64 = (0x5C60u64 << 48) | (2u64 << 20) | 0x0007_0100;
        let (decoded, _) = decode(insn);
        assert!(matches!(decoded, Instruction::Fmnmx { .. }));
    }

    #[test]
    fn test_decode_bfe_reg() {
        // BFE_reg: top16 pattern 0x5C00
        let insn: u64 = (0x5C00u64 << 48) | (2u64 << 20) | 0x0007_0100;
        let (decoded, _) = decode(insn);
        assert!(matches!(decoded, Instruction::Bfe { .. }));
    }

    #[test]
    fn test_decode_popc_reg() {
        // POPC_reg: top16 pattern 0x5C08
        let insn: u64 = (0x5C08u64 << 48) | (2u64 << 20) | 0x0007_0000;
        let (decoded, _) = decode(insn);
        assert!(matches!(decoded, Instruction::Popc { .. }));
    }

    #[test]
    fn test_decode_flo_reg() {
        // FLO_reg: top16 pattern 0x5C30
        let insn: u64 = (0x5C30u64 << 48) | (2u64 << 20) | 0x0007_0000;
        let (decoded, _) = decode(insn);
        assert!(matches!(decoded, Instruction::Flo { .. }));
    }

    #[test]
    fn test_decode_lop3_reg() {
        // LOP3_reg: top16 pattern 0x5BE0
        let lut: u64 = 0xC0; // AND operation LUT
        let insn: u64 = (0x5BE0u64 << 48) | (3u64 << 39) | (lut << 28) | (2u64 << 20) | 0x0007_0100;
        let (decoded, _) = decode(insn);
        match decoded {
            Instruction::Lop3 { lut, .. } => {
                assert_eq!(lut, 0xC0u8);
            }
            _ => panic!("Expected Lop3, got {:?}", decoded),
        }
    }

    #[test]
    fn test_decode_ldg() {
        // LDG: top16 pattern 0xEED0
        let insn: u64 = (0xEED0u64 << 48) | (4u64 << 48) | (0u64 << 20) | 0x0007_0100;
        let top16 = ((insn >> 48) & 0xFFFF) as u16;
        // Verify the top bits match
        if top16 & 0xFFF0 == 0xEED0 {
            let (decoded, _) = decode(insn);
            assert!(matches!(decoded, Instruction::Ldg { .. }));
        }
    }

    #[test]
    fn test_decode_stg() {
        // STG: top16 pattern 0xEEF0
        let insn: u64 = (0xEEF0u64 << 48) | 0x0007_0100;
        let (decoded, _) = decode(insn);
        assert!(matches!(decoded, Instruction::Stg { .. }));
    }

    #[test]
    fn test_decode_iadd3_reg() {
        // IADD3_reg: top16 pattern 0x5CC0
        let insn: u64 = (0x5CC0u64 << 48) | (3u64 << 39) | (2u64 << 20) | 0x0007_0100;
        let (decoded, _) = decode(insn);
        assert!(matches!(decoded, Instruction::Iadd3 { .. }));
    }

    #[test]
    fn test_decode_depbar() {
        // DEPBAR: top16 pattern 0xF0F0 → NOP
        let insn: u64 = (0xF0F0u64 << 48) | 0x0007_0000;
        let (decoded, _) = decode(insn);
        assert!(matches!(decoded, Instruction::Nop));
    }
}
