use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;

fn fp_three_register(
    visitor: &mut TranslatorVisitor<'_>,
    ftype: u32,
    vm: Vec,
    va: Vec,
    vn: Vec,
    vd: Vec,
    op: impl FnOnce(
        &mut TranslatorVisitor<'_>,
        usize,
        crate::ir::value::Value,
        crate::ir::value::Value,
        crate::ir::value::Value,
    ) -> crate::ir::value::Value,
) -> bool {
    let datasize = match visitor.fp_datasize(ftype) {
        Some(size) => size,
        None => return visitor.unallocated_encoding(),
    };

    let addend = visitor.v_scalar_read(datasize, va);
    let operand1 = visitor.v_scalar_read(datasize, vn);
    let operand2 = visitor.v_scalar_read(datasize, vm);
    let result = op(visitor, datasize, addend, operand1, operand2);
    visitor.v_scalar_write(datasize, vd, result);
    true
}

impl<'a> TranslatorVisitor<'a> {
    pub fn fmadd_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.rm());
        let va = Vec::from_u32(inst.ra());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        fp_three_register(
            self,
            ftype,
            vm,
            va,
            vn,
            vd,
            |visitor, datasize, addend, operand1, operand2| {
                visitor
                    .ir
                    .ir()
                    .fp_mul_add(datasize, addend, operand1, operand2)
            },
        )
    }

    pub fn fmsub_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.rm());
        let va = Vec::from_u32(inst.ra());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        fp_three_register(
            self,
            ftype,
            vm,
            va,
            vn,
            vd,
            |visitor, datasize, addend, operand1, operand2| {
                visitor
                    .ir
                    .ir()
                    .fp_mul_sub(datasize, addend, operand1, operand2)
            },
        )
    }

    pub fn fnmadd_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.rm());
        let va = Vec::from_u32(inst.ra());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        fp_three_register(
            self,
            ftype,
            vm,
            va,
            vn,
            vd,
            |visitor, datasize, addend, operand1, operand2| {
                let neg_addend = visitor.ir.ir().fp_neg(datasize, addend);
                visitor
                    .ir
                    .ir()
                    .fp_mul_sub(datasize, neg_addend, operand1, operand2)
            },
        )
    }

    pub fn fnmsub_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.rm());
        let va = Vec::from_u32(inst.ra());
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());
        fp_three_register(
            self,
            ftype,
            vm,
            va,
            vn,
            vd,
            |visitor, datasize, addend, operand1, operand2| {
                let neg_addend = visitor.ir.ir().fp_neg(datasize, addend);
                visitor
                    .ir
                    .ir()
                    .fp_mul_add(datasize, neg_addend, operand1, operand2)
            },
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
    fn fmadd_float_translates_without_interpret_terminal() {
        let block = translate_single(0x1f020c20);
        assert!(block.instructions.iter().any(|inst| matches!(
            inst.opcode,
            Opcode::FPMulAdd16 | Opcode::FPMulAdd32 | Opcode::FPMulAdd64
        )));
    }

    #[test]
    fn fmsub_float_translates_without_interpret_terminal() {
        let block = translate_single(0x1f028c20);
        assert!(block.instructions.iter().any(|inst| matches!(
            inst.opcode,
            Opcode::FPMulSub16 | Opcode::FPMulSub32 | Opcode::FPMulSub64
        )));
    }

    #[test]
    fn fnmadd_float_translates_without_interpret_terminal() {
        let block = translate_single(0x1f220c20);
        assert!(block.instructions.iter().any(|inst| {
            matches!(
                inst.opcode,
                Opcode::FPNeg16
                    | Opcode::FPNeg32
                    | Opcode::FPNeg64
                    | Opcode::FPMulSub16
                    | Opcode::FPMulSub32
                    | Opcode::FPMulSub64
            )
        }));
    }

    #[test]
    fn fnmsub_float_translates_without_interpret_terminal() {
        let block = translate_single(0x1f228c20);
        assert!(block.instructions.iter().any(|inst| {
            matches!(
                inst.opcode,
                Opcode::FPNeg16
                    | Opcode::FPNeg32
                    | Opcode::FPNeg64
                    | Opcode::FPMulAdd16
                    | Opcode::FPMulAdd32
                    | Opcode::FPMulAdd64
            )
        }));
    }
}
