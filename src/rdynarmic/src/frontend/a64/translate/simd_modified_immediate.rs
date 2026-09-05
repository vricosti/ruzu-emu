//! Port of upstream
//! `dynarmic/frontend/A64/translate/impl/simd_modified_immediate.cpp`.
//!
//! Implements MOVI / MVNI / ORR (vector, immediate) / BIC (vector, immediate),
//! which all share the encoding `0Qo0111100000abcmmmm01defghddddd`. The
//! sub-operation is selected by `cmode:op`.

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;
use crate::ir::value::Value;

/// Matches upstream `AdvSIMDExpandImm(op, cmode, imm8)` from
/// `dynarmic/frontend/imm.cpp`. Expands an 8-bit immediate + cmode + op
/// into the 64-bit element value used by MOVI/MVNI/ORR-imm/BIC-imm.
fn adv_simd_expand_imm(op: bool, cmode: u32, imm8: u32) -> u64 {
    let byte = (imm8 & 0xFF) as u64;
    let cmode_hi3 = (cmode >> 1) & 0x7;
    let cmode_lo = cmode & 1;

    let rep32 = |v: u64| -> u64 { (v & 0xFFFF_FFFF) | ((v & 0xFFFF_FFFF) << 32) };
    let rep16 = |v: u64| -> u64 {
        let v = v & 0xFFFF;
        v | (v << 16) | (v << 32) | (v << 48)
    };
    let rep8 = |v: u64| -> u64 {
        let v = v & 0xFF;
        v.wrapping_mul(0x0101_0101_0101_0101)
    };

    match cmode_hi3 {
        0b000 => rep32(byte),
        0b001 => rep32(byte << 8),
        0b010 => rep32(byte << 16),
        0b011 => rep32(byte << 24),
        0b100 => rep16(byte),
        0b101 => rep16(byte << 8),
        0b110 => {
            if cmode_lo == 0 {
                rep32((byte << 8) | 0xFF)
            } else {
                rep32((byte << 16) | 0xFFFF)
            }
        }
        0b111 => {
            if cmode_lo == 0 && !op {
                rep8(byte)
            } else if cmode_lo == 0 && op {
                let mut result = 0u64;
                for i in 0..8 {
                    if (imm8 >> i) & 1 != 0 {
                        result |= 0xFFu64 << (i * 8);
                    }
                }
                result
            } else if cmode_lo == 1 && !op {
                let mut r = 0u32;
                if (imm8 >> 7) & 1 != 0 {
                    r |= 0x8000_0000;
                }
                if (imm8 >> 6) & 1 != 0 {
                    r |= 0x3E00_0000;
                } else {
                    r |= 0x4000_0000;
                }
                r |= ((imm8 & 0x3F) as u32) << 19;
                rep32(r as u64)
            } else {
                let mut r = 0u64;
                if (imm8 >> 7) & 1 != 0 {
                    r |= 0x8000_0000_0000_0000;
                }
                if (imm8 >> 6) & 1 != 0 {
                    r |= 0x3FC0_0000_0000_0000;
                } else {
                    r |= 0x4000_0000_0000_0000;
                }
                r |= ((imm8 & 0x3F) as u64) << 48;
                r
            }
        }
        _ => unreachable!(),
    }
}

impl<'a> TranslatorVisitor<'a> {
    /// MOVI / MVNI / ORR (vector, immediate) / BIC (vector, immediate).
    /// Encoding: `0 Q o 0 1 1 1 1 0 0 0 0 0 a b c cmode 0 1 d e f g h Rd`.
    pub fn movi(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let op = inst.bit(29);
        let abc = inst.bits(18, 16);
        let cmode = inst.bits(15, 12);
        let defgh = inst.bits(9, 5);
        let imm8 = (abc << 5) | defgh;
        let vd = Vec::from_u32(inst.rd());

        let datasize = if q { 128 } else { 64 };

        // Build the 128-bit immediate: replicate imm64 across both lanes
        // when datasize==128, zero-extend into low 64 when datasize==64.
        let make_imm128 = |v: &mut TranslatorVisitor, imm64: u64| -> Value {
            let imm = v.ir.ir().imm64(imm64);
            if datasize == 64 {
                v.ir.ir().zero_extend_long_to_quad(imm)
            } else {
                v.ir.ir().vector_broadcast(64, imm)
            }
        };

        // sub-op selector matches upstream's `concatenate(cmode, op)` table.
        // cmode<3:1> already groups the variants; cmode<0> + op give the
        // MOVI vs MVNI vs ORR vs BIC distinction.
        match (cmode, op) {
            // MOVI (32-bit shifted, 16-bit shifted, 8-bit, 32-bit shifted-ones, 64-bit, F32, F64)
            (0b0000, false)
            | (0b0010, false)
            | (0b0100, false)
            | (0b0110, false)
            | (0b1000, false)
            | (0b1010, false)
            | (0b1100, false)
            | (0b1101, false)
            | (0b1110, false)
            | (0b1110, true)
            | (0b1111, false) => {
                let imm64 = adv_simd_expand_imm(op, cmode, imm8);
                let imm = make_imm128(self, imm64);
                self.ir.set_q(vd, imm);
                true
            }
            (0b1111, true) => {
                if !q {
                    return self.unallocated_encoding();
                }
                let imm64 = adv_simd_expand_imm(op, cmode, imm8);
                let imm = make_imm128(self, imm64);
                self.ir.set_q(vd, imm);
                true
            }
            // MVNI
            (0b0000, true)
            | (0b0010, true)
            | (0b0100, true)
            | (0b0110, true)
            | (0b1000, true)
            | (0b1010, true)
            | (0b1100, true)
            | (0b1101, true) => {
                let imm64 = !adv_simd_expand_imm(op, cmode, imm8);
                let imm = make_imm128(self, imm64);
                self.ir.set_q(vd, imm);
                true
            }
            // ORR (vector, immediate)
            (0b0001, false)
            | (0b0011, false)
            | (0b0101, false)
            | (0b0111, false)
            | (0b1001, false)
            | (0b1011, false) => {
                let imm64 = adv_simd_expand_imm(op, cmode, imm8);
                let imm = make_imm128(self, imm64);
                let operand = self.ir.get_q(vd);
                let result = self.ir.ir().vector_or(operand, imm);
                self.ir.set_q(vd, result);
                true
            }
            // BIC (vector, immediate)
            (0b0001, true)
            | (0b0011, true)
            | (0b0101, true)
            | (0b0111, true)
            | (0b1001, true)
            | (0b1011, true) => {
                let imm64 = !adv_simd_expand_imm(op, cmode, imm8);
                let imm = make_imm128(self, imm64);
                let operand = self.ir.get_q(vd);
                let result = self.ir.ir().vector_and(operand, imm);
                self.ir.set_q(vd, result);
                true
            }
            _ => self.unallocated_encoding(),
        }
    }

    /// FMOV (vector, immediate) — single-precision (cmode forced to 0b1111).
    /// Encoding: `0 Q op 0 1 1 1 1 0 0 0 0 0 a b c 1 1 1 1 0 1 d e f g h Rd`.
    pub fn fmov_2(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let op = inst.bit(29);
        let abc = inst.bits(18, 16);
        let defgh = inst.bits(9, 5);
        let imm8 = (abc << 5) | defgh;
        let vd = Vec::from_u32(inst.rd());

        if op && !q {
            return self.unallocated_encoding();
        }

        let datasize = if q { 128 } else { 64 };
        let imm64 = adv_simd_expand_imm(op, 0b1111, imm8);
        let imm_val = self.ir.ir().imm64(imm64);
        let imm = if datasize == 64 {
            self.ir.ir().zero_extend_long_to_quad(imm_val)
        } else {
            self.ir.ir().vector_broadcast(64, imm_val)
        };
        self.ir.set_q(vd, imm);
        true
    }

    /// FMOV (vector, immediate) — half-precision.
    /// Encoding: `0 Q 0 0 1 1 1 1 0 0 0 0 0 a b c 1 1 1 1 1 1 d e f g h Rd`.
    pub fn fmov_3(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let abc = inst.bits(18, 16);
        let defgh = inst.bits(9, 5);
        let imm8 = (abc << 5) | defgh;
        let vd = Vec::from_u32(inst.rd());

        let datasize = if q { 128 } else { 64 };

        // Build a half-precision immediate from imm8, then replicate it across
        // the 64-bit element (matches upstream's replicate_element<u16, u64>).
        let mut imm16: u16 = 0;
        if (imm8 >> 7) & 1 != 0 {
            imm16 |= 0x8000;
        }
        imm16 |= if (imm8 >> 6) & 1 != 0 { 0x3000 } else { 0x4000 };
        imm16 |= ((imm8 & 0x3F) as u16) << 6;
        let imm16 = imm16 as u64;
        let imm64 = imm16 | (imm16 << 16) | (imm16 << 32) | (imm16 << 48);

        let imm_val = self.ir.ir().imm64(imm64);
        let imm = if datasize == 64 {
            self.ir.ir().zero_extend_long_to_quad(imm_val)
        } else {
            self.ir.ir().vector_broadcast(64, imm_val)
        };
        self.ir.set_q(vd, imm);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::adv_simd_expand_imm;
    use crate::frontend::a64::decoder::decode;
    use crate::frontend::a64::translate::visitor::TranslatorVisitor;
    use crate::ir::block::Block;
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;

    fn translate_one(raw: u32) -> (Block, bool) {
        let decoded = decode(raw).expect("instruction should decode");
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let mut visitor = TranslatorVisitor::new(
            &mut block,
            A64LocationDescriptor::new(0x1000, 0, false),
            crate::frontend::a64::translate::TranslationOptions::default(),
        );
        let should_continue = visitor.dispatch(&decoded);
        drop(visitor);
        (block, should_continue)
    }

    #[test]
    fn fmov_v0_4s_imm_4f03f600_translates() {
        // AnimH: `FMOV V0.4S, #<fpimm>` = 0x4F03F600 (cmode=0b1111,
        // op=0, Q=1). Must decode to MOVI and translate to a vector immediate,
        // NOT fall back to the interpreter (PrefetchAbort at runtime).
        let (_block, should_continue) = translate_one(0x4F03F600);
        assert!(should_continue);
    }

    #[test]
    fn test_movi_4s_zero_imm() {
        // STK encoding `MOVI V31.4S, #0` = 0x4F00041F. cmode=0b0000, op=0,
        // imm8=0. Upstream's AdvSIMDExpandImm returns 0 for any all-zero
        // 32-bit-shifted MOVI variant. The 128-bit replicated value is
        // therefore 0 in both lanes. Regression: if `MOVI V31.4S, #0` is
        // mistranslated (or skipped), STK's libnx mutex region at NRO
        // 0x80773094 gets stale `0xFF...FF` from V31, breaking
        // `svcArbitrateLock` after fsp-srv init.
        assert_eq!(adv_simd_expand_imm(false, 0b0000, 0), 0);
    }

    #[test]
    fn test_movi_8b_all_ones() {
        // MOVI Vd.8B, #0xFF — cmode=0b1110, op=0 → replicate byte.
        assert_eq!(
            adv_simd_expand_imm(false, 0b1110, 0xFF),
            0xFFFF_FFFF_FFFF_FFFF
        );
    }

    #[test]
    fn test_mvni_4s_zero_becomes_all_ones() {
        // MVNI uses ~AdvSIMDExpandImm.
        let v = adv_simd_expand_imm(false, 0b0000, 0);
        assert_eq!(!v, 0xFFFF_FFFF_FFFF_FFFF);
    }

    #[test]
    fn movi_d28_zero_stk_encoding_translates_without_exception_path() {
        let (block, should_continue) = translate_one(0x2f00_e41c);

        assert!(should_continue);
        assert!(
            block
                .instructions
                .iter()
                .any(|insn| insn.opcode == Opcode::ZeroExtendLongToQuad),
            "expected MOVI d28, #0 to materialize a 64-bit immediate and zero-extend it"
        );
    }
}
