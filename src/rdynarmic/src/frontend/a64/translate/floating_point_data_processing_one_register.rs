use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;

const ROUND_TO_NEAREST_TIE_EVEN: u8 = 0;
const ROUND_TOWARDS_PLUS_INFINITY: u8 = 1;
const ROUND_TOWARDS_MINUS_INFINITY: u8 = 2;
const ROUND_TOWARDS_ZERO: u8 = 3;
const ROUND_TO_NEAREST_TIE_AWAY_FROM_ZERO: u8 = 4;

fn current_fpcr_rounding_mode(visitor: &TranslatorVisitor<'_>) -> u8 {
    ((visitor
        .ir
        .current_location
        .expect("current_location not set")
        .fpcr()
        >> 22)
        & 0x3) as u8
}

fn floating_point_round_to_integral(
    visitor: &mut TranslatorVisitor<'_>,
    inst: &DecodedInst,
    rounding_mode: u8,
    exact: bool,
) -> bool {
    let ftype = inst.bits(23, 22);
    let vn = Vec::from_u32(inst.rn());
    let vd = Vec::from_u32(inst.rd());

    let datasize = match visitor.fp_datasize(ftype) {
        Some(size) => size,
        None => return visitor.unallocated_encoding(),
    };

    let operand = visitor.v_scalar_read(datasize, vn);
    let result = visitor
        .ir
        .ir()
        .fp_round_int(datasize, operand, rounding_mode, exact);
    visitor.v_scalar_write(datasize, vd, result);
    true
}

impl<'a> TranslatorVisitor<'a> {
    pub fn fmov_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());

        let datasize = match self.fp_datasize(ftype) {
            Some(size) => size,
            None => return self.unallocated_encoding(),
        };

        let operand = self.v_scalar_read(datasize, vn);
        self.v_scalar_write(datasize, vd, operand);
        true
    }

    pub fn fabs_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());

        let datasize = match self.fp_datasize(ftype) {
            Some(size) => size,
            None => return self.unallocated_encoding(),
        };

        let operand = self.v_scalar_read(datasize, vn);
        let result = self.ir.ir().fp_abs(datasize, operand);
        self.v_scalar_write(datasize, vd, result);
        true
    }

    pub fn fneg_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());

        let datasize = match self.fp_datasize(ftype) {
            Some(size) => size,
            None => return self.unallocated_encoding(),
        };

        let operand = self.v_scalar_read(datasize, vn);
        let result = self.ir.ir().fp_neg(datasize, operand);
        self.v_scalar_write(datasize, vd, result);
        true
    }

    pub fn fsqrt_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());

        let datasize = match self.fp_datasize(ftype) {
            Some(size) if size != 16 => size,
            _ => return self.unallocated_encoding(),
        };

        let operand = self.v_scalar_read(datasize, vn);
        let result = self.ir.ir().fp_sqrt(datasize, operand);
        self.v_scalar_write(datasize, vd, result);
        true
    }

    pub fn fmov_float_imm(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let imm8 = inst.bits(20, 13) as u8;
        let vd = Vec::from_u32(inst.rd());

        let datasize = match self.fp_datasize(ftype) {
            Some(size) => size,
            None => return self.unallocated_encoding(),
        };

        let result = match datasize {
            16 => {
                let sign = if (imm8 >> 7) & 1 != 0 { 1u16 } else { 0u16 };
                let exp = (if (imm8 >> 6) & 1 != 0 {
                    0b0_1100u16
                } else {
                    0b1_0000u16
                }) | (((imm8 >> 4) & 0x3) as u16);
                let fract = ((imm8 & 0xF) as u16) << 6;
                self.ir.ir().imm16((sign << 15) | (exp << 10) | fract)
            }
            32 => {
                let sign = if (imm8 >> 7) & 1 != 0 { 1u32 } else { 0u32 };
                let exp = (if (imm8 >> 6) & 1 != 0 {
                    0b0111_1100u32
                } else {
                    0b1000_0000u32
                }) | (((imm8 >> 4) & 0x3) as u32);
                let fract = ((imm8 & 0xF) as u32) << 19;
                self.ir.ir().imm32((sign << 31) | (exp << 23) | fract)
            }
            64 => {
                let sign = if (imm8 >> 7) & 1 != 0 { 1u64 } else { 0u64 };
                let exp = (if (imm8 >> 6) & 1 != 0 {
                    0b011_1111_1100u64
                } else {
                    0b100_0000_0000u64
                }) | (((imm8 >> 4) & 0x3) as u64);
                let fract = ((imm8 & 0xF) as u64) << 48;
                self.ir.ir().imm64((sign << 63) | (exp << 52) | fract)
            }
            _ => unreachable!(),
        };

        self.v_scalar_write(datasize, vd, result);
        true
    }

    pub fn fcvt_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let opc = inst.bits(16, 15);
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());

        if ftype == opc {
            return self.unallocated_encoding();
        }

        let srcsize = match self.fp_datasize(ftype) {
            Some(size) => size,
            None => return self.unallocated_encoding(),
        };
        let dstsize = match self.fp_datasize(opc) {
            Some(size) => size,
            None => return self.unallocated_encoding(),
        };

        let operand = self.v_scalar_read(srcsize, vn);
        let rounding_mode = current_fpcr_rounding_mode(self);

        let result = match (srcsize, dstsize) {
            (16, 32) => self.ir.ir().fp_half_to_single(operand, rounding_mode),
            (16, 64) => self.ir.ir().fp_half_to_double(operand, rounding_mode),
            (32, 16) => self.ir.ir().fp_single_to_half(operand, rounding_mode),
            (32, 64) => self.ir.ir().fp_single_to_double(operand, rounding_mode),
            (64, 16) => self.ir.ir().fp_double_to_half(operand, rounding_mode),
            (64, 32) => self.ir.ir().fp_double_to_single(operand, rounding_mode),
            _ => unreachable!(),
        };

        self.v_scalar_write(dstsize, vd, result);
        true
    }

    pub fn frintn_float(&mut self, inst: &DecodedInst) -> bool {
        floating_point_round_to_integral(self, inst, ROUND_TO_NEAREST_TIE_EVEN, false)
    }

    pub fn frintp_float(&mut self, inst: &DecodedInst) -> bool {
        floating_point_round_to_integral(self, inst, ROUND_TOWARDS_PLUS_INFINITY, false)
    }

    pub fn frintm_float(&mut self, inst: &DecodedInst) -> bool {
        floating_point_round_to_integral(self, inst, ROUND_TOWARDS_MINUS_INFINITY, false)
    }

    pub fn frintz_float(&mut self, inst: &DecodedInst) -> bool {
        floating_point_round_to_integral(self, inst, ROUND_TOWARDS_ZERO, false)
    }

    pub fn frinta_float(&mut self, inst: &DecodedInst) -> bool {
        floating_point_round_to_integral(self, inst, ROUND_TO_NEAREST_TIE_AWAY_FROM_ZERO, false)
    }

    pub fn frintx_float(&mut self, inst: &DecodedInst) -> bool {
        floating_point_round_to_integral(self, inst, current_fpcr_rounding_mode(self), true)
    }

    pub fn frinti_float(&mut self, inst: &DecodedInst) -> bool {
        floating_point_round_to_integral(self, inst, current_fpcr_rounding_mode(self), false)
    }
}

#[cfg(test)]
mod tests {
    use crate::frontend::a64::translate::{translate, TranslationOptions};
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::terminal::Terminal;

    fn translate_single(raw: u32) -> crate::ir::block::Block {
        let loc = A64LocationDescriptor::new(0x1000, 0, true);
        translate(
            loc,
            &move |pc: u64| {
                if pc == 0x1000 {
                    Some(raw)
                } else {
                    None
                }
            },
            TranslationOptions::default(),
        )
    }

    #[test]
    fn fmov_float_translates_without_interpret_terminal() {
        let block = translate_single(0x1E204020);
        assert!(!block.instructions.is_empty());
    }

    #[test]
    fn fsqrt_float_translates_without_interpret_terminal() {
        let block = translate_single(0x1E21C020);
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, Opcode::FPSqrt32 | Opcode::FPSqrt64)));
    }

    #[test]
    fn fsqrt_float_half_precision_rejects_as_unallocated() {
        let block = translate_single(0x1EE1C020);
        assert!(!matches!(block.terminal, Terminal::LinkBlock { .. }));
        assert!(block
            .instructions
            .iter()
            .all(|inst| !matches!(inst.opcode, Opcode::FPSqrt32 | Opcode::FPSqrt64)));
    }

    #[test]
    fn fmov_float_imm_translates_without_interpret_terminal() {
        let block = translate_single(0x1E2E1000);
        assert!(!block.instructions.is_empty());
    }

    #[test]
    fn fcvt_float_translates_without_interpret_terminal() {
        let block = translate_single(0x1E22C020);
        assert!(block.instructions.iter().any(|inst| {
            matches!(
                inst.opcode,
                Opcode::FPHalfToSingle
                    | Opcode::FPHalfToDouble
                    | Opcode::FPSingleToHalf
                    | Opcode::FPSingleToDouble
                    | Opcode::FPDoubleToHalf
                    | Opcode::FPDoubleToSingle
            )
        }));
    }

    #[test]
    fn frintn_float_translates_without_interpret_terminal() {
        let block = translate_single(0x1E244020);
        assert!(block.instructions.iter().any(|inst| {
            matches!(
                inst.opcode,
                Opcode::FPRoundInt16 | Opcode::FPRoundInt32 | Opcode::FPRoundInt64
            )
        }));
    }

    #[test]
    fn frinta_float_uses_tie_away_rounding_mode() {
        let block = translate_single(0x1E264020);
        let round_inst = block
            .instructions
            .iter()
            .find(|inst| {
                matches!(
                    inst.opcode,
                    Opcode::FPRoundInt16 | Opcode::FPRoundInt32 | Opcode::FPRoundInt64
                )
            })
            .expect("missing FPRoundInt instruction");
        assert_eq!(
            round_inst.arg(1).get_u8(),
            super::ROUND_TO_NEAREST_TIE_AWAY_FROM_ZERO
        );
        assert!(!round_inst.arg(2).get_u1());
    }
}
