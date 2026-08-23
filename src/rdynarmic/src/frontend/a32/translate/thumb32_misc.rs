//! Thumb32 miscellaneous data-processing operations.
//!
//! Upstream owner: `frontend/A32/translate/impl/thumb32_misc.cpp`.

use crate::frontend::a32::decoder_thumb32::DecodedThumb32;
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::value::Value;

pub fn thumb32_clz(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if m != n || d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let result = ir.ir().count_leading_zeros_32(reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_qadd(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().signed_saturated_add_with_flag(reg_m, reg_n);
    ir.set_register(d, result.result);
    ir.or_q_flag(result.overflow);
    true
}

pub fn thumb32_qdadd(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let doubled_n = ir.ir().signed_saturated_add_with_flag(reg_n, reg_n);
    ir.or_q_flag(doubled_n.overflow);
    let result = ir
        .ir()
        .signed_saturated_add_with_flag(reg_m, doubled_n.result);
    ir.set_register(d, result.result);
    ir.or_q_flag(result.overflow);
    true
}

pub fn thumb32_qdsub(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let doubled_n = ir.ir().signed_saturated_add_with_flag(reg_n, reg_n);
    ir.or_q_flag(doubled_n.overflow);
    let result = ir
        .ir()
        .signed_saturated_sub_with_flag(reg_m, doubled_n.result);
    ir.set_register(d, result.result);
    ir.or_q_flag(result.overflow);
    true
}

pub fn thumb32_qsub(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let result = ir.ir().signed_saturated_sub_with_flag(reg_m, reg_n);
    ir.set_register(d, result.result);
    ir.or_q_flag(result.overflow);
    true
}

pub fn thumb32_rbit(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if m != n || d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let swapped = ir.ir().byte_reverse_word(reg_m);
    let masked = ir.ir().and_32(swapped, Value::ImmU32(0xf0f0_f0f0));
    let first_lsr = ir
        .ir()
        .logical_shift_right_32(masked, Value::ImmU8(4), Value::ImmU1(false));
    let masked = ir.ir().and_32(swapped, Value::ImmU32(0x0f0f_0f0f));
    let first_lsl = ir
        .ir()
        .logical_shift_left_32(masked, Value::ImmU8(4), Value::ImmU1(false));
    let corrected = ir.ir().or_32(first_lsl, first_lsr);

    let masked = ir.ir().and_32(corrected, Value::ImmU32(0x8888_8888));
    let second_lsr = ir
        .ir()
        .logical_shift_right_32(masked, Value::ImmU8(3), Value::ImmU1(false));
    let masked = ir.ir().and_32(corrected, Value::ImmU32(0x4444_4444));
    let third_lsr = ir
        .ir()
        .logical_shift_right_32(masked, Value::ImmU8(1), Value::ImmU1(false));
    let masked = ir.ir().and_32(corrected, Value::ImmU32(0x2222_2222));
    let second_lsl = ir
        .ir()
        .logical_shift_left_32(masked, Value::ImmU8(1), Value::ImmU1(false));
    let masked = ir.ir().and_32(corrected, Value::ImmU32(0x1111_1111));
    let third_lsl = ir
        .ir()
        .logical_shift_left_32(masked, Value::ImmU8(3), Value::ImmU1(false));

    let result = ir.ir().or_32(second_lsr, third_lsr);
    let result = ir.ir().or_32(result, second_lsl);
    let result = ir.ir().or_32(result, third_lsl);
    ir.set_register(d, result);
    true
}

pub fn thumb32_rev(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if m != n || d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let result = ir.ir().byte_reverse_word(reg_m);
    ir.set_register(d, result);
    true
}

pub fn thumb32_rev16(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if m != n || d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let lo_shift = ir
        .ir()
        .logical_shift_right_32(reg_m, Value::ImmU8(8), Value::ImmU1(false));
    let lo = ir.ir().and_32(lo_shift, Value::ImmU32(0x00ff_00ff));
    let hi_shift = ir
        .ir()
        .logical_shift_left_32(reg_m, Value::ImmU8(8), Value::ImmU1(false));
    let hi = ir.ir().and_32(hi_shift, Value::ImmU32(0xff00_ff00));
    let result = ir.ir().or_32(lo, hi);
    ir.set_register(d, result);
    true
}

pub fn thumb32_revsh(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if m != n || d == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let half = ir.ir().least_significant_half(reg_m);
    let rev_half = ir.ir().byte_reverse_half(half);
    let result = ir.ir().sign_extend_half_to_word(rev_half);
    ir.set_register(d, result);
    true
}

pub fn thumb32_sel(ir: &mut A32IREmitter<'_>, inst: &DecodedThumb32) -> bool {
    let n = inst.rn();
    let d = inst.rd();
    let m = inst.rm();
    if d == Reg::PC || n == Reg::PC || m == Reg::PC {
        return super::unpredictable_instruction(ir);
    }

    let reg_m = ir.get_register(m);
    let reg_n = ir.get_register(n);
    let ge = ir.get_ge_flags();
    let result = ir.ir().packed_select(ge, reg_m, reg_n);
    ir.set_register(d, result);
    true
}

#[cfg(test)]
mod tests {
    use crate::frontend::a32::decoder_thumb32::{decode_thumb32, Thumb32InstId};
    use crate::frontend::a32::types::{Exception, Reg};
    use crate::ir::a32_emitter::A32IREmitter;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::value::Value;

    const INSTRUCTIONS: &[(u32, Thumb32InstId)] = &[
        (0xfa81_f283, Thumb32InstId::QADD),
        (0xfa81_f293, Thumb32InstId::QDADD),
        (0xfa81_f2a3, Thumb32InstId::QSUB),
        (0xfa81_f2b3, Thumb32InstId::QDSUB),
        (0xfa93_f283, Thumb32InstId::REV),
        (0xfa93_f293, Thumb32InstId::REV16),
        (0xfa93_f2a3, Thumb32InstId::RBIT),
        (0xfa93_f2b3, Thumb32InstId::REVSH),
        (0xfaa1_f283, Thumb32InstId::SEL),
        (0xfab3_f283, Thumb32InstId::CLZ),
    ];

    fn translate(raw: u32) -> (bool, Block) {
        let location = A32LocationDescriptor::at(0x1000).set_t_flag(true);
        let decoded = decode_thumb32((raw >> 16) as u16, raw as u16);
        let mut block = Block::new(location.to_location());
        let result = {
            let mut ir = A32IREmitter::with_location(&mut block, location);
            super::super::thumb32::translate_thumb32(
                &mut ir,
                &decoded,
                super::super::TranslationOptions::default(),
            )
        };
        (result, block)
    }

    #[test]
    fn all_misc_patterns_decode_and_translate() {
        for &(raw, id) in INSTRUCTIONS {
            assert_eq!(decode_thumb32((raw >> 16) as u16, raw as u16).id, id);
            assert!(translate(raw).0, "raw={raw:08X}");
        }
    }

    #[test]
    fn duplicated_register_mismatch_is_unpredictable_before_reads() {
        let (result, block) = translate(0xfa91_f283);
        assert!(!result);
        assert!(!block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32GetRegister));
        assert!(block.instructions.iter().any(|inst| {
            inst.opcode == Opcode::A32ExceptionRaised
                && inst.args[1]
                    == Value::ImmU64(Exception::UnpredictableInstruction.as_u32() as u64)
        }));
    }

    #[test]
    fn qdadd_sets_first_q_before_second_saturation_and_destination_before_final_q() {
        let (_, block) = translate(0xfa81_f293);
        let saturations = block
            .instructions
            .iter()
            .enumerate()
            .filter(|(_, inst)| inst.opcode == Opcode::SignedSaturatedAddWithFlag32)
            .map(|(position, _)| position)
            .collect::<Vec<_>>();
        let q_flags = block
            .instructions
            .iter()
            .enumerate()
            .filter(|(_, inst)| inst.opcode == Opcode::A32OrQFlag)
            .map(|(position, _)| position)
            .collect::<Vec<_>>();
        let destination = block
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::A32SetRegister)
            .unwrap();
        assert_eq!(saturations.len(), 2);
        assert_eq!(q_flags.len(), 2);
        assert!(saturations[0] < q_flags[0] && q_flags[0] < saturations[1]);
        assert!(saturations[1] < destination && destination < q_flags[1]);
    }

    #[test]
    fn rbit_and_sel_use_exact_upstream_ir_families_and_order() {
        let (_, rbit) = translate(0xfa93_f2a3);
        assert_eq!(
            rbit.instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::And32)
                .count(),
            6
        );
        assert_eq!(
            rbit.instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::Or32)
                .count(),
            4
        );

        let (_, sel) = translate(0xfaa1_f283);
        let opcodes = sel
            .instructions
            .iter()
            .map(|inst| inst.opcode)
            .collect::<Vec<_>>();
        assert_eq!(
            opcodes,
            vec![
                Opcode::A32GetRegister,
                Opcode::A32GetRegister,
                Opcode::A32GetGEFlags,
                Opcode::PackedSelect,
                Opcode::A32SetRegister,
            ]
        );
        assert_eq!(sel.instructions[0].args[0], Value::ImmA32Reg(Reg::R3));
        assert_eq!(sel.instructions[1].args[0], Value::ImmA32Reg(Reg::R1));
    }
}
