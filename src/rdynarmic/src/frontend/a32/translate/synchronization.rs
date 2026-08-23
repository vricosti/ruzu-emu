use crate::frontend::a32::decoder::DecodedArm;
use crate::frontend::a32::types::Exception;
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::acc_type::AccType;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

fn unpredictable_instruction(ir: &mut A32IREmitter) -> bool {
    let loc = ir.current_location.expect("current_location not set");
    let next_pc = loc.pc().wrapping_add(4);
    ir.update_upper_location_descriptor();
    ir.branch_write_pc(Value::ImmU32(next_pc));
    ir.exception_raised(Exception::UnpredictableInstruction);
    ir.set_term(Terminal::CheckHalt {
        else_: Box::new(Terminal::ReturnToDispatch),
    });
    false
}

/// ARM LDREX.
pub fn arm_lda(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();

    if rt == Reg::R15 || rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let value = ir.read_memory_32(address, AccType::Ordered);
    ir.set_register(rt, value);
    true
}

pub fn arm_ldab(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();

    if rt == Reg::R15 || rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let value = ir.read_memory_8(address, AccType::Ordered);
    let extended = ir.ir().zero_extend_byte_to_word(value);
    ir.set_register(rt, extended);
    true
}

pub fn arm_ldah(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();

    if rt == Reg::R15 || rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let value = ir.read_memory_16(address, AccType::Ordered);
    let extended = ir.ir().zero_extend_half_to_word(value);
    ir.set_register(rt, extended);
    true
}

pub fn arm_ldaex(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();

    if rt == Reg::R15 || rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let value = ir.exclusive_read_memory_32(address, AccType::Ordered);
    ir.set_register(rt, value);
    true
}

pub fn arm_ldaexb(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();

    if rt == Reg::R15 || rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let value = ir.exclusive_read_memory_8(address, AccType::Ordered);
    let extended = ir.ir().zero_extend_byte_to_word(value);
    ir.set_register(rt, extended);
    true
}

pub fn arm_ldaexh(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();

    if rt == Reg::R15 || rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let value = ir.exclusive_read_memory_16(address, AccType::Ordered);
    let extended = ir.ir().zero_extend_half_to_word(value);
    ir.set_register(rt, extended);
    true
}

pub fn arm_ldaexd(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rt2 = Reg::from_u32((rt as u32) + 1);
    let rn = inst.rn();

    if rt == Reg::R14 || rt == Reg::R15 || rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let (lo, hi) = ir.exclusive_read_memory_64(address, AccType::Ordered);
    ir.set_register(rt, lo);
    ir.set_register(rt2, hi);
    true
}

/// ARM LDREX.
pub fn arm_ldrex(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();

    if rt == Reg::R15 || rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let value = ir.exclusive_read_memory_32(address, AccType::Atomic);
    ir.set_register(rt, value);
    true
}

/// ARM LDREXB.
pub fn arm_ldrexb(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();

    if rt == Reg::R15 || rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let value = ir.exclusive_read_memory_8(address, AccType::Atomic);
    let extended = ir.ir().zero_extend_byte_to_word(value);
    ir.set_register(rt, extended);
    true
}

/// ARM LDREXH.
pub fn arm_ldrexh(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();

    if rt == Reg::R15 || rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let value = ir.exclusive_read_memory_16(address, AccType::Atomic);
    let extended = ir.ir().zero_extend_half_to_word(value);
    ir.set_register(rt, extended);
    true
}

/// ARM LDREXD.
pub fn arm_ldrexd(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rt2 = Reg::from_u32((rt as u32) + 1);
    let rn = inst.rn();

    if rt == Reg::R14 || rt == Reg::R15 || rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let (lo, hi) = ir.exclusive_read_memory_64(address, AccType::Atomic);
    ir.set_register(rt, lo);
    ir.set_register(rt2, hi);
    true
}

/// ARM STREX.
pub fn arm_stl(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rm();
    let rn = inst.rn();

    if rt == Reg::R15 || rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let value = ir.get_register(rt);
    ir.write_memory_32(address, value, AccType::Ordered);
    true
}

pub fn arm_stlb(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rm();
    let rn = inst.rn();

    if rt == Reg::R15 || rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let value = ir.get_register(rt);
    let byte = ir.ir().least_significant_byte(value);
    ir.write_memory_8(address, byte, AccType::Ordered);
    true
}

pub fn arm_stlh(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rm();
    let rn = inst.rn();

    if rt == Reg::R15 || rn == Reg::R15 {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let value = ir.get_register(rt);
    let half = ir.ir().least_significant_half(value);
    ir.write_memory_16(address, half, AccType::Ordered);
    true
}

pub fn arm_stlex(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = inst.rd();
    let rt = inst.rm();
    let rn = inst.rn();

    if rn == Reg::R15 || rd == Reg::R15 || rt == Reg::R15 {
        return unpredictable_instruction(ir);
    }
    if rd == rn || rd == rt {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let value = ir.get_register(rt);
    let result = ir.exclusive_write_memory_32(address, value, AccType::Ordered);
    ir.set_register(rd, result);
    true
}

pub fn arm_stlexb(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = inst.rd();
    let rt = inst.rm();
    let rn = inst.rn();

    if rn == Reg::R15 || rd == Reg::R15 || rt == Reg::R15 {
        return unpredictable_instruction(ir);
    }
    if rd == rn || rd == rt {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let value = ir.get_register(rt);
    let byte = ir.ir().least_significant_byte(value);
    let result = ir.exclusive_write_memory_8(address, byte, AccType::Ordered);
    ir.set_register(rd, result);
    true
}

pub fn arm_stlexh(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = inst.rd();
    let rt = inst.rm();
    let rn = inst.rn();

    if rn == Reg::R15 || rd == Reg::R15 || rt == Reg::R15 {
        return unpredictable_instruction(ir);
    }
    if rd == rn || rd == rt {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let value = ir.get_register(rt);
    let half = ir.ir().least_significant_half(value);
    let result = ir.exclusive_write_memory_16(address, half, AccType::Ordered);
    ir.set_register(rd, result);
    true
}

pub fn arm_stlexd(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = inst.rd();
    let rt = inst.rm();
    let rt2_idx = (rt as u32) + 1;
    let rn = inst.rn();

    if rn == Reg::R15 || rd == Reg::R15 || rt == Reg::R14 || (rt as u32) % 2 == 1 {
        return unpredictable_instruction(ir);
    }
    let rt2 = Reg::from_u32(rt2_idx);
    if rd == rn || rd == rt || rd == rt2 {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let lo = ir.get_register(rt);
    let hi = ir.get_register(rt2);
    let result = ir.exclusive_write_memory_64(address, lo, hi, AccType::Ordered);
    ir.set_register(rd, result);
    true
}

/// ARM STREX.
pub fn arm_strex(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = inst.rd();
    let rt = inst.rm(); // Rt is bits [3:0] for STREX
    let rn = inst.rn();

    if rn == Reg::R15 || rd == Reg::R15 || rt == Reg::R15 {
        return unpredictable_instruction(ir);
    }
    if rd == rn || rd == rt {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let value = ir.get_register(rt);
    let result = ir.exclusive_write_memory_32(address, value, AccType::Atomic);
    ir.set_register(rd, result);
    true
}

/// ARM STREXB.
pub fn arm_strexb(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = inst.rd();
    let rt = inst.rm();
    let rn = inst.rn();

    if rn == Reg::R15 || rd == Reg::R15 || rt == Reg::R15 {
        return unpredictable_instruction(ir);
    }
    if rd == rn || rd == rt {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let value = ir.get_register(rt);
    let byte = ir.ir().least_significant_byte(value);
    let result = ir.exclusive_write_memory_8(address, byte, AccType::Atomic);
    ir.set_register(rd, result);
    true
}

/// ARM STREXH.
pub fn arm_strexh(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = inst.rd();
    let rt = inst.rm();
    let rn = inst.rn();

    if rn == Reg::R15 || rd == Reg::R15 || rt == Reg::R15 {
        return unpredictable_instruction(ir);
    }
    if rd == rn || rd == rt {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let value = ir.get_register(rt);
    let half = ir.ir().least_significant_half(value);
    let result = ir.exclusive_write_memory_16(address, half, AccType::Atomic);
    ir.set_register(rd, result);
    true
}

/// ARM STREXD.
pub fn arm_strexd(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rd = inst.rd();
    let rt = inst.rm();
    let rt2_idx = (rt as u32) + 1;
    let rn = inst.rn();

    if rn == Reg::R15 || rd == Reg::R15 || rt == Reg::R14 || (rt as u32) % 2 == 1 {
        return unpredictable_instruction(ir);
    }
    let rt2 = Reg::from_u32(rt2_idx);
    if rd == rn || rd == rt || rd == rt2 {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let lo = ir.get_register(rt);
    let hi = ir.get_register(rt2);
    let result = ir.exclusive_write_memory_64(address, lo, hi, AccType::Atomic);
    ir.set_register(rd, result);
    true
}

/// ARM SWP (deprecated).
/// Atomically swaps a word between register and memory.
/// SWP Rt, Rt2, [Rn]: temp = [Rn]; [Rn] = Rt2; Rt = temp
pub fn arm_swp(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt(); // destination (bits[15:12])
    let rt2 = inst.rm(); // source (bits[3:0])
    let rn = inst.rn(); // address (bits[19:16])

    if rt == Reg::R15 || rt2 == Reg::R15 || rn == Reg::R15 || rn == rt || rn == rt2 {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let old_value = ir.read_memory_32(address, AccType::Atomic);
    let new_value = ir.get_register(rt2);
    ir.write_memory_32(address, new_value, AccType::Atomic);
    ir.set_register(rt, old_value);
    true
}

/// ARM SWPB (deprecated).
/// Atomically swaps a byte between register and memory.
pub fn arm_swpb(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let rt = inst.rt();
    let rt2 = inst.rm();
    let rn = inst.rn();

    if rt == Reg::R15 || rt2 == Reg::R15 || rn == Reg::R15 || rn == rt || rn == rt2 {
        return unpredictable_instruction(ir);
    }

    let address = ir.get_register(rn);
    let old_value = ir.read_memory_8(address, AccType::Atomic);
    let new_value = ir.get_register(rt2);
    let byte = ir.ir().least_significant_byte(new_value);
    ir.write_memory_8(address, byte, AccType::Atomic);
    let extended = ir.ir().zero_extend_byte_to_word(old_value);
    ir.set_register(rt, extended);
    true
}

/// ARM CLREX.
pub fn arm_clrex(ir: &mut A32IREmitter) -> bool {
    ir.clear_exclusive();
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder::{decode_arm, ArmInstId};
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;

    #[test]
    fn arm_stlb_emits_ordered_byte_store_instead_of_exception() {
        let loc = A32LocationDescriptor::new(0x0151FB08, PSR::default(), FPSCR::default(), false);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);
        let inst = decode_arm(0xE1C7_FC91);
        assert_eq!(inst.id, ArmInstId::STLB);

        assert!(arm_stlb(&mut ir, &inst));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32WriteMemory8));
        assert!(!block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32ExceptionRaised));
    }

    #[test]
    fn arm_stlh_emits_ordered_half_store_instead_of_exception() {
        let loc = A32LocationDescriptor::new(0x0151FB00, PSR::default(), FPSCR::default(), false);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);
        let inst = decode_arm(0xE1E0_FC91);
        assert_eq!(inst.id, ArmInstId::STLH);

        assert!(arm_stlh(&mut ir, &inst));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32WriteMemory16));
        assert!(!block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32ExceptionRaised));
    }

    #[test]
    fn arm_strexh_with_rd_pc_raises_unpredictable_instead_of_writing_pc() {
        let loc = A32LocationDescriptor::new(0x01DF8048, PSR::default(), FPSCR::default(), false);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);
        let inst = decode_arm(0xE1E7_FF90);
        assert_eq!(inst.id, ArmInstId::STREXH);

        assert!(!arm_strexh(&mut ir, &inst));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32ExceptionRaised));
        assert!(!block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32BXWritePC));
        assert!(matches!(block.terminal, Terminal::CheckHalt { .. }));
    }

    #[test]
    fn arm_strexb_with_rd_pc_raises_unpredictable_instead_of_writing_pc() {
        let loc = A32LocationDescriptor::new(0x01DF8050, PSR::default(), FPSCR::default(), false);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);
        let inst = decode_arm(0xE1C0_FF92);
        assert_eq!(inst.id, ArmInstId::STREXB);

        assert!(!arm_strexb(&mut ir, &inst));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32ExceptionRaised));
        assert!(!block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32BXWritePC));
        assert!(matches!(block.terminal, Terminal::CheckHalt { .. }));
    }

    #[test]
    fn arm_strexd_emits_single_atomic_exclusive_write_memory_64() {
        let loc = A32LocationDescriptor::new(0x01DF8060, PSR::default(), FPSCR::default(), false);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);
        let inst = decode_arm(0xE1A1_2F94);
        assert_eq!(inst.id, ArmInstId::STREXD);

        assert!(arm_strexd(&mut ir, &inst));
        assert_eq!(
            block
                .instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::A32ExclusiveWriteMemory64)
                .count(),
            1
        );
        assert_eq!(
            block
                .instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::A32ExclusiveWriteMemory32)
                .count(),
            0
        );
    }
}
