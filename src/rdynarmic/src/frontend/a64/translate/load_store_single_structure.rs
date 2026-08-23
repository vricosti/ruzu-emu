//! Port of upstream
//! `dynarmic/frontend/A64/translate/impl/load_store_single_structure.cpp`.

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::{AccType, Reg, Vec};
use crate::ir::emitter::MemOp;

impl<'a> TranslatorVisitor<'a> {
    fn single_structure_shared_decode_and_operation(
        &mut self,
        wback: bool,
        memop: MemOp,
        q: bool,
        s_bit: bool,
        r_bit: bool,
        replicate: bool,
        rm: Option<Reg>,
        opcode: u32,
        size: u32,
        rn: Reg,
        vt: Vec,
    ) -> bool {
        let selem = (((opcode & 1) << 1) | u32::from(r_bit)) + 1;
        let mut scale = (opcode >> 1) & 0b11;
        let index: u32;

        match scale {
            0 => {
                index = (u32::from(q) << 3) | (u32::from(s_bit) << 2) | size;
            }
            1 => {
                if (size & 1) != 0 {
                    return self.unallocated_encoding();
                }
                index = (u32::from(q) << 2) | (u32::from(s_bit) << 1) | ((size >> 1) & 1);
            }
            2 => {
                if (size & 0b10) != 0 {
                    return self.unallocated_encoding();
                }
                if (size & 1) != 0 {
                    if s_bit {
                        return self.unallocated_encoding();
                    }
                    index = u32::from(q);
                    scale = 3;
                } else {
                    index = (u32::from(q) << 1) | u32::from(s_bit);
                }
            }
            3 => {
                if memop == MemOp::Store || s_bit {
                    return self.unallocated_encoding();
                }
                scale = size;
                index = 0;
            }
            _ => unreachable!(),
        }

        let datasize = if q { 128usize } else { 64usize };
        let esize = 8usize << scale as usize;
        let ebytes = esize / 8;
        let address = self.base_address(rn);
        let mut offs = self.ir.ir().imm64(0);

        if replicate {
            for elem in 0..selem {
                let tt = Vec::from_u32((vt.number() as u32 + elem) % 32);
                let addr = self.addr_add(address, offs);
                let element = self.mem_read(addr, ebytes, AccType::Vec);
                let broadcasted = self.ir.ir().vector_broadcast(esize, element);
                self.v_write(datasize, tt, broadcasted);
                let delta = self.ir.ir().imm64(ebytes as u64);
                offs = self.addr_add(offs, delta);
            }
        } else {
            for elem in 0..selem {
                let tt = Vec::from_u32((vt.number() as u32 + elem) % 32);
                let current = self.v_read(128, tt);
                let addr = self.addr_add(address, offs);

                match memop {
                    MemOp::Load => {
                        let element = self.mem_read(addr, ebytes, AccType::Vec);
                        let vec =
                            self.ir
                                .ir()
                                .vector_set_element(esize, current, index as u8, element);
                        self.v_write(128, tt, vec);
                    }
                    MemOp::Store => {
                        let element = self.ir.ir().vector_get_element(esize, current, index as u8);
                        self.mem_write(addr, element, ebytes, AccType::Vec);
                    }
                    MemOp::Prefetch => unreachable!(),
                }

                let delta = self.ir.ir().imm64(ebytes as u64);
                offs = self.addr_add(offs, delta);
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

    fn sngl_no_wback_fields(&mut self, inst: &DecodedInst) -> (bool, u32, bool, u32, Reg, Vec) {
        (
            inst.q(),
            inst.bits(15, 14),
            inst.bit(12),
            inst.bits(11, 10),
            Reg::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
        )
    }

    fn sngl_wback_fields(&mut self, inst: &DecodedInst) -> (bool, Reg, u32, bool, u32, Reg, Vec) {
        (
            inst.q(),
            Reg::from_u32(inst.bits(20, 16)),
            inst.bits(15, 14),
            inst.bit(12),
            inst.bits(11, 10),
            Reg::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
        )
    }

    pub fn ld1_sngl_1(&mut self, inst: &DecodedInst) -> bool {
        let (q, upper_opcode, s_bit, size, rn, vt) = self.sngl_no_wback_fields(inst);
        self.single_structure_shared_decode_and_operation(
            false,
            MemOp::Load,
            q,
            s_bit,
            false,
            false,
            None,
            upper_opcode << 1,
            size,
            rn,
            vt,
        )
    }

    pub fn ld1_sngl_2(&mut self, inst: &DecodedInst) -> bool {
        let (q, rm, upper_opcode, s_bit, size, rn, vt) = self.sngl_wback_fields(inst);
        self.single_structure_shared_decode_and_operation(
            true,
            MemOp::Load,
            q,
            s_bit,
            false,
            false,
            Some(rm),
            upper_opcode << 1,
            size,
            rn,
            vt,
        )
    }

    pub fn ld1r_1(&mut self, inst: &DecodedInst) -> bool {
        self.single_structure_shared_decode_and_operation(
            false,
            MemOp::Load,
            inst.q(),
            false,
            false,
            true,
            None,
            0b110,
            inst.bits(11, 10),
            Reg::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
        )
    }

    pub fn ld1r_2(&mut self, inst: &DecodedInst) -> bool {
        self.single_structure_shared_decode_and_operation(
            true,
            MemOp::Load,
            inst.q(),
            false,
            false,
            true,
            Some(Reg::from_u32(inst.bits(20, 16))),
            0b110,
            inst.bits(11, 10),
            Reg::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
        )
    }

    pub fn ld2_sngl_1(&mut self, inst: &DecodedInst) -> bool {
        let (q, upper_opcode, s_bit, size, rn, vt) = self.sngl_no_wback_fields(inst);
        self.single_structure_shared_decode_and_operation(
            false,
            MemOp::Load,
            q,
            s_bit,
            true,
            false,
            None,
            upper_opcode << 1,
            size,
            rn,
            vt,
        )
    }

    pub fn ld2_sngl_2(&mut self, inst: &DecodedInst) -> bool {
        let (q, rm, upper_opcode, s_bit, size, rn, vt) = self.sngl_wback_fields(inst);
        self.single_structure_shared_decode_and_operation(
            true,
            MemOp::Load,
            q,
            s_bit,
            true,
            false,
            Some(rm),
            upper_opcode << 1,
            size,
            rn,
            vt,
        )
    }

    pub fn ld2r_1(&mut self, inst: &DecodedInst) -> bool {
        self.single_structure_shared_decode_and_operation(
            false,
            MemOp::Load,
            inst.q(),
            false,
            true,
            true,
            None,
            0b110,
            inst.bits(11, 10),
            Reg::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
        )
    }

    pub fn ld2r_2(&mut self, inst: &DecodedInst) -> bool {
        self.single_structure_shared_decode_and_operation(
            true,
            MemOp::Load,
            inst.q(),
            false,
            true,
            true,
            Some(Reg::from_u32(inst.bits(20, 16))),
            0b110,
            inst.bits(11, 10),
            Reg::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
        )
    }

    pub fn ld3_sngl_1(&mut self, inst: &DecodedInst) -> bool {
        let (q, upper_opcode, s_bit, size, rn, vt) = self.sngl_no_wback_fields(inst);
        self.single_structure_shared_decode_and_operation(
            false,
            MemOp::Load,
            q,
            s_bit,
            false,
            false,
            None,
            (upper_opcode << 1) | 1,
            size,
            rn,
            vt,
        )
    }

    pub fn ld3_sngl_2(&mut self, inst: &DecodedInst) -> bool {
        let (q, rm, upper_opcode, s_bit, size, rn, vt) = self.sngl_wback_fields(inst);
        self.single_structure_shared_decode_and_operation(
            true,
            MemOp::Load,
            q,
            s_bit,
            false,
            false,
            Some(rm),
            (upper_opcode << 1) | 1,
            size,
            rn,
            vt,
        )
    }

    pub fn ld3r_1(&mut self, inst: &DecodedInst) -> bool {
        self.single_structure_shared_decode_and_operation(
            false,
            MemOp::Load,
            inst.q(),
            false,
            false,
            true,
            None,
            0b111,
            inst.bits(11, 10),
            Reg::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
        )
    }

    pub fn ld3r_2(&mut self, inst: &DecodedInst) -> bool {
        self.single_structure_shared_decode_and_operation(
            true,
            MemOp::Load,
            inst.q(),
            false,
            false,
            true,
            Some(Reg::from_u32(inst.bits(20, 16))),
            0b111,
            inst.bits(11, 10),
            Reg::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
        )
    }

    pub fn ld4_sngl_1(&mut self, inst: &DecodedInst) -> bool {
        let (q, upper_opcode, s_bit, size, rn, vt) = self.sngl_no_wback_fields(inst);
        self.single_structure_shared_decode_and_operation(
            false,
            MemOp::Load,
            q,
            s_bit,
            true,
            false,
            None,
            (upper_opcode << 1) | 1,
            size,
            rn,
            vt,
        )
    }

    pub fn ld4_sngl_2(&mut self, inst: &DecodedInst) -> bool {
        let (q, rm, upper_opcode, s_bit, size, rn, vt) = self.sngl_wback_fields(inst);
        self.single_structure_shared_decode_and_operation(
            true,
            MemOp::Load,
            q,
            s_bit,
            true,
            false,
            Some(rm),
            (upper_opcode << 1) | 1,
            size,
            rn,
            vt,
        )
    }

    pub fn ld4r_1(&mut self, inst: &DecodedInst) -> bool {
        self.single_structure_shared_decode_and_operation(
            false,
            MemOp::Load,
            inst.q(),
            false,
            true,
            true,
            None,
            0b111,
            inst.bits(11, 10),
            Reg::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
        )
    }

    pub fn ld4r_2(&mut self, inst: &DecodedInst) -> bool {
        self.single_structure_shared_decode_and_operation(
            true,
            MemOp::Load,
            inst.q(),
            false,
            true,
            true,
            Some(Reg::from_u32(inst.bits(20, 16))),
            0b111,
            inst.bits(11, 10),
            Reg::from_u32(inst.rn()),
            Vec::from_u32(inst.rd()),
        )
    }

    pub fn st1_sngl_1(&mut self, inst: &DecodedInst) -> bool {
        let (q, upper_opcode, s_bit, size, rn, vt) = self.sngl_no_wback_fields(inst);
        self.single_structure_shared_decode_and_operation(
            false,
            MemOp::Store,
            q,
            s_bit,
            false,
            false,
            None,
            upper_opcode << 1,
            size,
            rn,
            vt,
        )
    }

    pub fn st1_sngl_2(&mut self, inst: &DecodedInst) -> bool {
        let (q, rm, upper_opcode, s_bit, size, rn, vt) = self.sngl_wback_fields(inst);
        self.single_structure_shared_decode_and_operation(
            true,
            MemOp::Store,
            q,
            s_bit,
            false,
            false,
            Some(rm),
            upper_opcode << 1,
            size,
            rn,
            vt,
        )
    }

    pub fn st2_sngl_1(&mut self, inst: &DecodedInst) -> bool {
        let (q, upper_opcode, s_bit, size, rn, vt) = self.sngl_no_wback_fields(inst);
        self.single_structure_shared_decode_and_operation(
            false,
            MemOp::Store,
            q,
            s_bit,
            true,
            false,
            None,
            upper_opcode << 1,
            size,
            rn,
            vt,
        )
    }

    pub fn st2_sngl_2(&mut self, inst: &DecodedInst) -> bool {
        let (q, rm, upper_opcode, s_bit, size, rn, vt) = self.sngl_wback_fields(inst);
        self.single_structure_shared_decode_and_operation(
            true,
            MemOp::Store,
            q,
            s_bit,
            true,
            false,
            Some(rm),
            upper_opcode << 1,
            size,
            rn,
            vt,
        )
    }

    pub fn st3_sngl_1(&mut self, inst: &DecodedInst) -> bool {
        let (q, upper_opcode, s_bit, size, rn, vt) = self.sngl_no_wback_fields(inst);
        self.single_structure_shared_decode_and_operation(
            false,
            MemOp::Store,
            q,
            s_bit,
            false,
            false,
            None,
            (upper_opcode << 1) | 1,
            size,
            rn,
            vt,
        )
    }

    pub fn st3_sngl_2(&mut self, inst: &DecodedInst) -> bool {
        let (q, rm, upper_opcode, s_bit, size, rn, vt) = self.sngl_wback_fields(inst);
        self.single_structure_shared_decode_and_operation(
            true,
            MemOp::Store,
            q,
            s_bit,
            false,
            false,
            Some(rm),
            (upper_opcode << 1) | 1,
            size,
            rn,
            vt,
        )
    }

    pub fn st4_sngl_1(&mut self, inst: &DecodedInst) -> bool {
        let (q, upper_opcode, s_bit, size, rn, vt) = self.sngl_no_wback_fields(inst);
        self.single_structure_shared_decode_and_operation(
            false,
            MemOp::Store,
            q,
            s_bit,
            true,
            false,
            None,
            (upper_opcode << 1) | 1,
            size,
            rn,
            vt,
        )
    }

    pub fn st4_sngl_2(&mut self, inst: &DecodedInst) -> bool {
        let (q, rm, upper_opcode, s_bit, size, rn, vt) = self.sngl_wback_fields(inst);
        self.single_structure_shared_decode_and_operation(
            true,
            MemOp::Store,
            q,
            s_bit,
            true,
            false,
            Some(rm),
            (upper_opcode << 1) | 1,
            size,
            rn,
            vt,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a64::decoder::{decode, A64InstructionName};
    use crate::ir::block::Block;
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::opcode::Opcode;

    fn translate_one(raw: u32) -> (Block, bool, A64InstructionName) {
        let decoded = decode(raw).expect("instruction should decode");
        let mut block = Block::new(A64LocationDescriptor::new(0x1000, 0, false).to_location());
        let mut visitor = TranslatorVisitor::new(
            &mut block,
            A64LocationDescriptor::new(0x1000, 0, false),
            crate::frontend::a64::translate::TranslationOptions::default(),
        );
        let should_continue = visitor.dispatch(&decoded);
        drop(visitor);
        (block, should_continue, decoded.name)
    }

    #[test]
    fn st1_single_structure_encoding_translates_without_interpret_terminal() {
        let (block, should_continue, name) = translate_one(0x4D00_8103);
        assert_eq!(name, A64InstructionName::ST1_sngl_1);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A64WriteMemory32));
    }

    #[test]
    fn ld1r_1_replicate_translates_without_interpret_terminal() {
        // LD1R { Vt.4S }, [Xn] — Q=1, size=0b10 (esize=32), Rn=1, Vt=0.
        let (block, should_continue, name) = translate_one(0x4D40_C820);
        assert_eq!(name, A64InstructionName::LD1R_1);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A64ReadMemory32));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorBroadcast32));
    }

    #[test]
    fn ld2_single_structure_translates_without_interpret_terminal() {
        // LD2 (single structure), 32-bit element (scale=2) — selem=2.
        let (block, should_continue, name) = translate_one(0x4D60_8020);
        assert_eq!(name, A64InstructionName::LD2_sngl_1);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A64ReadMemory32));
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
    fn st2_single_structure_translates_without_interpret_terminal() {
        // ST2 (single structure), 32-bit element (scale=2) — selem=2.
        let (block, should_continue, name) = translate_one(0x4D20_8020);
        assert_eq!(name, A64InstructionName::ST2_sngl_1);
        assert!(should_continue);
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A64WriteMemory32));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::VectorGetElement32));
    }
}
