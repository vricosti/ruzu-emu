use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;
use crate::ir::cond::Cond;
use crate::ir::value::Value;

impl<'a> TranslatorVisitor<'a> {
    pub fn fcsel_float(&mut self, inst: &DecodedInst) -> bool {
        let ftype = inst.bits(23, 22);
        let vm = Vec::from_u32(inst.rm());
        let cond = Cond::from_u8(inst.cond_field() as u8);
        let vn = Vec::from_u32(inst.rn());
        let vd = Vec::from_u32(inst.rd());

        let datasize = match self.fp_datasize(ftype) {
            Some(size) if size != 16 => size,
            _ => return self.unallocated_encoding(),
        };

        let operand1 = self.v_scalar_read(datasize, vn);
        let operand2 = self.v_scalar_read(datasize, vm);
        let cond_val = Value::ImmCond(cond);
        let result = match datasize {
            32 => self
                .ir
                .ir()
                .conditional_select_32(cond_val, operand1, operand2),
            _ => self
                .ir
                .ir()
                .conditional_select_64(cond_val, operand1, operand2),
        };
        self.v_scalar_write(datasize, vd, result);
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
    fn fcsel_float_translates_without_interpret_terminal() {
        // FCSEL S0, S1, S2, EQ
        let block = translate_single(0x1E220C20);
        assert!(block.instructions.iter().any(|inst| matches!(
            inst.opcode,
            Opcode::ConditionalSelect32 | Opcode::ConditionalSelect64
        )));
    }
}
