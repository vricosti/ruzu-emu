//! Port of upstream `dynarmic/frontend/A64/translate/impl/simd_permute.cpp`.

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::Vec;

#[derive(Clone, Copy)]
enum Transposition {
    Trn1,
    Trn2,
}

#[derive(Clone, Copy)]
enum UnzipType {
    Even,
    Odd,
}

impl<'a> TranslatorVisitor<'a> {
    fn vector_transpose(&mut self, inst: &DecodedInst, kind: Transposition) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if !q && size == 0b11 {
            return self.reserved_value();
        }

        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());

        let datasize = if q { 128 } else { 64 };
        let esize = 8usize << size as usize;

        let m = self.v_read(datasize, vm);
        let n = self.v_read(datasize, vn);
        let result =
            self.ir
                .ir()
                .vector_transpose(esize, n, m, matches!(kind, Transposition::Trn2));

        self.v_write(datasize, vd, result);
        true
    }

    fn vector_unzip(&mut self, inst: &DecodedInst, kind: UnzipType) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 && !q {
            return self.reserved_value();
        }

        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());

        let datasize = if q { 128 } else { 64 };
        let esize = 8usize << size as usize;

        let n = self.v_read(datasize, vn);
        let m = self.v_read(datasize, vm);
        let result = match (kind, q) {
            (UnzipType::Even, true) => self.ir.ir().vector_deinterleave_even(esize, n, m),
            (UnzipType::Even, false) => self.ir.ir().vector_deinterleave_even_lower(esize, n, m),
            (UnzipType::Odd, true) => self.ir.ir().vector_deinterleave_odd(esize, n, m),
            (UnzipType::Odd, false) => self.ir.ir().vector_deinterleave_odd_lower(esize, n, m),
        };

        self.v_write(datasize, vd, result);
        true
    }

    pub fn trn1(&mut self, inst: &DecodedInst) -> bool {
        self.vector_transpose(inst, Transposition::Trn1)
    }

    pub fn trn2(&mut self, inst: &DecodedInst) -> bool {
        self.vector_transpose(inst, Transposition::Trn2)
    }

    pub fn uzp1(&mut self, inst: &DecodedInst) -> bool {
        self.vector_unzip(inst, UnzipType::Even)
    }

    pub fn uzp2(&mut self, inst: &DecodedInst) -> bool {
        self.vector_unzip(inst, UnzipType::Odd)
    }

    pub fn zip1(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 && !q {
            return self.reserved_value();
        }

        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());

        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };

        let operand1 = self.v_read(datasize, vn);
        let operand2 = self.v_read(datasize, vm);
        let result = self
            .ir
            .ir()
            .vector_interleave_lower(esize, operand1, operand2);

        self.v_write(datasize, vd, result);
        true
    }

    pub fn zip2(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let size = inst.bits(23, 22);
        if size == 0b11 && !q {
            return self.reserved_value();
        }

        let vm = Vec::from_u32(inst.bits(20, 16));
        let vn = Vec::from_u32(inst.bits(9, 5));
        let vd = Vec::from_u32(inst.rd());

        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };

        let operand1 = self.v_read(datasize, vn);
        let operand2 = self.v_read(datasize, vm);
        let result = if q {
            self.ir
                .ir()
                .vector_interleave_upper(esize, operand1, operand2)
        } else {
            let interleaved = self
                .ir
                .ir()
                .vector_interleave_lower(esize, operand1, operand2);
            let high = self.ir.ir().vector_get_element(64, interleaved, 1);
            let mut zipped = self.ir.ir().zero_vector();
            zipped = self.ir.ir().vector_set_element(64, zipped, 0, high);
            zipped
        };

        self.v_write(datasize, vd, result);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a64::decoder::decode;
    use crate::frontend::a64::translate::TranslationOptions;
    use crate::frontend::a64::types::Exception;
    use crate::ir::block::Block;
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::terminal::Terminal;

    fn translate_one(raw: u32) -> Block {
        let decoded = decode(raw).expect("instruction should decode");
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let mut visitor = TranslatorVisitor::new(
            &mut block,
            A64LocationDescriptor::new(0x1000, 0, false),
            TranslationOptions::default(),
        );
        let should_continue = visitor.dispatch(&decoded);
        assert!(should_continue);
        drop(visitor);
        block
    }

    #[test]
    fn uzp1_stk_encoding_translates_without_interpret_terminal() {
        let block = translate_one(0x4E961BE7);
        assert!(!block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, Opcode::A64ExceptionRaised)));
    }

    #[test]
    fn uzp1_q0_size_11_raises_reserved_value() {
        let decoded = decode(0x0ED61BE7).expect("instruction should decode");
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let mut visitor = TranslatorVisitor::new(
            &mut block,
            A64LocationDescriptor::new(0x1000, 0, false),
            TranslationOptions::default(),
        );
        let should_continue = visitor.dispatch(&decoded);
        drop(visitor);

        assert!(!should_continue);
        assert!(block.instructions.iter().any(|inst| {
            matches!(inst.opcode, Opcode::A64ExceptionRaised)
                && inst.args.iter().any(|arg| {
                    matches!(
                        arg,
                        crate::ir::value::Value::ImmU64(v)
                        if *v == Exception::ReservedValue as u64
                    )
                })
        }));
        assert!(matches!(
            &block.terminal,
            Terminal::CheckHalt { .. } | Terminal::ReturnToDispatch
        ));
    }
}
