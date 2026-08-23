use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;

fn fp_two_register(
    visitor: &mut TranslatorVisitor<'_>,
    ftype: u32,
    vm: Vec,
    vn: Vec,
    vd: Vec,
    op: impl FnOnce(
        &mut TranslatorVisitor<'_>,
        usize,
        crate::ir::value::Value,
        crate::ir::value::Value,
    ) -> crate::ir::value::Value,
) -> bool {
    let datasize = match visitor.fp_datasize(ftype) {
        Some(size) if size != 16 => size,
        _ => return visitor.unallocated_encoding(),
    };

    let operand1 = visitor.v_scalar_read(datasize, vn);
    let operand2 = visitor.v_scalar_read(datasize, vm);
    let result = op(visitor, datasize, operand1, operand2);
    visitor.v_scalar_write(datasize, vd, result);
    true
}

impl<'a> TranslatorVisitor<'a> {
    pub fn fmul_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.rm());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        fp_two_register(
            self,
            ftype,
            vm,
            vn,
            vd,
            |visitor, datasize, operand1, operand2| {
                visitor.ir.ir().fp_mul(datasize, operand1, operand2)
            },
        )
    }

    pub fn fdiv_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.rm());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        fp_two_register(
            self,
            ftype,
            vm,
            vn,
            vd,
            |visitor, datasize, operand1, operand2| {
                visitor.ir.ir().fp_div(datasize, operand1, operand2)
            },
        )
    }

    pub fn fadd_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.rm());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        fp_two_register(
            self,
            ftype,
            vm,
            vn,
            vd,
            |visitor, datasize, operand1, operand2| {
                visitor.ir.ir().fp_add(datasize, operand1, operand2)
            },
        )
    }

    pub fn fsub_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.rm());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        fp_two_register(
            self,
            ftype,
            vm,
            vn,
            vd,
            |visitor, datasize, operand1, operand2| {
                visitor.ir.ir().fp_sub(datasize, operand1, operand2)
            },
        )
    }

    pub fn fmax_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.rm());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        fp_two_register(
            self,
            ftype,
            vm,
            vn,
            vd,
            |visitor, datasize, operand1, operand2| {
                visitor.ir.ir().fp_max(datasize, operand1, operand2)
            },
        )
    }

    pub fn fmin_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.rm());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        fp_two_register(
            self,
            ftype,
            vm,
            vn,
            vd,
            |visitor, datasize, operand1, operand2| {
                visitor.ir.ir().fp_min(datasize, operand1, operand2)
            },
        )
    }

    pub fn fmaxnm_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.rm());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        fp_two_register(
            self,
            ftype,
            vm,
            vn,
            vd,
            |visitor, datasize, operand1, operand2| {
                visitor.ir.ir().fp_max_numeric(datasize, operand1, operand2)
            },
        )
    }

    pub fn fminnm_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.rm());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        fp_two_register(
            self,
            ftype,
            vm,
            vn,
            vd,
            |visitor, datasize, operand1, operand2| {
                visitor.ir.ir().fp_min_numeric(datasize, operand1, operand2)
            },
        )
    }

    pub fn fnmul_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.rm());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        fp_two_register(
            self,
            ftype,
            vm,
            vn,
            vd,
            |visitor, datasize, operand1, operand2| {
                let mul = visitor.ir.ir().fp_mul(datasize, operand1, operand2);
                visitor.ir.ir().fp_neg(datasize, mul)
            },
        )
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
    fn fadd_float_translates_without_interpret_terminal() {
        let block = translate_single(0x1E222820);
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, Opcode::FPAdd32 | Opcode::FPAdd64)));
    }

    #[test]
    fn fnmul_float_translates_without_interpret_terminal() {
        let block = translate_single(0x1E228820);
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, Opcode::FPMul32 | Opcode::FPMul64)));
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, Opcode::FPNeg32 | Opcode::FPNeg64)));
    }

    #[test]
    fn fadd_float_half_precision_rejects_as_unallocated() {
        let block = translate_single(0x1EE22820);
        assert!(!matches!(block.terminal, Terminal::LinkBlock { .. }));
        assert!(block
            .instructions
            .iter()
            .all(|inst| !matches!(inst.opcode, Opcode::FPAdd32 | Opcode::FPAdd64)));
    }
}
