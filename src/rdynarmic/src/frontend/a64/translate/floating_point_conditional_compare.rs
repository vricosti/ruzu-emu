use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;
use crate::ir::cond::Cond;
use crate::ir::value::Value;

fn fp_compare(
    visitor: &mut TranslatorVisitor<'_>,
    ftype: u32,
    vm: Vec,
    cond: Cond,
    vn: Vec,
    nzcv_imm: u32,
    exc_on_qnan: bool,
) -> bool {
    let datasize = match visitor.fp_datasize(ftype) {
        Some(size) if size != 16 => size,
        _ => return visitor.unallocated_encoding(),
    };
    let flags = nzcv_imm << 28;

    let operand1 = visitor.v_scalar_read(datasize, vn);
    let operand2 = visitor.v_scalar_read(datasize, vm);

    let then_flags =
        visitor
            .ir
            .ir()
            .fp_compare(datasize, operand1, operand2, Value::ImmU1(exc_on_qnan));
    let packed_flags = visitor.ir.ir().imm32(flags);
    let else_flags = visitor.ir.ir().nzcv_from_packed_flags(packed_flags);
    let nzcv =
        visitor
            .ir
            .ir()
            .conditional_select_nzcv(Value::ImmCond(cond), then_flags, else_flags);
    visitor.ir.set_nzcv(nzcv);
    true
}

impl<'a> TranslatorVisitor<'a> {
    pub fn fccmp_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.rm());
        let cond = Cond::from_u8(inst.cond_field() as u8);
        let vn = Vec::from_u32(inst.rn());
        let nzcv_imm = inst.bits(3, 0);
        fp_compare(self, ftype, vm, cond, vn, nzcv_imm, false)
    }

    pub fn fccmpe_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.rm());
        let cond = Cond::from_u8(inst.cond_field() as u8);
        let vn = Vec::from_u32(inst.rn());
        let nzcv_imm = inst.bits(3, 0);
        fp_compare(self, ftype, vm, cond, vn, nzcv_imm, true)
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
    fn fccmp_float_translates_without_interpret_terminal() {
        let block = translate_single(0x1E22042A);
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, Opcode::FPCompare32 | Opcode::FPCompare64)));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::NZCVFromPackedFlags));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::ConditionalSelectNZCV));
    }

    #[test]
    fn fccmpe_float_translates_without_interpret_terminal() {
        let block = translate_single(0x1E22043A);
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, Opcode::FPCompare32 | Opcode::FPCompare64)));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::ConditionalSelectNZCV));
    }
}
