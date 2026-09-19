//! Port of upstream
//! `dynarmic/frontend/A64/translate/impl/simd_copy.cpp` (subset focused
//! on `DUP (general)` and `DUP (element)`, used by libc's
//! `__memset_aarch64` to broadcast the fill byte across `V0`. Without
//! these, V0 retains stale data and `memset(buf, 0, size)` fills `buf`
//! with garbage instead of zeros — manifesting in STK as a
//! refcount-table full of `0x0000FF00FFFF0000` sentinel pointers,
//! followed by an atomic-CAS spin on `0xFF00FFFF0008` and SIGSEGV.

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::{Reg, Vec};
use crate::ir::value::Value;

fn lowest_set_bit(x: u32) -> u32 {
    x.trailing_zeros()
}

impl<'a> TranslatorVisitor<'a> {
    /// Truncate a u64 IR value down to `esize` bits (8/16/32/64) so it can
    /// feed `vector_broadcast(esize, ...)` which type-checks the input
    /// width.
    fn truncate_to_esize(&mut self, value: Value, esize: usize) -> Value {
        let ir = self.ir.ir();
        match esize {
            8 => ir.least_significant_byte(value),
            16 => ir.least_significant_half(value),
            32 => ir.least_significant_word(value),
            64 => value,
            _ => panic!("Invalid esize {}", esize),
        }
    }

    /// DUP (general): broadcast scalar GPR to all lanes of a vector.
    /// Encoding: `0Q001110000iiiii000011nnnnnddddd`
    pub fn dup_gen(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let imm5 = inst.bits(20, 16);
        let size = lowest_set_bit(imm5);
        if size > 3 {
            return self.reserved_value();
        }
        if size == 3 && !q {
            return self.reserved_value();
        }
        let rn = Reg::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());

        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };

        // X(esize, Rn) — read full register (32/64) and truncate.
        let regsize = if esize == 64 { 64 } else { 32 };
        let element_full = self.x(regsize, rn);
        let element = self.truncate_to_esize(element_full, esize);

        let result = if q {
            self.ir.ir().vector_broadcast(esize, element)
        } else {
            self.ir.ir().vector_broadcast_lower(esize, element)
        };

        self.v_write(datasize, rd, result);
        true
    }

    /// DUP (element) — Q form: broadcast one element of a source vector
    /// to all lanes of the destination vector.
    /// Encoding: `0Q001110000iiiii000001nnnnnddddd`
    pub fn dup_elt_2(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let imm5 = inst.bits(20, 16);
        let size = lowest_set_bit(imm5);
        if size > 3 {
            return self.reserved_value();
        }
        if size == 3 && !q {
            return self.reserved_value();
        }
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());

        let index = (imm5 >> (size + 1)) as u8;
        let idxdsize = if (imm5 >> 4) & 1 != 0 { 128 } else { 64 };
        let esize = 8usize << size as usize;
        let datasize = if q { 128 } else { 64 };

        let operand = self.v_read(idxdsize, rn);
        let element = self.ir.ir().vector_get_element(esize, operand, index);
        let result = if q {
            self.ir.ir().vector_broadcast(esize, element)
        } else {
            self.ir.ir().vector_broadcast_lower(esize, element)
        };
        self.v_write(datasize, rd, result);
        true
    }

    /// INS (general): copy a GPR value into one lane of a vector.
    /// Encoding: `01001110000iiiii000111nnnnnddddd`
    pub fn ins_gen(&mut self, inst: &DecodedInst) -> bool {
        let imm5 = inst.bits(20, 16);
        let size = lowest_set_bit(imm5);
        if size > 3 {
            return self.reserved_value();
        }
        let rn = Reg::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let index = (imm5 >> (size + 1)) as u8;
        let esize = 8usize << size as usize;
        let regsize = if esize == 64 { 64 } else { 32 };
        let element_full = self.x(regsize, rn);
        let element = self.truncate_to_esize(element_full, esize);
        let cur = self.v_read(128, rd);
        let result = self.ir.ir().vector_set_element(esize, cur, index, element);
        self.v_write(128, rd, result);
        true
    }

    /// INS (element): copy one element of one vector into one lane of
    /// another vector.
    /// Encoding: `01101110000iiiii0jjjj1nnnnnddddd`
    pub fn ins_elt(&mut self, inst: &DecodedInst) -> bool {
        let imm5 = inst.bits(20, 16);
        let imm4 = inst.bits(14, 11);
        let size = lowest_set_bit(imm5);
        if size > 3 {
            return self.reserved_value();
        }
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());
        let dst_index = (imm5 >> (size + 1)) as u8;
        let src_index = (imm4 >> size) as u8;
        let idxdsize = if (imm4 >> 3) & 1 != 0 { 128 } else { 64 };
        let esize = 8usize << size as usize;

        let operand = self.v_read(idxdsize, rn);
        let elem = self.ir.ir().vector_get_element(esize, operand, src_index);
        let cur = self.v_read(128, rd);
        let result = self.ir.ir().vector_set_element(esize, cur, dst_index, elem);
        self.v_write(128, rd, result);
        true
    }

    /// UMOV: copy one element of a vector into a GPR (zero-extended).
    /// Encoding: `0Q001110000iiiii001111nnnnnddddd`
    pub fn umov(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let imm5 = inst.bits(20, 16);
        let size = lowest_set_bit(imm5);
        if size < 3 && q {
            return self.unallocated_encoding();
        }
        if size == 3 && !q {
            return self.unallocated_encoding();
        }
        if size > 3 {
            return self.reserved_value();
        }
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Reg::from_u32(inst.rd());

        let idxdsize = if (imm5 >> 4) & 1 != 0 { 128 } else { 64 };
        let index = (imm5 >> (size + 1)) as u8;
        let esize = 8usize << size as usize;
        let datasize = if q { 64 } else { 32 };

        let operand = self.v_read(idxdsize, rn);
        let elem = self.ir.ir().vector_get_element(esize, operand, index);
        // Zero-extend element to datasize (32 or 64).
        let zext = self.sign_or_zero_extend(elem, esize, datasize, false);
        self.set_x(datasize, rd, zext);
        true
    }

    /// SMOV: copy one element of a vector into a GPR (sign-extended).
    /// Encoding: `0Q001110000iiiii001011nnnnnddddd`
    pub fn smov(&mut self, inst: &DecodedInst) -> bool {
        let q = inst.bit(30);
        let imm5 = inst.bits(20, 16);
        let size = lowest_set_bit(imm5);
        if size == 2 && !q {
            return self.unallocated_encoding();
        }
        if size > 2 {
            return self.reserved_value();
        }
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Reg::from_u32(inst.rd());

        let idxdsize = if (imm5 >> 4) & 1 != 0 { 128 } else { 64 };
        let index = (imm5 >> (size + 1)) as u8;
        let esize = 8usize << size as usize;
        let datasize = if q { 64 } else { 32 };

        let operand = self.v_read(idxdsize, rn);
        let elem = self.ir.ir().vector_get_element(esize, operand, index);
        let sext = self.sign_or_zero_extend(elem, esize, datasize, true);
        self.set_x(datasize, rd, sext);
        true
    }

    /// DUP (element) — scalar form: copy one element of a source vector to
    /// the lower lane of the destination, zeroing the upper lanes.
    /// Encoding: `01011110000iiiii000001nnnnnddddd`
    pub fn dup_elt_1(&mut self, inst: &DecodedInst) -> bool {
        let imm5 = inst.bits(20, 16);
        let size = lowest_set_bit(imm5);
        if size > 3 {
            return self.reserved_value();
        }
        let rn = Vec::from_u32(inst.bits(9, 5));
        let rd = Vec::from_u32(inst.rd());

        let index = (imm5 >> (size + 1)) as u8;
        let idxdsize = if (imm5 >> 4) & 1 != 0 { 128 } else { 64 };
        let esize = 8usize << size as usize;

        let operand = self.v_read(idxdsize, rn);
        let element = self.ir.ir().vector_get_element(esize, operand, index);

        let result = self.ir.ir().zero_extend_to_quad(element);
        self.v_write(128, rd, result);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a64::decoder::decode;
    use crate::frontend::a64::translate::TranslationOptions;
    use crate::ir::{block::Block, location::A64LocationDescriptor, opcode::Opcode};

    #[test]
    fn scalar_dup_zero_extends_every_element_width_to_a_full_register() {
        for size in 0..4 {
            for index in 0..(16 >> size) {
                let imm5 = (1 << size) | (index << (size + 1));
                let decoded = decode(0x5e000420 | (imm5 << 16)).unwrap();
                let location = A64LocationDescriptor::new(0x1000, 0, false);
                let mut block = Block::new(location.to_location());
                let mut visitor = TranslatorVisitor::new(&mut block, location, TranslationOptions::default());
                assert!(visitor.dispatch(&decoded));
                drop(visitor);
                assert_eq!(block.instructions.last().unwrap().opcode, Opcode::A64SetQ);
                assert!(block.instructions.iter().any(|i| i.opcode == Opcode::ZeroExtendLongToQuad));
                assert!(!block.instructions.iter().any(|i| matches!(i.opcode, Opcode::A64SetS | Opcode::A64SetD)));
            }
        }
    }

    #[test]
    fn zero_immediate_is_reserved_in_scalar_dup() {
        let location = A64LocationDescriptor::new(0x1000, 0, false);
        let mut block = Block::new(location.to_location());
        let mut visitor = TranslatorVisitor::new(&mut block, location, TranslationOptions::default());
        assert!(!visitor.dispatch(&decode(0x5e000420).unwrap()));
        drop(visitor);
        assert!(!block.instructions.iter().any(|i| i.opcode == Opcode::A64SetQ));
    }
}
