//! Thumb32 load/store multiple translation.
//!
//! Upstream owner: `frontend/A32/translate/impl/thumb32_load_store_multiple.cpp`.

use super::helpers::it_block_check;
use crate::frontend::a32::decoder_thumb32::DecodedThumb32;
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::acc_type::AccType;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

fn ldm_helper(
    ir: &mut A32IREmitter<'_>,
    w: bool,
    n: Reg,
    list: u32,
    start_address: Value,
    writeback_address: Value,
) -> bool {
    let mut address = start_address;
    for i in 0..=14 {
        if list & (1 << i) != 0 {
            let value = ir.read_memory_32(address, AccType::Atomic);
            ir.set_register(Reg::from_u32(i), value);
            address = ir
                .ir()
                .add_32(address, Value::ImmU32(4), Value::ImmU1(false));
        }
    }
    if w && list & (1 << n.number()) == 0 {
        ir.set_register(n, writeback_address);
    }
    if list & (1 << 15) != 0 {
        ir.update_upper_location_descriptor();
        let value = ir.read_memory_32(address, AccType::Atomic);
        ir.load_write_pc(value);
        if n == Reg::R13 {
            ir.set_term(Terminal::PopRSBHint);
        } else {
            ir.set_term(Terminal::FastDispatchHint);
        }
        return false;
    }
    true
}

fn stm_helper(
    ir: &mut A32IREmitter<'_>,
    w: bool,
    n: Reg,
    list: u32,
    start_address: Value,
    writeback_address: Value,
) -> bool {
    let mut address = start_address;
    for i in 0..=14 {
        if list & (1 << i) != 0 {
            let value = ir.get_register(Reg::from_u32(i));
            ir.write_memory_32(address, value, AccType::Atomic);
            address = ir
                .ir()
                .add_32(address, Value::ImmU32(4), Value::ImmU1(false));
        }
    }
    if w {
        ir.set_register(n, writeback_address);
    }
    true
}

fn ldmdb(ir: &mut A32IREmitter<'_>, w: bool, n: Reg, list: u32) -> bool {
    let num_regs = list.count_ones();
    if n == Reg::PC || num_regs < 2 {
        return super::unpredictable_instruction(ir);
    }
    if list & (1 << 15) != 0 && list & (1 << 14) != 0 {
        return super::unpredictable_instruction(ir);
    }
    if w && list & (1 << n.number()) != 0 {
        return super::unpredictable_instruction(ir);
    }
    if list & (1 << 13) != 0 {
        return super::unpredictable_instruction(ir);
    }
    if list & (1 << 15) != 0 && it_block_check(ir) {
        return super::unpredictable_instruction(ir);
    }

    let reg_n = ir.get_register(n);
    let start_address = ir
        .ir()
        .sub_32(reg_n, Value::ImmU32(4 * num_regs), Value::ImmU1(true));
    ldm_helper(ir, w, n, list, start_address, start_address)
}

fn ldmia(ir: &mut A32IREmitter<'_>, w: bool, n: Reg, list: u32) -> bool {
    let num_regs = list.count_ones();
    if n == Reg::PC || num_regs < 2 {
        return super::unpredictable_instruction(ir);
    }
    if list & (1 << 15) != 0 && list & (1 << 14) != 0 {
        return super::unpredictable_instruction(ir);
    }
    if w && list & (1 << n.number()) != 0 {
        return super::unpredictable_instruction(ir);
    }
    if list & (1 << 13) != 0 {
        return super::unpredictable_instruction(ir);
    }
    if list & (1 << 15) != 0 && it_block_check(ir) {
        return super::unpredictable_instruction(ir);
    }

    let start_address = ir.get_register(n);
    let writeback_address = ir.ir().add_32(
        start_address,
        Value::ImmU32(num_regs * 4),
        Value::ImmU1(false),
    );
    ldm_helper(ir, w, n, list, start_address, writeback_address)
}

fn stmia(ir: &mut A32IREmitter<'_>, w: bool, n: Reg, list: u32) -> bool {
    let num_regs = list.count_ones();
    if n == Reg::PC || num_regs < 2 {
        return super::unpredictable_instruction(ir);
    }
    if w && list & (1 << n.number()) != 0 {
        return super::unpredictable_instruction(ir);
    }
    if list & (1 << 13) != 0 {
        return super::unpredictable_instruction(ir);
    }

    let start_address = ir.get_register(n);
    let writeback_address = ir.ir().add_32(
        start_address,
        Value::ImmU32(num_regs * 4),
        Value::ImmU1(false),
    );
    stm_helper(ir, w, n, list, start_address, writeback_address)
}

fn stmdb(ir: &mut A32IREmitter<'_>, w: bool, n: Reg, list: u32) -> bool {
    let num_regs = list.count_ones();
    if n == Reg::PC || num_regs < 2 {
        return super::unpredictable_instruction(ir);
    }
    if w && list & (1 << n.number()) != 0 {
        return super::unpredictable_instruction(ir);
    }
    if list & (1 << 13) != 0 {
        return super::unpredictable_instruction(ir);
    }

    let reg_n = ir.get_register(n);
    let start_address = ir
        .ir()
        .sub_32(reg_n, Value::ImmU32(4 * num_regs), Value::ImmU1(true));
    stm_helper(ir, w, n, list, start_address, start_address)
}

pub fn thumb32_ldmdb(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    ldmdb(
        ir,
        inst.raw & (1 << 21) != 0,
        inst.rn(),
        u32::from(inst.register_list()),
    )
}

pub fn thumb32_ldmia(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    ldmia(
        ir,
        inst.raw & (1 << 21) != 0,
        inst.rn(),
        u32::from(inst.register_list()),
    )
}

pub fn thumb32_pop(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    ldmia(ir, true, Reg::SP, u32::from(inst.register_list()))
}

pub fn thumb32_push(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    stmdb(ir, true, Reg::SP, u32::from(inst.register_list() & 0x7fff))
}

pub fn thumb32_stmia(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    stmia(
        ir,
        inst.raw & (1 << 21) != 0,
        inst.rn(),
        u32::from(inst.register_list() & 0x7fff),
    )
}

pub fn thumb32_stmdb(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    stmdb(
        ir,
        inst.raw & (1 << 21) != 0,
        inst.rn(),
        u32::from(inst.register_list() & 0x7fff),
    )
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

    fn location(it: u8) -> A32LocationDescriptor {
        let mut psr = PSR::default();
        psr.set_t(true);
        A32LocationDescriptor::new(0x1000, psr, FPSCR::default(), false).set_it(ITState::new(it))
    }

    fn decoded(raw: u32, id: Thumb32InstId) -> DecodedThumb32 {
        DecodedThumb32 { raw, id }
    }

    fn has_operand_side_effect(block: &Block) -> bool {
        block.instructions.iter().any(|inst| {
            matches!(
                inst.opcode,
                Opcode::A32GetRegister | Opcode::A32ReadMemory32 | Opcode::A32WriteMemory32
            )
        })
    }

    #[test]
    fn invalid_loads_are_unpredictable_before_operand_access() {
        for (raw, id, translate, it) in [
            (
                0xE8B1_0003u32,
                Thumb32InstId::LDMIA,
                thumb32_ldmia as fn(&mut A32IREmitter<'_>, &DecodedThumb32) -> bool,
                0,
            ),
            (0xE891_C000, Thumb32InstId::LDMIA, thumb32_ldmia, 0),
            (0xE891_2001, Thumb32InstId::LDMIA, thumb32_ldmia, 0),
            (0xE891_8001, Thumb32InstId::LDMIA, thumb32_ldmia, 0x0c),
        ] {
            let loc = location(it);
            let mut block = Block::new(loc.to_location());
            let inst = decoded(raw, id);
            {
                let mut ir = A32IREmitter::with_location(&mut block, loc);
                assert!(!translate(&mut ir, &inst));
            }
            assert!(!has_operand_side_effect(&block), "raw={raw:08X}");
            assert!(block
                .instructions
                .iter()
                .any(|inst| inst.opcode == Opcode::A32ExceptionRaised));
        }
    }

    #[test]
    fn invalid_stores_raise_unpredictable_instead_of_stopping_silently() {
        let loc = location(0);
        let mut block = Block::new(loc.to_location());
        let inst = decoded(0xE8A1_0002, Thumb32InstId::STMIA);
        {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            assert!(!thumb32_stmia(&mut ir, &inst));
        }
        assert!(!has_operand_side_effect(&block));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32ExceptionRaised));
    }

    #[test]
    fn ldmia_preserves_atomic_load_register_and_writeback_order() {
        let loc = location(0);
        let mut block = Block::new(loc.to_location());
        let inst = decoded(0xE8B4_0005, Thumb32InstId::LDMIA);
        {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            assert!(thumb32_ldmia(&mut ir, &inst));
        }
        let reads = block
            .instructions
            .iter()
            .filter(|inst| inst.opcode == Opcode::A32ReadMemory32)
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 2);
        assert!(reads
            .iter()
            .all(|inst| inst.args[2] == Value::ImmAccType(AccType::Atomic)));
        let sets = block
            .instructions
            .iter()
            .filter(|inst| inst.opcode == Opcode::A32SetRegister)
            .collect::<Vec<_>>();
        assert_eq!(sets.len(), 3);
        assert_eq!(sets[0].args[0], Value::ImmA32Reg(Reg::R0));
        assert_eq!(sets[1].args[0], Value::ImmA32Reg(Reg::R2));
        assert_eq!(sets[2].args[0], Value::ImmA32Reg(Reg::R4));
    }

    #[test]
    fn pop_updates_location_before_pc_read_and_uses_pop_terminal() {
        let loc = location(0);
        let mut block = Block::new(loc.to_location());
        let raw = 0xE8BD_8003u32;
        let inst = decode_thumb32((raw >> 16) as u16, raw as u16);
        assert_eq!(inst.id, Thumb32InstId::POP);
        {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            assert!(!thumb32_pop(&mut ir, &inst));
        }
        let update = block
            .instructions
            .iter()
            .rposition(|inst| inst.opcode == Opcode::A32UpdateUpperLocationDescriptor)
            .expect("upper-location update");
        let pc_read = block
            .instructions
            .iter()
            .rposition(|inst| inst.opcode == Opcode::A32ReadMemory32)
            .expect("PC load");
        assert!(update < pc_read);
        assert!(matches!(block.terminal, Terminal::PopRSBHint));
    }

    #[test]
    fn stmdb_uses_one_start_address_for_writeback_after_atomic_stores() {
        let loc = location(0);
        let mut block = Block::new(loc.to_location());
        let inst = decoded(0xE924_0005, Thumb32InstId::STMDB);
        {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            assert!(thumb32_stmdb(&mut ir, &inst));
        }
        let writes = block
            .instructions
            .iter()
            .filter(|inst| inst.opcode == Opcode::A32WriteMemory32)
            .collect::<Vec<_>>();
        assert_eq!(writes.len(), 2);
        assert!(writes
            .iter()
            .all(|inst| inst.args[3] == Value::ImmAccType(AccType::Atomic)));
        let final_set = block
            .instructions
            .iter()
            .rposition(|inst| inst.opcode == Opcode::A32SetRegister)
            .expect("writeback");
        let final_write = block
            .instructions
            .iter()
            .rposition(|inst| inst.opcode == Opcode::A32WriteMemory32)
            .expect("store");
        assert!(final_write < final_set);
        assert_eq!(
            block.instructions[final_set].args[0],
            Value::ImmA32Reg(Reg::R4)
        );
    }
}
