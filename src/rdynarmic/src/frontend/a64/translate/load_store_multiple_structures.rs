//! Port of upstream
//! `dynarmic/frontend/A64/translate/impl/load_store_multiple_structures.cpp`.

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::{AccType, Reg, Vec};
use crate::ir::emitter::MemOp;

impl<'a> TranslatorVisitor<'a> {
    fn shared_decode_and_operation(
        &mut self,
        wback: bool,
        memop: MemOp,
        q: bool,
        rm: Option<Reg>,
        opcode: u32,
        size: u32,
        rn: Reg,
        vt: Vec,
    ) -> bool {
        let datasize = if q { 128usize } else { 64usize };
        let esize = 8usize << size as usize;
        let elements = datasize / esize;
        let ebytes = esize / 8;

        let (rpt, selem) = match opcode {
            0b0000 => (1usize, 4usize),
            0b0010 => (4usize, 1usize),
            0b0100 => (1usize, 3usize),
            0b0110 => (3usize, 1usize),
            0b0111 => (1usize, 1usize),
            0b1000 => (1usize, 2usize),
            0b1010 => (2usize, 1usize),
            _ => return self.unallocated_encoding(),
        };
        debug_assert!(rpt == 1 || selem == 1);

        if size == 0b11 && !q && selem != 1 {
            return self.reserved_value();
        }

        let address = self.base_address(rn);
        let mut offs = self.ir.ir().imm64(0);

        if selem == 1 {
            for r in 0..rpt {
                let tt = Vec::from_u32((vt.number() as u32 + r as u32) % 32);
                let addr = self.addr_add(address, offs);
                match memop {
                    MemOp::Load => {
                        let vec = self.mem_read(addr, ebytes * elements, AccType::Vec);
                        self.v_scalar_write(datasize, tt, vec);
                    }
                    MemOp::Store => {
                        let vec = self.v_scalar_read(datasize, tt);
                        self.mem_write(addr, vec, ebytes * elements, AccType::Vec);
                    }
                    MemOp::Prefetch => unreachable!(),
                }
                let delta = self.ir.ir().imm64((ebytes * elements) as u64);
                offs = self.addr_add(offs, delta);
            }
        } else {
            for e in 0..elements {
                for s in 0..selem {
                    let tt = Vec::from_u32((vt.number() as u32 + s as u32) % 32);
                    let addr = self.addr_add(address, offs);
                    match memop {
                        MemOp::Load => {
                            let elem = self.mem_read(addr, ebytes, AccType::Vec);
                            let cur = self.v_read(datasize, tt);
                            let vec = self.ir.ir().vector_set_element(esize, cur, e as u8, elem);
                            self.v_write(datasize, tt, vec);
                        }
                        MemOp::Store => {
                            let cur = self.v_read(datasize, tt);
                            let elem = self.ir.ir().vector_get_element(esize, cur, e as u8);
                            self.mem_write(addr, elem, ebytes, AccType::Vec);
                        }
                        MemOp::Prefetch => unreachable!(),
                    }
                    let delta = self.ir.ir().imm64(ebytes as u64);
                    offs = self.addr_add(offs, delta);
                }
            }
        }

        if wback {
            if let Some(rm) = rm {
                if rm != Reg::SP {
                    offs = self.x(64, rm);
                }
            }
            let writeback = self.addr_add(address, offs);
            self.writeback_address(rn, writeback);
        }

        true
    }

    pub fn stx_mult_1(&mut self, inst: &DecodedInst) -> bool {
        self.shared_decode_and_operation(
            false,
            MemOp::Store,
            inst.q(),
            None,
            inst.bits(15, 12),
            inst.bits(11, 10),
            Reg::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
        )
    }

    pub fn stx_mult_2(&mut self, inst: &DecodedInst) -> bool {
        self.shared_decode_and_operation(
            true,
            MemOp::Store,
            inst.q(),
            Some(Reg::from_u32(inst.rm())),
            inst.bits(15, 12),
            inst.bits(11, 10),
            Reg::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
        )
    }

    pub fn ldx_mult_1(&mut self, inst: &DecodedInst) -> bool {
        self.shared_decode_and_operation(
            false,
            MemOp::Load,
            inst.q(),
            None,
            inst.bits(15, 12),
            inst.bits(11, 10),
            Reg::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
        )
    }

    pub fn ldx_mult_2(&mut self, inst: &DecodedInst) -> bool {
        self.shared_decode_and_operation(
            true,
            MemOp::Load,
            inst.q(),
            Some(Reg::from_u32(inst.rm())),
            inst.bits(15, 12),
            inst.bits(11, 10),
            Reg::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a64::decoder::decode;
    use crate::frontend::a64::translate::TranslationOptions;
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
            TranslationOptions::default(),
        );
        let should_continue = visitor.dispatch(&decoded);
        drop(visitor);
        (block, should_continue)
    }

    #[test]
    fn ldx_mult_1_stk_encoding_translates_without_interpret_terminal() {
        let (block, should_continue) = translate_one(0x4CDFA041);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A64ReadMemory128));
    }

    #[test]
    fn ld4_interleaved_32_bit_elements_remain_vector_backed() {
        let (block, should_continue) = translate_one(0x4C40_0820);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorSetElement32));
        assert!(!block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::ZeroExtendLongToQuad));
    }

    #[test]
    fn ldx_mult_reserved_value_matches_upstream() {
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let mut visitor = TranslatorVisitor::new(
            &mut block,
            A64LocationDescriptor::new(0x1000, 0, false),
            TranslationOptions::default(),
        );
        let should_continue = visitor.shared_decode_and_operation(
            false,
            MemOp::Load,
            false,
            None,
            0b0000,
            0b11,
            Reg::R0,
            Vec::V0,
        );
        drop(visitor);

        assert!(!should_continue);
        assert!(matches!(
            &block.terminal,
            Terminal::CheckHalt { else_ } if matches!(else_.as_ref(), Terminal::ReturnToDispatch)
        ));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A64ExceptionRaised));
    }
}
