use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;
use crate::ir::value::Value;

fn fp_compare(
    visitor: &mut TranslatorVisitor<'_>,
    ftype: u32,
    vm: Vec,
    vn: Vec,
    exc_on_qnan: bool,
    cmp_with_zero: bool,
) -> bool {
    let datasize = match visitor.fp_datasize(ftype) {
        Some(size) if size != 16 => size,
        _ => return visitor.unallocated_encoding(),
    };

    let operand1 = visitor.v_scalar_read(datasize, vn);
    let operand2 = if cmp_with_zero {
        visitor.i(datasize, 0)
    } else {
        visitor.v_scalar_read(datasize, vm)
    };

    let nzcv = visitor
        .ir
        .ir()
        .fp_compare(datasize, operand1, operand2, Value::ImmU1(exc_on_qnan));
    visitor.ir.set_nzcv(nzcv);
    true
}

impl<'a> TranslatorVisitor<'a> {
    pub fn fcmp_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.rm());
        let vn = Vec::from_u32(inst.rn());
        let cmp_with_zero = inst.bit(3);
        fp_compare(self, ftype, vm, vn, false, cmp_with_zero)
    }

    pub fn fcmpe_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.rm());
        let vn = Vec::from_u32(inst.rn());
        let cmp_with_zero = inst.bit(3);
        fp_compare(self, ftype, vm, vn, true, cmp_with_zero)
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
    fn fcmp_float_translates_without_interpret_terminal() {
        let block = translate_single(0x1E222020);
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, Opcode::FPCompare32 | Opcode::FPCompare64)));
    }

    #[test]
    fn fcmpe_float_translates_without_interpret_terminal() {
        let block = translate_single(0x1E222030);
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, Opcode::FPCompare32 | Opcode::FPCompare64)));
    }
}
