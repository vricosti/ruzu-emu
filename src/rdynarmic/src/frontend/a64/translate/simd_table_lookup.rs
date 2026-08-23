//! Port of upstream
//! `dynarmic/frontend/A64/translate/impl/simd_table_lookup.cpp`.

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;
use crate::ir::value::Value;

impl<'a> TranslatorVisitor<'a> {
    fn table_lookup(&mut self, inst: &DecodedInst, is_tbl: bool) -> bool {
        let q = inst.bit(30);
        let vm = Vec::from_u32(inst.rm());
        let len = inst.bits(14, 13) as usize;
        let vn = inst.rn() as usize;
        let vd = Vec::from_u32(inst.rd());
        let datasize = if q { 128 } else { 64 };

        let table_values: std::vec::Vec<Value> = (0..=len)
            .map(|i| self.ir.get_q(Vec::from_u32(((vn + i) % 32) as u32)))
            .collect();
        let table = self.ir.ir().vector_table(&table_values);
        let indices = self.ir.get_q(vm);
        let defaults = if is_tbl {
            self.ir.ir().zero_vector()
        } else {
            self.ir.get_q(vd)
        };
        let result = self
            .ir
            .ir()
            .vector_table_lookup_128(defaults, table, indices);
        let result = if datasize == 128 {
            result
        } else {
            self.ir.ir().vector_zero_upper(result)
        };
        self.v_write(datasize, vd, result);
        true
    }

    pub fn tbl(&mut self, inst: &DecodedInst) -> bool {
        self.table_lookup(inst, true)
    }

    pub fn tbx(&mut self, inst: &DecodedInst) -> bool {
        self.table_lookup(inst, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a64::decoder::decode;
    use crate::ir::block::Block;
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::terminal::Terminal;

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
    fn tbl_q1_emits_table_lookup128() {
        let (block, should_continue) = translate_one(0x4E022003);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorTable));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorTableLookup128));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A64SetQ));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn tbl_q0_zeroes_upper_half_before_set_d() {
        let (block, should_continue) = translate_one(0x0E022003);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorTableLookup128));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorZeroUpper));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A64SetD));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }

    #[test]
    fn tbx_q1_uses_existing_destination_as_default() {
        let (block, should_continue) = translate_one(0x4E023003);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorTable));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorTableLookup128));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A64GetQ));
        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
    }
}
