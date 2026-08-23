use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::{Reg, Vec};

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

fn scalar_float_int_datasize(visitor: &mut TranslatorVisitor<'_>, ftype: u32) -> Option<usize> {
    match visitor.fp_datasize(ftype) {
        Some(size) if size != 16 => Some(size),
        _ => None,
    }
}

fn floating_point_convert_signed_integer(
    visitor: &mut TranslatorVisitor<'_>,
    sf: bool,
    ftype: u32,
    vn: Vec,
    rd: Reg,
    rounding_mode: u8,
) -> bool {
    let intsize = if sf { 64 } else { 32 };
    let fltsize = match visitor.fp_datasize(ftype) {
        Some(size) => size,
        None => return visitor.unallocated_encoding(),
    };

    let fltval = visitor.v_scalar_read(fltsize, vn);
    let intval = match intsize {
        32 => visitor
            .ir
            .ir()
            .fp_to_fixed_s32(fltval, fltsize, 0, rounding_mode),
        64 => visitor
            .ir
            .ir()
            .fp_to_fixed_s64(fltval, fltsize, 0, rounding_mode),
        _ => unreachable!(),
    };
    visitor.set_x(intsize, rd, intval);
    true
}

fn floating_point_convert_unsigned_integer(
    visitor: &mut TranslatorVisitor<'_>,
    sf: bool,
    ftype: u32,
    vn: Vec,
    rd: Reg,
    rounding_mode: u8,
) -> bool {
    let intsize = if sf { 64 } else { 32 };
    let fltsize = match visitor.fp_datasize(ftype) {
        Some(size) => size,
        None => return visitor.unallocated_encoding(),
    };

    let fltval = visitor.v_scalar_read(fltsize, vn);
    let intval = match intsize {
        32 => visitor
            .ir
            .ir()
            .fp_to_fixed_u32(fltval, fltsize, 0, rounding_mode),
        64 => visitor
            .ir
            .ir()
            .fp_to_fixed_u64(fltval, fltsize, 0, rounding_mode),
        _ => unreachable!(),
    };
    visitor.set_x(intsize, rd, intval);
    true
}

impl<'a> TranslatorVisitor<'a> {
    pub fn scvtf_float_int(&mut self, inst: &DecodedInst) -> bool {
        let sf = inst.sf();
        let ftype = inst.bits(23, 22);
        let rn = Reg::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());

        let intsize = if sf { 64 } else { 32 };
        let fltsize = match scalar_float_int_datasize(self, ftype) {
            Some(size) => size,
            None => return self.unallocated_encoding(),
        };
        let rounding_mode = current_fpcr_rounding_mode(self);

        let intval = self.x(intsize, rn);
        let fltval = match fltsize {
            32 => self
                .ir
                .ir()
                .fp_fixed_to_single(intval, intsize, true, 0, rounding_mode),
            64 => self
                .ir
                .ir()
                .fp_fixed_to_double(intval, intsize, true, 0, rounding_mode),
            _ => unreachable!(),
        };
        self.v_scalar_write(fltsize, vd, fltval);
        true
    }

    pub fn ucvtf_float_int(&mut self, inst: &DecodedInst) -> bool {
        let sf = inst.sf();
        let ftype = inst.bits(23, 22);
        let rn = Reg::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());

        let intsize = if sf { 64 } else { 32 };
        let fltsize = match scalar_float_int_datasize(self, ftype) {
            Some(size) => size,
            None => return self.unallocated_encoding(),
        };
        let rounding_mode = current_fpcr_rounding_mode(self);

        let intval = self.x(intsize, rn);
        let fltval = match fltsize {
            32 => self
                .ir
                .ir()
                .fp_fixed_to_single(intval, intsize, false, 0, rounding_mode),
            64 => self
                .ir
                .ir()
                .fp_fixed_to_double(intval, intsize, false, 0, rounding_mode),
            _ => unreachable!(),
        };
        self.v_scalar_write(fltsize, vd, fltval);
        true
    }

    pub fn fmov_float_gen(&mut self, inst: &DecodedInst) -> bool {
        let sf = inst.sf();
        let ftype = inst.bits(23, 22);
        let rmode_0 = inst.bit(19);
        let opc_0 = inst.bit(16);
        let n = inst.rn();
        let d = inst.rd();

        if ftype == 0b10 && !rmode_0 {
            return self.unallocated_encoding();
        }

        let intsize = if sf { 64 } else { 32 };
        let mut fltsize = match ftype {
            0b00 => 32,
            0b01 => 64,
            0b10 => 128,
            0b11 => 16,
            _ => unreachable!(),
        };

        let integer_to_float;
        let part;
        if !rmode_0 {
            if fltsize != 16 && fltsize != intsize {
                return self.unallocated_encoding();
            }
            integer_to_float = opc_0;
            part = 0;
        } else {
            if intsize != 64 || fltsize != 128 {
                return self.unallocated_encoding();
            }
            integer_to_float = opc_0;
            part = 1;
            fltsize = 64;
        }

        if integer_to_float {
            let value = if part == 0 {
                match fltsize {
                    16 => {
                        let intval = self.x(32, Reg::from_u32(n));
                        self.ir.ir().least_significant_half(intval)
                    }
                    32 | 64 => self.x(fltsize, Reg::from_u32(n)),
                    _ => unreachable!(),
                }
            } else {
                self.x(64, Reg::from_u32(n))
            };

            if part == 0 {
                self.v_scalar_write(fltsize, Vec::from_u32(d), value);
            } else {
                let vec = self.ir.get_q(Vec::from_u32(d));
                let result = self.ir.ir().vector_set_element(64, vec, 1, value);
                self.ir.set_q(Vec::from_u32(d), result);
            }
        } else {
            // Mirror upstream `Vpart_scalar(fltsize, vec, part)`
            // (impl.cpp:202-210): the `part==0`/`fltsize<128` read MUST
            // extract the lane via `VectorGetElement(fltsize, GetQ(vec), part)`
            // — using `v_scalar_read` (which delegates to GetD/GetS and
            // returns U128) instead would feed a 128-bit value to the U64
            // `set_x` below, propagating a bogus type through the IR. That
            // type mismatch silently worked when `Inst::return_type()` for
            // Identity returned `Opaque → 64`, but breaks the moment the
            // regalloc sizes operations off the real underlying width
            // (e.g. spilling/reloading the vector with movaps would
            // corrupt the X-register write).
            let vec = self.ir.get_q(Vec::from_u32(n));
            let value = self.ir.ir().vector_get_element(fltsize, vec, part);

            let intval = match fltsize {
                16 => self.sign_or_zero_extend(value, 16, intsize, false),
                32 if intsize == 64 => self.ir.ir().zero_extend_word_to_long(value),
                _ => value,
            };
            self.set_x(intsize, Reg::from_u32(d), intval);
        }

        true
    }

    pub fn fcvtns_float(&mut self, inst: &DecodedInst) -> bool {
        floating_point_convert_signed_integer(
            self,
            inst.sf(),
            inst.bits(23, 22),
            Vec::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
            ROUND_TO_NEAREST_TIE_EVEN,
        )
    }

    pub fn fcvtnu_float(&mut self, inst: &DecodedInst) -> bool {
        floating_point_convert_unsigned_integer(
            self,
            inst.sf(),
            inst.bits(23, 22),
            Vec::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
            ROUND_TO_NEAREST_TIE_EVEN,
        )
    }

    pub fn fcvtzs_float_int(&mut self, inst: &DecodedInst) -> bool {
        floating_point_convert_signed_integer(
            self,
            inst.sf(),
            inst.bits(23, 22),
            Vec::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
            ROUND_TOWARDS_ZERO,
        )
    }

    pub fn fcvtzu_float_int(&mut self, inst: &DecodedInst) -> bool {
        floating_point_convert_unsigned_integer(
            self,
            inst.sf(),
            inst.bits(23, 22),
            Vec::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
            ROUND_TOWARDS_ZERO,
        )
    }

    pub fn fcvtas_float(&mut self, inst: &DecodedInst) -> bool {
        floating_point_convert_signed_integer(
            self,
            inst.sf(),
            inst.bits(23, 22),
            Vec::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
            ROUND_TO_NEAREST_TIE_AWAY_FROM_ZERO,
        )
    }

    pub fn fcvtau_float(&mut self, inst: &DecodedInst) -> bool {
        floating_point_convert_unsigned_integer(
            self,
            inst.sf(),
            inst.bits(23, 22),
            Vec::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
            ROUND_TO_NEAREST_TIE_AWAY_FROM_ZERO,
        )
    }

    pub fn fcvtps_float(&mut self, inst: &DecodedInst) -> bool {
        floating_point_convert_signed_integer(
            self,
            inst.sf(),
            inst.bits(23, 22),
            Vec::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
            ROUND_TOWARDS_PLUS_INFINITY,
        )
    }

    pub fn fcvtpu_float(&mut self, inst: &DecodedInst) -> bool {
        floating_point_convert_unsigned_integer(
            self,
            inst.sf(),
            inst.bits(23, 22),
            Vec::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
            ROUND_TOWARDS_PLUS_INFINITY,
        )
    }

    pub fn fcvtms_float(&mut self, inst: &DecodedInst) -> bool {
        floating_point_convert_signed_integer(
            self,
            inst.sf(),
            inst.bits(23, 22),
            Vec::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
            ROUND_TOWARDS_MINUS_INFINITY,
        )
    }

    pub fn fcvtmu_float(&mut self, inst: &DecodedInst) -> bool {
        floating_point_convert_unsigned_integer(
            self,
            inst.sf(),
            inst.bits(23, 22),
            Vec::from_u32(inst.rn()),
            Reg::from_u32(inst.rd()),
            ROUND_TOWARDS_MINUS_INFINITY,
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::frontend::a64::translate::{translate, TranslationOptions};
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;

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
    fn scvtf_float_int_translates_without_interpret_terminal() {
        let block = translate_single(0x1e220020);
        assert!(block.instructions.iter().any(|inst| matches!(
            inst.opcode,
            Opcode::FPFixedS32ToSingle | Opcode::FPFixedS64ToSingle
        )));
    }

    #[test]
    fn fcvtzs_float_int_translates_without_interpret_terminal() {
        let block = translate_single(0x1e380020);
        assert!(block.instructions.iter().any(|inst| matches!(
            inst.opcode,
            Opcode::FPSingleToFixedS32 | Opcode::FPDoubleToFixedS32
        )));
    }

    #[test]
    fn fmov_float_gen_translates_without_interpret_terminal() {
        let block = translate_single(0x1e270020);
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, Opcode::A64GetW | Opcode::A64SetS)));
    }
}
