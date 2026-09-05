//! Thumb32 load/store dual, load/store exclusive, and table-branch translation.
//!
//! Upstream owner: `frontend/A32/translate/impl/thumb32_load_store_dual.cpp`.

use super::helpers::it_block_check;
use crate::frontend::a32::decoder_thumb32::DecodedThumb32;
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::acc_type::AccType;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

fn table_branch(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32, half: bool) -> bool {
    let n = inst.rn();
    let m = inst.rm();
    if m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    if it_block_check(ir) {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let halfwords = if half {
        let offset = ir
            .ir()
            .logical_shift_left_32(reg_m, Value::ImmU8(1), Value::ImmU1(false));
        let address = ir.ir().add_32(reg_n, offset, Value::ImmU1(false));
        let data = ir.read_memory_16(address, AccType::Normal);
        ir.ir().zero_extend_half_to_word(data)
    } else {
        let address = ir.ir().add_32(reg_n, reg_m, Value::ImmU1(false));
        let data = ir.read_memory_8(address, AccType::Normal);
        ir.ir().zero_extend_byte_to_word(data)
    };

    let current_pc = Value::ImmU32(ir.pc());
    let doubled = ir.ir().add_32(halfwords, halfwords, Value::ImmU1(false));
    let branch_value = ir.ir().add_32(current_pc, doubled, Value::ImmU1(false));
    ir.update_upper_location_descriptor();
    ir.branch_write_pc(branch_value);
    ir.set_term(Terminal::FastDispatchHint);
    false
}

fn load_dual_immediate(
    ir: &mut A32IREmitter<'_>,
    inst: &DecodedThumb32,
    p: bool,
    u: bool,
    w: bool,
) -> bool {
    let n = inst.rn();
    let t = inst.rt();
    let t2 = inst.rt2();
    if w && (n == t || n == t2) {
        return super::unpredictable_instruction(ir);
    }
    if t == Reg::PC || t2 == Reg::PC || t == t2 {
        return super::unpredictable_instruction(ir);
    }

    let imm = inst.imm8() << 2;
    let reg_n = ir.get_register(n);
    let offset_address = if u {
        ir.ir()
            .add_32(reg_n, Value::ImmU32(imm), Value::ImmU1(false))
    } else {
        ir.ir()
            .sub_32(reg_n, Value::ImmU32(imm), Value::ImmU1(true))
    };
    let address = if p { offset_address } else { reg_n };
    let data = ir.read_memory_64(address, AccType::Atomic);
    let e_flag = ir
        .current_location
        .expect("current_location not set")
        .e_flag();
    if e_flag {
        let hi = ir.ir().most_significant_word(data).result;
        ir.set_register(t, hi);
        let lo = ir.ir().least_significant_word(data);
        ir.set_register(t2, lo);
    } else {
        let lo = ir.ir().least_significant_word(data);
        ir.set_register(t, lo);
        let hi = ir.ir().most_significant_word(data).result;
        ir.set_register(t2, hi);
    }
    if w {
        ir.set_register(n, offset_address);
    }
    true
}

fn load_dual_literal(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32, u: bool, w: bool) -> bool {
    let t = inst.rt();
    let t2 = inst.rt2();
    if t == Reg::PC || t2 == Reg::PC || t == t2 {
        return super::unpredictable_instruction(ir);
    }
    if w {
        return super::unpredictable_instruction(ir);
    }

    let imm = inst.imm8() << 2;
    let base = Value::ImmU32(ir.align_pc(4));
    let address = if u {
        ir.ir()
            .add_32(base, Value::ImmU32(imm), Value::ImmU1(false))
    } else {
        ir.ir().sub_32(base, Value::ImmU32(imm), Value::ImmU1(true))
    };
    let data = ir.read_memory_64(address, AccType::Atomic);
    let e_flag = ir
        .current_location
        .expect("current_location not set")
        .e_flag();
    if e_flag {
        let hi = ir.ir().most_significant_word(data).result;
        ir.set_register(t, hi);
        let lo = ir.ir().least_significant_word(data);
        ir.set_register(t2, lo);
    } else {
        let lo = ir.ir().least_significant_word(data);
        ir.set_register(t, lo);
        let hi = ir.ir().most_significant_word(data).result;
        ir.set_register(t2, hi);
    }
    true
}

fn store_dual(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32, p: bool, u: bool, w: bool) -> bool {
    let n = inst.rn();
    let t = inst.rt();
    let t2 = inst.rt2();
    if w && (n == t || n == t2) {
        return super::unpredictable_instruction(ir);
    }
    if n == Reg::PC || t == Reg::PC || t2 == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let imm = inst.imm8() << 2;
    let reg_n = ir.get_register(n);
    let reg_t = ir.get_register(t);
    let reg_t2 = ir.get_register(t2);
    let offset_address = if u {
        ir.ir()
            .add_32(reg_n, Value::ImmU32(imm), Value::ImmU1(false))
    } else {
        ir.ir()
            .sub_32(reg_n, Value::ImmU32(imm), Value::ImmU1(true))
    };
    let address = if p { offset_address } else { reg_n };
    let e_flag = ir
        .current_location
        .expect("current_location not set")
        .e_flag();
    let data = if e_flag {
        ir.ir().pack_2x32_to_1x64(reg_t2, reg_t)
    } else {
        ir.ir().pack_2x32_to_1x64(reg_t, reg_t2)
    };
    ir.write_memory_64(address, data, AccType::Atomic);
    if w {
        ir.set_register(n, offset_address);
    }
    true
}

pub fn thumb32_lda(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let t = inst.rt();
    if t == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    let address = ir.get_register(n);
    let value = ir.read_memory_32(address, AccType::Ordered);
    ir.set_register(t, value);
    true
}

pub fn thumb32_ldrd_imm_1(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    load_dual_immediate(ir, inst, false, (inst.raw >> 23) & 1 != 0, true)
}

pub fn thumb32_ldrd_imm_2(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    load_dual_immediate(
        ir,
        inst,
        true,
        (inst.raw >> 23) & 1 != 0,
        (inst.raw >> 21) & 1 != 0,
    )
}

pub fn thumb32_ldrd_lit_1(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    load_dual_literal(ir, inst, (inst.raw >> 23) & 1 != 0, true)
}

pub fn thumb32_ldrd_lit_2(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    load_dual_literal(
        ir,
        inst,
        (inst.raw >> 23) & 1 != 0,
        (inst.raw >> 21) & 1 != 0,
    )
}

pub fn thumb32_strd_imm_1(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    store_dual(ir, inst, false, (inst.raw >> 23) & 1 != 0, true)
}

pub fn thumb32_strd_imm_2(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    store_dual(
        ir,
        inst,
        true,
        (inst.raw >> 23) & 1 != 0,
        (inst.raw >> 21) & 1 != 0,
    )
}

pub fn thumb32_ldrex(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let t = inst.rt();
    if t == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    let reg_n = ir.get_register(n);
    let address = ir
        .ir()
        .add_32(reg_n, Value::ImmU32(inst.imm8() << 2), Value::ImmU1(false));
    let value = ir.exclusive_read_memory_32(address, AccType::Atomic);
    ir.set_register(t, value);
    true
}

pub fn thumb32_ldrexb(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let t = inst.rt();
    if t == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    let address = ir.get_register(n);
    let value = ir.exclusive_read_memory_8(address, AccType::Atomic);
    let value = ir.ir().zero_extend_byte_to_word(value);
    ir.set_register(t, value);
    true
}

pub fn thumb32_ldrexd(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let t = inst.rt();
    let t2 = inst.rt2();
    if t == Reg::PC || t2 == Reg::PC || t == t2 || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    let address = ir.get_register(n);
    let (lo, hi) = ir.exclusive_read_memory_64(address, AccType::Atomic);
    ir.set_register(t, lo);
    ir.set_register(t2, hi);
    true
}

pub fn thumb32_ldrexh(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let t = inst.rt();
    if t == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    let address = ir.get_register(n);
    let value = ir.exclusive_read_memory_16(address, AccType::Atomic);
    let value = ir.ir().zero_extend_half_to_word(value);
    ir.set_register(t, value);
    true
}

pub fn thumb32_stl(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let t = inst.rt();
    if t == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    let address = ir.get_register(n);
    let value = ir.get_register(t);
    ir.write_memory_32(address, value, AccType::Ordered);
    true
}

pub fn thumb32_strex(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let t = inst.rt();
    let d = inst.rd();
    if d == Reg::PC || t == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    if d == n || d == t {
        return super::unpredictable_instruction(ir);
    }
    let reg_n = ir.get_register(n);
    let address = ir
        .ir()
        .add_32(reg_n, Value::ImmU32(inst.imm8() << 2), Value::ImmU1(false));
    let value = ir.get_register(t);
    let passed = ir.exclusive_write_memory_32(address, value, AccType::Atomic);
    ir.set_register(d, passed);
    true
}

pub fn thumb32_strexb(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let t = inst.rt();
    let d = inst.rm();
    if d == Reg::PC || t == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    if d == n || d == t {
        return super::unpredictable_instruction(ir);
    }
    let address = ir.get_register(n);
    let value = ir.get_register(t);
    let value = ir.ir().least_significant_byte(value);
    let passed = ir.exclusive_write_memory_8(address, value, AccType::Atomic);
    ir.set_register(d, passed);
    true
}

pub fn thumb32_strexd(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let t = inst.rt();
    let t2 = inst.rt2();
    let d = inst.rm();
    if d == Reg::PC || t == Reg::PC || t2 == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    if d == n || d == t || d == t2 {
        return super::unpredictable_instruction(ir);
    }
    let address = ir.get_register(n);
    let value_lo = ir.get_register(t);
    let value_hi = ir.get_register(t2);
    let passed = ir.exclusive_write_memory_64(address, value_lo, value_hi, AccType::Atomic);
    ir.set_register(d, passed);
    true
}

pub fn thumb32_strexh(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let t = inst.rt();
    let d = inst.rm();
    if d == Reg::PC || t == Reg::PC || n == Reg::PC {
        return super::unpredictable_instruction(ir);
    }
    if d == n || d == t {
        return super::unpredictable_instruction(ir);
    }
    let address = ir.get_register(n);
    let value = ir.get_register(t);
    let value = ir.ir().least_significant_half(value);
    let passed = ir.exclusive_write_memory_16(address, value, AccType::Atomic);
    ir.set_register(d, passed);
    true
}

pub fn thumb32_tbb(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    table_branch(ir, inst, false)
}

pub fn thumb32_tbh(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    table_branch(ir, inst, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder_thumb32::{decode_thumb32, Thumb32InstId};
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::it_state::ITState;
    use crate::frontend::a32::psr::PSR;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;

    fn location(e: bool, it: u8) -> A32LocationDescriptor {
        let mut psr = PSR::default();
        psr.set_t(true);
        psr.set_e(e);
        A32LocationDescriptor::new(0x1000, psr, FPSCR::default(), false).set_it(ITState::new(it))
    }

    fn decoded(raw: u32, id: Thumb32InstId) -> DecodedThumb32 {
        DecodedThumb32 { raw, id }
    }

    #[test]
    fn dual_load_preserves_atomic_endian_and_writeback_order() {
        let loc = location(true, 0);
        let mut block = Block::new(loc.to_location());
        let inst = decoded(0xE9F1_2304, Thumb32InstId::LdrdImm2);
        {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            assert!(thumb32_ldrd_imm_2(&mut ir, &inst));
        }
        let read = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::A32ReadMemory64)
            .expect("dual read");
        assert_eq!(read.args[2], Value::ImmAccType(AccType::Atomic));
        let hi = block
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::MostSignificantWord)
            .expect("high word");
        let lo = block
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::LeastSignificantWord)
            .expect("low word");
        assert!(hi < lo);
        let sets = block
            .instructions
            .iter()
            .filter(|inst| inst.opcode == Opcode::A32SetRegister)
            .collect::<Vec<_>>();
        assert_eq!(sets[0].args[0], Value::ImmA32Reg(Reg::R2));
        assert_eq!(sets[1].args[0], Value::ImmA32Reg(Reg::R3));
        assert_eq!(sets[2].args[0], Value::ImmA32Reg(Reg::R1));
    }

    #[test]
    fn dual_literal_and_store_validation_precede_operands() {
        for (raw, id, translate) in [
            (
                0xE87F_2304,
                Thumb32InstId::LdrdLit1,
                thumb32_ldrd_lit_1 as fn(&mut A32IREmitter<'_>, &DecodedThumb32) -> bool,
            ),
            (0xE9E2_2304, Thumb32InstId::StrdImm2, thumb32_strd_imm_2),
        ] {
            let loc = location(false, 0);
            let mut block = Block::new(loc.to_location());
            let inst = decoded(raw, id);
            {
                let mut ir = A32IREmitter::with_location(&mut block, loc);
                assert!(!translate(&mut ir, &inst));
            }
            assert!(!block.instructions.iter().any(|inst| matches!(
                inst.opcode,
                Opcode::A32GetRegister | Opcode::A32ReadMemory64 | Opcode::A32WriteMemory64
            )));
        }
    }

    #[test]
    fn ordered_and_exclusive_families_use_upstream_access_types() {
        for (raw, expected, translate, opcode, access_index, access) in [
            (
                0xE8D1_2FAFu32,
                Thumb32InstId::LDA,
                thumb32_lda as fn(&mut A32IREmitter<'_>, &DecodedThumb32) -> bool,
                Opcode::A32ReadMemory32,
                2,
                AccType::Ordered,
            ),
            (
                0xE8C1_2FAF,
                Thumb32InstId::STL,
                thumb32_stl,
                Opcode::A32WriteMemory32,
                3,
                AccType::Ordered,
            ),
            (
                0xE851_2F04,
                Thumb32InstId::LDREX,
                thumb32_ldrex,
                Opcode::A32ExclusiveReadMemory32,
                2,
                AccType::Atomic,
            ),
            (
                0xE841_2304,
                Thumb32InstId::STREX,
                thumb32_strex,
                Opcode::A32ExclusiveWriteMemory32,
                3,
                AccType::Atomic,
            ),
        ] {
            let loc = location(false, 0);
            let mut block = Block::new(loc.to_location());
            let inst = decode_thumb32((raw >> 16) as u16, raw as u16);
            assert_eq!(inst.id, expected);
            {
                let mut ir = A32IREmitter::with_location(&mut block, loc);
                assert!(translate(&mut ir, &inst));
            }
            let memory = block
                .instructions
                .iter()
                .find(|inst| inst.opcode == opcode)
                .expect("memory operation");
            assert_eq!(memory.args[access_index], Value::ImmAccType(access));
        }
    }

    #[test]
    fn byte_half_and_dual_exclusives_are_implemented() {
        for (raw, expected, translate, opcode) in [
            (
                0xE8D1_2F4Fu32,
                Thumb32InstId::LDREXB,
                thumb32_ldrexb as fn(&mut A32IREmitter<'_>, &DecodedThumb32) -> bool,
                Opcode::A32ExclusiveReadMemory8,
            ),
            (
                0xE8D1_2F5F,
                Thumb32InstId::LDREXH,
                thumb32_ldrexh,
                Opcode::A32ExclusiveReadMemory16,
            ),
            (
                0xE8D1_247F,
                Thumb32InstId::LDREXD,
                thumb32_ldrexd,
                Opcode::A32ExclusiveReadMemory64,
            ),
            (
                0xE8C1_2F43,
                Thumb32InstId::STREXB,
                thumb32_strexb,
                Opcode::A32ExclusiveWriteMemory8,
            ),
            (
                0xE8C1_2F53,
                Thumb32InstId::STREXH,
                thumb32_strexh,
                Opcode::A32ExclusiveWriteMemory16,
            ),
            (
                0xE8C1_2473,
                Thumb32InstId::STREXD,
                thumb32_strexd,
                Opcode::A32ExclusiveWriteMemory64,
            ),
        ] {
            let loc = location(false, 0);
            let mut block = Block::new(loc.to_location());
            let inst = decode_thumb32((raw >> 16) as u16, raw as u16);
            assert_eq!(inst.id, expected);
            {
                let mut ir = A32IREmitter::with_location(&mut block, loc);
                assert!(translate(&mut ir, &inst));
            }
            assert!(block.instructions.iter().any(|inst| inst.opcode == opcode));
        }
    }

    #[test]
    fn table_branches_validate_it_and_select_the_read_width() {
        let loc = location(false, 0x0c);
        let mut block = Block::new(loc.to_location());
        let inst = decoded(0xE8D1_F003, Thumb32InstId::TBB);
        {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            assert!(!thumb32_tbb(&mut ir, &inst));
        }
        assert!(!block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, Opcode::A32GetRegister | Opcode::A32ReadMemory8)));

        for (raw, expected, translate, read) in [
            (
                0xE8D1_F003u32,
                Thumb32InstId::TBB,
                thumb32_tbb as fn(&mut A32IREmitter<'_>, &DecodedThumb32) -> bool,
                Opcode::A32ReadMemory8,
            ),
            (
                0xE8D1_F013,
                Thumb32InstId::TBH,
                thumb32_tbh,
                Opcode::A32ReadMemory16,
            ),
        ] {
            let loc = location(false, 0);
            let mut block = Block::new(loc.to_location());
            let inst = decode_thumb32((raw >> 16) as u16, raw as u16);
            assert_eq!(inst.id, expected);
            {
                let mut ir = A32IREmitter::with_location(&mut block, loc);
                assert!(!translate(&mut ir, &inst));
            }
            assert!(block.instructions.iter().any(|inst| inst.opcode == read));
            assert!(matches!(block.terminal, Terminal::FastDispatchHint));
        }
    }
}
