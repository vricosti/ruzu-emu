use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::{Reg, Vec};

const ROUND_TOWARDS_ZERO: u8 = 3;

fn current_fpcr_rounding_mode(visitor: &TranslatorVisitor<'_>) -> u8 {
    ((visitor
        .ir
        .current_location
        .expect("current_location not set")
        .fpcr()
        >> 22)
        & 0x3) as u8
}

fn scale_is_valid(sf: bool, scale: u32) -> bool {
    sf || (scale & 0x20) != 0
}

impl<'a> TranslatorVisitor<'a> {
    pub fn scvtf_float_fix(&mut self, inst: &DecodedInst) -> bool {
        let sf = inst.sf();
        let ftype = inst.bits(23, 22);
        let scale = inst.bits(15, 10);
        let rn = Reg::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());

        let intsize = if sf { 64 } else { 32 };
        let fltsize = match self.fp_datasize(ftype) {
            Some(size) if size != 16 => size,
            _ => return self.unallocated_encoding(),
        };
        if !scale_is_valid(sf, scale) {
            return self.unallocated_encoding();
        }

        let fracbits = (64 - scale) as u8;
        let rounding_mode = current_fpcr_rounding_mode(self);
        let intval = self.x(intsize, rn);
        let fltval = match fltsize {
            32 => self
                .ir
                .ir()
                .fp_fixed_to_single(intval, intsize, true, fracbits, rounding_mode),
            64 => self
                .ir
                .ir()
                .fp_fixed_to_double(intval, intsize, true, fracbits, rounding_mode),
            _ => unreachable!(),
        };
        self.v_scalar_write(fltsize, vd, fltval);
        true
    }

    pub fn ucvtf_float_fix(&mut self, inst: &DecodedInst) -> bool {
        let sf = inst.sf();
        let ftype = inst.bits(23, 22);
        let scale = inst.bits(15, 10);
        let rn = Reg::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());

        let intsize = if sf { 64 } else { 32 };
        let fltsize = match self.fp_datasize(ftype) {
            Some(size) if size != 16 => size,
            _ => return self.unallocated_encoding(),
        };
        if !scale_is_valid(sf, scale) {
            return self.unallocated_encoding();
        }

        let fracbits = (64 - scale) as u8;
        let rounding_mode = current_fpcr_rounding_mode(self);
        let intval = self.x(intsize, rn);
        let fltval = match fltsize {
            32 => self
                .ir
                .ir()
                .fp_fixed_to_single(intval, intsize, false, fracbits, rounding_mode),
            64 => self
                .ir
                .ir()
                .fp_fixed_to_double(intval, intsize, false, fracbits, rounding_mode),
            _ => unreachable!(),
        };
        self.v_scalar_write(fltsize, vd, fltval);
        true
    }

    pub fn fcvtzs_float_fix(&mut self, inst: &DecodedInst) -> bool {
        let sf = inst.sf();
        let ftype = inst.bits(23, 22);
        let scale = inst.bits(15, 10);
        let vn = Vec::from_u32(inst.rn());
        let rd = Reg::from_u32(inst.rd());

        let intsize = if sf { 64 } else { 32 };
        let fltsize = match self.fp_datasize(ftype) {
            Some(size) => size,
            None => return self.unallocated_encoding(),
        };
        if !scale_is_valid(sf, scale) {
            return self.unallocated_encoding();
        }

        let fracbits = (64 - scale) as u8;
        let fltval = self.v_scalar_read(fltsize, vn);
        let intval = match intsize {
            32 => self
                .ir
                .ir()
                .fp_to_fixed_s32(fltval, fltsize, fracbits, ROUND_TOWARDS_ZERO),
            64 => self
                .ir
                .ir()
                .fp_to_fixed_s64(fltval, fltsize, fracbits, ROUND_TOWARDS_ZERO),
            _ => unreachable!(),
        };
        self.set_x(intsize, rd, intval);
        true
    }

    pub fn fcvtzu_float_fix(&mut self, inst: &DecodedInst) -> bool {
        let sf = inst.sf();
        let ftype = inst.bits(23, 22);
        let scale = inst.bits(15, 10);
        let vn = Vec::from_u32(inst.rn());
        let rd = Reg::from_u32(inst.rd());

        let intsize = if sf { 64 } else { 32 };
        let fltsize = match self.fp_datasize(ftype) {
            Some(size) => size,
            None => return self.unallocated_encoding(),
        };
        if !scale_is_valid(sf, scale) {
            return self.unallocated_encoding();
        }

        let fracbits = (64 - scale) as u8;
        let fltval = self.v_scalar_read(fltsize, vn);
        let intval = match intsize {
            32 => self
                .ir
                .ir()
                .fp_to_fixed_u32(fltval, fltsize, fracbits, ROUND_TOWARDS_ZERO),
            64 => self
                .ir
                .ir()
                .fp_to_fixed_u64(fltval, fltsize, fracbits, ROUND_TOWARDS_ZERO),
            _ => unreachable!(),
        };
        self.set_x(intsize, rd, intval);
        true
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
    fn scvtf_float_fix_translates_without_interpret_terminal() {
        let block = translate_single(0x1e02fc20);
        assert!(block.instructions.iter().any(|inst| matches!(
            inst.opcode,
            Opcode::FPFixedS32ToSingle | Opcode::FPFixedS64ToSingle
        )));
    }

    #[test]
    fn fcvtzs_float_fix_translates_without_interpret_terminal() {
        let block = translate_single(0x1e18fc20);
        assert!(block.instructions.iter().any(|inst| matches!(
            inst.opcode,
            Opcode::FPSingleToFixedS32 | Opcode::FPDoubleToFixedS32
        )));
    }
}
