use super::helpers::{emit_imm_shift, get_address};
use super::thumb32_control;
use super::thumb32_coprocessor;
use crate::frontend::a32::decoder_thumb32::{DecodedThumb32, Thumb32InstId};
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::acc_type::AccType;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

use super::TranslationOptions;

fn thumb32_unpredictable(ir: &mut A32IREmitter) -> bool {
    // Full RaiseException lifecycle (UpdateUpperLocationDescriptor +
    // BranchWritePC(PC+4) + terminal), not just the bare opcode + terminal.
    super::unpredictable_instruction(ir)
}

/// Translate a single Thumb32 instruction. Returns true to continue.
pub fn translate_thumb32(
    ir: &mut A32IREmitter,
    inst: &DecodedThumb32,
    options: TranslationOptions,
) -> bool {
    use Thumb32InstId::*;
    match inst.id {
        // Data processing (modified immediate)
        AND_imm => thumb32_dp_imm(ir, inst, DpOp::And),
        TST_imm => thumb32_dp_imm(ir, inst, DpOp::Tst),
        BIC_imm => thumb32_dp_imm(ir, inst, DpOp::Bic),
        ORR_imm => thumb32_dp_imm(ir, inst, DpOp::Orr),
        MOV_imm => thumb32_dp_imm(ir, inst, DpOp::Mov),
        ORN_imm => thumb32_dp_imm(ir, inst, DpOp::Orn),
        MVN_imm => thumb32_dp_imm(ir, inst, DpOp::Mvn),
        EOR_imm => thumb32_dp_imm(ir, inst, DpOp::Eor),
        TEQ_imm => thumb32_dp_imm(ir, inst, DpOp::Teq),
        ADD_imm => thumb32_dp_imm(ir, inst, DpOp::Add),
        CMN_imm => thumb32_dp_imm(ir, inst, DpOp::Cmn),
        ADC_imm => thumb32_dp_imm(ir, inst, DpOp::Adc),
        SBC_imm => thumb32_dp_imm(ir, inst, DpOp::Sbc),
        SUB_imm => thumb32_dp_imm(ir, inst, DpOp::Sub),
        CMP_imm => thumb32_dp_imm(ir, inst, DpOp::Cmp),
        RSB_imm => thumb32_dp_imm(ir, inst, DpOp::Rsb),

        // Data processing (shifted register)
        AND_reg => thumb32_dp_reg(ir, inst, DpOp::And),
        TST_reg => thumb32_dp_reg(ir, inst, DpOp::Tst),
        BIC_reg => thumb32_dp_reg(ir, inst, DpOp::Bic),
        ORR_reg => thumb32_dp_reg(ir, inst, DpOp::Orr),
        MOV_reg => thumb32_dp_reg(ir, inst, DpOp::Mov),
        ORN_reg => thumb32_dp_reg(ir, inst, DpOp::Orn),
        MVN_reg => thumb32_dp_reg(ir, inst, DpOp::Mvn),
        EOR_reg => thumb32_dp_reg(ir, inst, DpOp::Eor),
        TEQ_reg => thumb32_dp_reg(ir, inst, DpOp::Teq),
        PKH => thumb32_dp_reg(ir, inst, DpOp::Pkh),
        ADD_reg => thumb32_dp_reg(ir, inst, DpOp::Add),
        CMN_reg => thumb32_dp_reg(ir, inst, DpOp::Cmn),
        ADC_reg => thumb32_dp_reg(ir, inst, DpOp::Adc),
        SBC_reg => thumb32_dp_reg(ir, inst, DpOp::Sbc),
        SUB_reg => thumb32_dp_reg(ir, inst, DpOp::Sub),
        CMP_reg => thumb32_dp_reg(ir, inst, DpOp::Cmp),
        RSB_reg => thumb32_dp_reg(ir, inst, DpOp::Rsb),

        // Data processing (plain binary immediate)
        MOV_imm_wide => thumb32_movw(ir, inst),
        MOVT => thumb32_movt(ir, inst),
        ADD_imm_wide => thumb32_add_imm_wide(ir, inst),
        SUB_imm_wide => thumb32_sub_imm_wide(ir, inst),
        ADR_add | ADR_sub => thumb32_adr(ir, inst),
        BFC => thumb32_bfc(ir, inst),
        BFI => thumb32_bfi(ir, inst),
        SBFX => thumb32_sbfx(ir, inst),
        UBFX => thumb32_ubfx(ir, inst),
        SSAT | SSAT16 | USAT | USAT16 => true, // stub

        // Branch
        B_t3 => thumb32_b_cond(ir, inst),
        B_t4 => thumb32_b_uncond(ir, inst),
        BL => thumb32_bl(ir, inst),
        BLX_imm => thumb32_blx_imm(ir, inst),

        // Load/Store
        LDR_imm_t3 | LDR_imm_t4 | LDR_lit => thumb32_ldr(ir, inst),
        LDR_reg => thumb32_ldr_reg(ir, inst),
        STR_imm_t3 | STR_imm_t4 => thumb32_str(ir, inst),
        STR_reg => thumb32_str_reg(ir, inst),
        LDRB_imm_t2 | LDRB_imm_t3 | LDRB_lit => thumb32_ldrb(ir, inst),
        LDRB_reg => thumb32_ldrb_reg(ir, inst),
        STRB_imm_t2 | STRB_imm_t3 => thumb32_strb(ir, inst),
        STRB_reg => thumb32_strb_reg(ir, inst),
        LDRH_imm_t2 | LDRH_imm_t3 | LDRH_lit => thumb32_ldrh(ir, inst),
        LDRH_reg => thumb32_ldrh_reg(ir, inst),
        STRH_imm_t2 | STRH_imm_t3 => thumb32_strh(ir, inst),
        STRH_reg => thumb32_strh_reg(ir, inst),
        LDRSB_imm_t1 | LDRSB_imm_t2 | LDRSB_lit => thumb32_ldrsb(ir, inst),
        LDRSB_reg => thumb32_ldrsb_reg(ir, inst),
        LDRSH_imm_t1 | LDRSH_imm_t2 | LDRSH_lit => thumb32_ldrsh(ir, inst),
        LDRSH_reg => thumb32_ldrsh_reg(ir, inst),

        // Load/Store dual
        LDRD_imm | LDRD_lit => thumb32_ldrd(ir, inst),
        STRD_imm => thumb32_strd(ir, inst),

        // Load/Store multiple
        LDM => thumb32_ldm(ir, inst, false),
        LDMDB => thumb32_ldm(ir, inst, true),
        STM => thumb32_stm(ir, inst, false),
        STMDB => thumb32_stm(ir, inst, true),
        PUSH => thumb32_push(ir, inst),
        POP => thumb32_pop(ir, inst),

        // Exclusive
        LDREX => thumb32_ldrex(ir, inst),
        STREX => thumb32_strex(ir, inst),
        LDREXB | LDREXH | LDREXD => true, // stub
        STREXB | STREXH | STREXD => true, // stub
        CLREX => thumb32_control::thumb32_clrex(ir),

        // Multiply
        MUL => thumb32_mul(ir, inst),
        MLA => thumb32_mla(ir, inst),
        MLS => thumb32_mls(ir, inst),
        SMMUL => thumb32_smmul(ir, inst),
        SMMLA => thumb32_smmla(ir, inst),
        SMMLS => thumb32_smmls(ir, inst),
        SMULL => thumb32_smull(ir, inst),
        UMULL => thumb32_umull(ir, inst),
        SMLAL => thumb32_smlal(ir, inst),
        UMLAL => thumb32_umlal(ir, inst),
        SDIV => thumb32_sdiv(ir, inst),
        UDIV => thumb32_udiv(ir, inst),

        // Coprocessor
        MCRR => thumb32_coprocessor::thumb32_mcrr(ir, inst),
        MRRC => thumb32_coprocessor::thumb32_mrrc(ir, inst),
        STC => thumb32_coprocessor::thumb32_stc(ir, inst),
        LDC => thumb32_coprocessor::thumb32_ldc(ir, inst),
        CDP => thumb32_coprocessor::thumb32_cdp(ir, inst),
        MCR => thumb32_coprocessor::thumb32_mcr(ir, inst),
        MRC => thumb32_coprocessor::thumb32_mrc(ir, inst),

        // Misc
        CLZ => thumb32_clz(ir, inst),
        RBIT => thumb32_rbit(ir, inst),
        REV => thumb32_rev(ir, inst),
        REV16 => thumb32_rev16(ir, inst),
        REVSH => thumb32_revsh(ir, inst),
        SXTH | SXTB | UXTH | UXTB | SXTAH | SXTAB | UXTAH | UXTAB => thumb32_extension(ir, inst),

        // Barriers
        DMB => thumb32_control::thumb32_dmb(ir),
        DSB => thumb32_control::thumb32_dsb(ir),
        ISB => thumb32_control::thumb32_isb(ir),

        // System
        MRS => thumb32_control::thumb32_mrs_reg(ir, inst),
        MSR_reg => thumb32_control::thumb32_msr_reg(ir, inst),
        SVC => thumb32_svc(ir, inst),
        UDF | BKPT => thumb32_control::thumb32_udf(ir),
        NOP => thumb32_control::thumb32_nop(),
        SEV => thumb32_control::thumb32_sev(ir, options),
        SEVL => thumb32_control::thumb32_sevl(ir, options),
        WFE => thumb32_control::thumb32_wfe(ir, options),
        WFI => thumb32_control::thumb32_wfi(ir, options),
        YIELD => thumb32_control::thumb32_yield(ir, options),

        PLD_lit => thumb32_control::thumb32_pld(ir, false, options),
        PLD_imm8 | PLD_imm12 => {
            let write_intent = ((inst.raw >> 21) & 1) != 0;
            thumb32_control::thumb32_pld(ir, write_intent, options)
        }
        PLD_reg => {
            if inst.rm() == Reg::PC {
                super::unpredictable_instruction(ir)
            } else {
                let write_intent = ((inst.raw >> 21) & 1) != 0;
                thumb32_control::thumb32_pld(ir, write_intent, options)
            }
        }
        PLI_lit | PLI_imm8 | PLI_imm12 => thumb32_control::thumb32_pli(ir, options),
        PLI_reg => {
            if inst.rm() == Reg::PC {
                super::unpredictable_instruction(ir)
            } else {
                thumb32_control::thumb32_pli(ir, options)
            }
        }

        // Unmatched Thumb32 encoding → UndefinedInstruction with the full
        // RaiseException lifecycle (PC+4 for a 4-byte Thumb32 instruction),
        // matching upstream rather than the bare opcode + terminal.
        Unknown => super::undefined_instruction(ir),
    }
}

#[derive(Debug, Clone, Copy)]
enum DpOp {
    And,
    Eor,
    Sub,
    Rsb,
    Add,
    Adc,
    Sbc,
    Tst,
    Teq,
    Cmp,
    Cmn,
    Orr,
    Orn,
    Mov,
    Bic,
    Mvn,
    Pkh,
}

fn thumb32_dp_imm(ir: &mut A32IREmitter, inst: &DecodedThumb32, op: DpOp) -> bool {
    let rd = inst.rd();
    let rn = inst.rn();
    let s = inst.s_flag();
    let (imm_val, carry) = inst.thumb_expand_imm_c(false);
    let carry_value = ir.ir().imm1(carry);
    let operand2 = Value::ImmU32(imm_val);
    let operand1 = ir.get_register(rn);
    dp_common(ir, op, rd, s, operand1, operand2, carry_value)
}

fn thumb32_dp_reg(ir: &mut A32IREmitter, inst: &DecodedThumb32, op: DpOp) -> bool {
    let rd = inst.rd();
    let rn = inst.rn();
    let rm = inst.rm();
    let s = inst.s_flag();
    let (shift_type, imm5) = inst.shift_type_amount();
    let carry_in = ir.get_c_flag();
    let rm_val = ir.get_register(rm);
    let (shifted, carry) = emit_imm_shift(ir, rm_val, shift_type, imm5, carry_in);
    let operand1 = ir.get_register(rn);
    dp_common(ir, op, rd, s, operand1, shifted, carry)
}

fn dp_common(
    ir: &mut A32IREmitter,
    op: DpOp,
    rd: Reg,
    s: bool,
    operand1: Value,
    operand2: Value,
    carry: Value,
) -> bool {
    let (result, is_test) = match op {
        DpOp::And => (ir.ir().and_32(operand1, operand2), false),
        DpOp::Eor => (ir.ir().eor_32(operand1, operand2), false),
        DpOp::Sub => (
            ir.ir().sub_32(operand1, operand2, Value::ImmU1(true)),
            false,
        ),
        DpOp::Rsb => (
            ir.ir().sub_32(operand2, operand1, Value::ImmU1(true)),
            false,
        ),
        DpOp::Add => (
            ir.ir().add_32(operand1, operand2, Value::ImmU1(false)),
            false,
        ),
        DpOp::Adc => {
            let c = ir.get_c_flag();
            (ir.ir().add_32(operand1, operand2, c), false)
        }
        DpOp::Sbc => {
            let c = ir.get_c_flag();
            (ir.ir().sub_32(operand1, operand2, c), false)
        }
        DpOp::Tst => (ir.ir().and_32(operand1, operand2), true),
        DpOp::Teq => (ir.ir().eor_32(operand1, operand2), true),
        DpOp::Cmp => (ir.ir().sub_32(operand1, operand2, Value::ImmU1(true)), true),
        DpOp::Cmn => (
            ir.ir().add_32(operand1, operand2, Value::ImmU1(false)),
            true,
        ),
        DpOp::Orr => (ir.ir().or_32(operand1, operand2), false),
        DpOp::Orn => {
            let not_op2 = ir.ir().not_32(operand2);
            (ir.ir().or_32(operand1, not_op2), false)
        }
        DpOp::Mov => (operand2, false),
        DpOp::Bic => {
            let not_op2 = ir.ir().not_32(operand2);
            (ir.ir().and_32(operand1, not_op2), false)
        }
        DpOp::Mvn => (ir.ir().not_32(operand2), false),
        DpOp::Pkh => {
            // Top half from Rn, bottom half from shifted Rm
            let lo = ir.ir().and_32(operand2, Value::ImmU32(0xFFFF));
            let hi = ir.ir().and_32(operand1, Value::ImmU32(0xFFFF_0000));
            (ir.ir().or_32(hi, lo), false)
        }
    };

    if s || is_test {
        match op {
            DpOp::Add | DpOp::Adc | DpOp::Cmn | DpOp::Sub | DpOp::Sbc | DpOp::Cmp | DpOp::Rsb => {
                let nzcv = ir.ir().get_nzcv_from_op(result);
                ir.set_cpsr_nzcv(nzcv);
            }
            _ => {
                let nzcv = ir.ir().get_nzcv_from_op(result);
                ir.set_cpsr_nzc(nzcv, carry);
            }
        }
    }

    if !is_test {
        ir.set_register(rd, result);
    }
    true
}

// --- Plain binary immediate ---

fn thumb32_movw(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let imm16 = inst.imm16();
    ir.set_register(rd, Value::ImmU32(imm16));
    true
}

fn thumb32_movt(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let imm16 = inst.imm16();
    let rd_val = ir.get_register(rd);
    let masked = ir.ir().and_32(rd_val, Value::ImmU32(0x0000_FFFF));
    let upper = Value::ImmU32(imm16 << 16);
    let result = ir.ir().or_32(masked, upper);
    ir.set_register(rd, result);
    true
}

fn thumb32_add_imm_wide(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rn = inst.rn();
    let imm12 = inst.imm12();
    let rn_val = ir.get_register(rn);
    let result = ir
        .ir()
        .add_32(rn_val, Value::ImmU32(imm12), Value::ImmU1(false));
    ir.set_register(rd, result);
    true
}

fn thumb32_sub_imm_wide(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rn = inst.rn();
    let imm12 = inst.imm12();
    let rn_val = ir.get_register(rn);
    let result = ir
        .ir()
        .sub_32(rn_val, Value::ImmU32(imm12), Value::ImmU1(true));
    ir.set_register(rd, result);
    true
}

fn thumb32_adr(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let imm12 = inst.imm12();
    let loc = ir.current_location.expect("location not set");
    let base = (loc.pc().wrapping_add(4)) & !3;
    let result = if inst.id == Thumb32InstId::ADR_add {
        base.wrapping_add(imm12)
    } else {
        base.wrapping_sub(imm12)
    };
    ir.set_register(rd, Value::ImmU32(result));
    true
}

fn thumb32_bfc(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let (lsb, msb) = inst.bfc_lsb_msb();
    let width = msb - lsb + 1;
    let rd_val = ir.get_register(rd);
    let mask = !(((1u32 << width) - 1) << lsb);
    let result = ir.ir().and_32(rd_val, Value::ImmU32(mask));
    ir.set_register(rd, result);
    true
}

fn thumb32_bfi(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rn = inst.rn();
    let (lsb, msb) = inst.bfc_lsb_msb();
    let width = msb - lsb + 1;
    let rd_val = ir.get_register(rd);
    let rn_val = ir.get_register(rn);
    let field_mask = ((1u32 << width) - 1) << lsb;
    let src_mask = (1u32 << width) - 1;
    let src_bits = ir.ir().and_32(rn_val, Value::ImmU32(src_mask));
    let shifted = if lsb != 0 {
        ir.ir()
            .logical_shift_left_32(src_bits, Value::ImmU8(lsb as u8), Value::ImmU1(false))
    } else {
        src_bits
    };
    let cleared = ir.ir().and_32(rd_val, Value::ImmU32(!field_mask));
    let result = ir.ir().or_32(cleared, shifted);
    ir.set_register(rd, result);
    true
}

fn thumb32_sbfx(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rn = inst.rn();
    let (lsb, width) = inst.bfx_lsb_width();
    let rn_val = ir.get_register(rn);
    let shifted = if lsb != 0 {
        ir.ir()
            .logical_shift_right_32(rn_val, Value::ImmU8(lsb as u8), Value::ImmU1(false))
    } else {
        rn_val
    };
    let shift_amount = 32 - width;
    let shl = ir.ir().logical_shift_left_32(
        shifted,
        Value::ImmU8(shift_amount as u8),
        Value::ImmU1(false),
    );
    let result = ir.ir().arithmetic_shift_right_32(
        shl,
        Value::ImmU8(shift_amount as u8),
        Value::ImmU1(false),
    );
    ir.set_register(rd, result);
    true
}

fn thumb32_ubfx(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rn = inst.rn();
    let (lsb, width) = inst.bfx_lsb_width();
    let rn_val = ir.get_register(rn);
    let shifted = if lsb != 0 {
        ir.ir()
            .logical_shift_right_32(rn_val, Value::ImmU8(lsb as u8), Value::ImmU1(false))
    } else {
        rn_val
    };
    let mask = (1u32 << width) - 1;
    let result = ir.ir().and_32(shifted, Value::ImmU32(mask));
    ir.set_register(rd, result);
    true
}

// --- Branch ---

fn thumb32_b_cond(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let cond = inst.cond();
    let offset = inst.branch_offset_t3();
    let loc = ir.current_location.expect("location not set");
    let next = loc.advance_pc(4).advance_it();
    let target = loc.advance_pc(offset + 4).advance_it();
    ir.set_term(Terminal::if_then_else(
        cond,
        Terminal::link_block(target.to_location()),
        Terminal::link_block(next.to_location()),
    ));
    false
}

fn thumb32_b_uncond(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let offset = inst.branch_offset_t4();
    let loc = ir.current_location.expect("location not set");
    let target = loc.advance_pc(offset + 4).advance_it();
    ir.set_term(Terminal::link_block(target.to_location()));
    false
}

fn thumb32_bl(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let offset = inst.branch_offset_t4();
    let loc = ir.current_location.expect("location not set");
    let return_addr = loc.pc().wrapping_add(4) | 1; // Thumb bit
    ir.base
        .push_rsb(loc.advance_pc(4).advance_it().to_location());
    ir.set_register(Reg::R14, Value::ImmU32(return_addr));
    let target = loc.advance_pc(offset + 4).advance_it();
    ir.set_term(Terminal::link_block(target.to_location()));
    false
}

fn thumb32_blx_imm(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let offset = inst.branch_offset_t4();
    let loc = ir.current_location.expect("location not set");
    let return_addr = loc.pc().wrapping_add(4) | 1;
    ir.base
        .push_rsb(loc.advance_pc(4).advance_it().to_location());
    ir.set_register(Reg::R14, Value::ImmU32(return_addr));
    let target_pc = (ir.align_pc(4) as i32).wrapping_add(offset) as u32;
    let target = loc.set_pc(target_pc).set_t_flag(false).advance_it();
    ir.set_term(Terminal::link_block(target.to_location()));
    false
}

// --- Load/Store ---

fn thumb32_ldr(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let address = compute_thumb32_ls_address(ir, inst);
    let value = ir.read_memory_32(address, AccType::Normal);
    if rt == Reg::R15 {
        ir.update_upper_location_descriptor();
        ir.load_write_pc(value);
        ir.set_term(Terminal::FastDispatchHint);
        return false;
    }
    ir.set_register(rt, value);
    true
}

fn thumb32_ldr_reg(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let rm = inst.rm();
    let shift = ((inst.raw >> 4) & 3) as u32;
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let shifted = if shift != 0 {
        ir.ir()
            .logical_shift_left_32(rm_val, Value::ImmU8(shift as u8), Value::ImmU1(false))
    } else {
        rm_val
    };
    let address = ir.ir().add_32(rn_val, shifted, Value::ImmU1(false));
    let value = ir.read_memory_32(address, AccType::Normal);
    if rt == Reg::R15 {
        ir.update_upper_location_descriptor();
        ir.load_write_pc(value);
        ir.set_term(Terminal::FastDispatchHint);
        return false;
    }
    ir.set_register(rt, value);
    true
}

fn thumb32_str(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let address = compute_thumb32_ls_address(ir, inst);
    let value = ir.get_register(rt);
    ir.write_memory_32(address, value, AccType::Normal);
    true
}

fn thumb32_str_reg(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let rm = inst.rm();
    let shift = ((inst.raw >> 4) & 3) as u32;
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let shifted = if shift != 0 {
        ir.ir()
            .logical_shift_left_32(rm_val, Value::ImmU8(shift as u8), Value::ImmU1(false))
    } else {
        rm_val
    };
    let address = ir.ir().add_32(rn_val, shifted, Value::ImmU1(false));
    let value = ir.get_register(rt);
    ir.write_memory_32(address, value, AccType::Normal);
    true
}

fn thumb32_ldrb(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let address = compute_thumb32_ls_address(ir, inst);
    let value = ir.read_memory_8(address, AccType::Normal);
    let extended = ir.ir().zero_extend_byte_to_word(value);
    ir.set_register(rt, extended);
    true
}

fn thumb32_ldrb_reg(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let rm = inst.rm();
    let shift = ((inst.raw >> 4) & 3) as u32;
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let shifted = if shift != 0 {
        ir.ir()
            .logical_shift_left_32(rm_val, Value::ImmU8(shift as u8), Value::ImmU1(false))
    } else {
        rm_val
    };
    let address = ir.ir().add_32(rn_val, shifted, Value::ImmU1(false));
    let value = ir.read_memory_8(address, AccType::Normal);
    let extended = ir.ir().zero_extend_byte_to_word(value);
    ir.set_register(rt, extended);
    true
}

fn thumb32_strb(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let address = compute_thumb32_ls_address(ir, inst);
    let value = ir.get_register(rt);
    let byte = ir.ir().least_significant_byte(value);
    ir.write_memory_8(address, byte, AccType::Normal);
    true
}

fn thumb32_strb_reg(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let rm = inst.rm();
    let shift = ((inst.raw >> 4) & 3) as u32;
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let shifted = if shift != 0 {
        ir.ir()
            .logical_shift_left_32(rm_val, Value::ImmU8(shift as u8), Value::ImmU1(false))
    } else {
        rm_val
    };
    let address = ir.ir().add_32(rn_val, shifted, Value::ImmU1(false));
    let value = ir.get_register(rt);
    let byte = ir.ir().least_significant_byte(value);
    ir.write_memory_8(address, byte, AccType::Normal);
    true
}

fn thumb32_ldrh(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let address = compute_thumb32_ls_address(ir, inst);
    let value = ir.read_memory_16(address, AccType::Normal);
    let extended = ir.ir().zero_extend_half_to_word(value);
    ir.set_register(rt, extended);
    true
}

fn thumb32_ldrh_reg(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let rm = inst.rm();
    let shift = ((inst.raw >> 4) & 3) as u32;
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let shifted = if shift != 0 {
        ir.ir()
            .logical_shift_left_32(rm_val, Value::ImmU8(shift as u8), Value::ImmU1(false))
    } else {
        rm_val
    };
    let address = ir.ir().add_32(rn_val, shifted, Value::ImmU1(false));
    let value = ir.read_memory_16(address, AccType::Normal);
    let extended = ir.ir().zero_extend_half_to_word(value);
    ir.set_register(rt, extended);
    true
}

fn thumb32_strh(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let address = compute_thumb32_ls_address(ir, inst);
    let value = ir.get_register(rt);
    let half = ir.ir().least_significant_half(value);
    ir.write_memory_16(address, half, AccType::Normal);
    true
}

fn thumb32_strh_reg(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let rm = inst.rm();
    let shift = ((inst.raw >> 4) & 3) as u32;
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let shifted = if shift != 0 {
        ir.ir()
            .logical_shift_left_32(rm_val, Value::ImmU8(shift as u8), Value::ImmU1(false))
    } else {
        rm_val
    };
    let address = ir.ir().add_32(rn_val, shifted, Value::ImmU1(false));
    let value = ir.get_register(rt);
    let half = ir.ir().least_significant_half(value);
    ir.write_memory_16(address, half, AccType::Normal);
    true
}

fn thumb32_ldrsb(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let address = compute_thumb32_ls_address(ir, inst);
    let value = ir.read_memory_8(address, AccType::Normal);
    let extended = ir.ir().sign_extend_byte_to_word(value);
    ir.set_register(rt, extended);
    true
}

fn thumb32_ldrsb_reg(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let rm = inst.rm();
    let shift = ((inst.raw >> 4) & 3) as u32;
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let shifted = if shift != 0 {
        ir.ir()
            .logical_shift_left_32(rm_val, Value::ImmU8(shift as u8), Value::ImmU1(false))
    } else {
        rm_val
    };
    let address = ir.ir().add_32(rn_val, shifted, Value::ImmU1(false));
    let value = ir.read_memory_8(address, AccType::Normal);
    let extended = ir.ir().sign_extend_byte_to_word(value);
    ir.set_register(rt, extended);
    true
}

fn thumb32_ldrsh(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let address = compute_thumb32_ls_address(ir, inst);
    let value = ir.read_memory_16(address, AccType::Normal);
    let extended = ir.ir().sign_extend_half_to_word(value);
    ir.set_register(rt, extended);
    true
}

fn thumb32_ldrsh_reg(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let rm = inst.rm();
    let shift = ((inst.raw >> 4) & 3) as u32;
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let shifted = if shift != 0 {
        ir.ir()
            .logical_shift_left_32(rm_val, Value::ImmU8(shift as u8), Value::ImmU1(false))
    } else {
        rm_val
    };
    let address = ir.ir().add_32(rn_val, shifted, Value::ImmU1(false));
    let value = ir.read_memory_16(address, AccType::Normal);
    let extended = ir.ir().sign_extend_half_to_word(value);
    ir.set_register(rt, extended);
    true
}

fn compute_thumb32_ls_address(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> Value {
    let rn = inst.rn();

    // Decide T3/T2/T1 (unsigned 12-bit positive offset in hw2[11:0]) vs
    // T4/T3 (8-bit offset in hw2[7:0] with P/U/W at hw2[10:8]) from the
    // decoded instruction id. `inst.imm12()` is the scattered
    // `i:imm3:imm8` modified-immediate field used by data-processing
    // wide-immediate forms, so it MUST NOT be used for plain load/store
    // offsets — its `imm3` bits overlap Rt at hw2[14:12] and silently
    // corrupt the address with the destination register encoding.
    let is_imm12 = matches!(
        inst.id,
        Thumb32InstId::LDR_imm_t3
            | Thumb32InstId::LDR_lit
            | Thumb32InstId::STR_imm_t3
            | Thumb32InstId::LDRB_imm_t2
            | Thumb32InstId::LDRB_lit
            | Thumb32InstId::STRB_imm_t2
            | Thumb32InstId::LDRH_imm_t2
            | Thumb32InstId::LDRH_lit
            | Thumb32InstId::STRH_imm_t2
            | Thumb32InstId::LDRSB_imm_t1
            | Thumb32InstId::LDRSB_lit
            | Thumb32InstId::LDRSH_imm_t1
            | Thumb32InstId::LDRSH_lit
    );

    if rn == Reg::R15 {
        // PC-relative literal. imm12 is hw2[11:0] and U is at raw bit 23
        // (hw1 bit 7) per the thumb32_*_lit decode patterns.
        let loc = ir.current_location.expect("location not set");
        let base = (loc.pc().wrapping_add(4)) & !3;
        let imm12 = inst.raw & 0xFFF;
        let u = inst.raw & (1 << 23) != 0;
        return if u {
            Value::ImmU32(base.wrapping_add(imm12))
        } else {
            Value::ImmU32(base.wrapping_sub(imm12))
        };
    }

    let rn_val = ir.get_register(rn);
    if is_imm12 {
        let imm12 = inst.raw & 0xFFF;
        ir.ir()
            .add_32(rn_val, Value::ImmU32(imm12), Value::ImmU1(false))
    } else {
        // 8-bit offset with P/U/W (T4 / T3-of-LDRB/LDRH / T2-of-LDRSB/LDRSH).
        let imm8 = inst.imm8();
        let p = inst.p_flag();
        let u = inst.u_flag();
        let w = inst.w_flag();
        let offset = Value::ImmU32(imm8);
        get_address(ir, p, u, w, rn, offset)
    }
}

// --- Load/Store dual ---

fn thumb32_ldrd(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let rt2 = inst.rt2();
    let rn = inst.rn();
    let imm8 = inst.imm8() << 2;
    let u = (inst.raw >> 23) & 1 != 0;
    let p = (inst.raw >> 24) & 1 != 0;
    let w = (inst.raw >> 21) & 1 != 0;

    let base = if rn == Reg::R15 {
        let loc = ir.current_location.expect("location not set");
        Value::ImmU32((loc.pc().wrapping_add(4)) & !3)
    } else {
        ir.get_register(rn)
    };

    let offset = Value::ImmU32(imm8);
    let offset_addr = if u {
        ir.ir().add_32(base, offset, Value::ImmU1(false))
    } else {
        ir.ir().sub_32(base, offset, Value::ImmU1(true))
    };

    let address = if p { offset_addr } else { base };

    // Match upstream `LoadDualImmediate` (thumb32_load_store_dual.cpp): single
    // 64-bit ATOMIC read, then split into the two destination registers with
    // most/least significant word ordering driven by CPSR.E. Two separate
    // 32-bit Normal reads break atomicity and the backend pattern matchers
    // that fold halves back into 64-bit ops.
    let data = ir.read_memory_64(address, AccType::Atomic);
    let e_flag = ir
        .current_location
        .expect("current_location not set")
        .e_flag();
    let lo_word = ir.ir().least_significant_word(data);
    let hi_word = ir.ir().most_significant_word(data);
    if e_flag {
        ir.set_register(rt, hi_word);
        ir.set_register(rt2, lo_word);
    } else {
        ir.set_register(rt, lo_word);
        ir.set_register(rt2, hi_word);
    }

    if w && rn != Reg::R15 {
        ir.set_register(rn, offset_addr);
    }
    true
}

fn thumb32_strd(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let rt2 = inst.rt2();
    let rn = inst.rn();
    let imm8 = inst.imm8() << 2;
    let u = (inst.raw >> 23) & 1 != 0;
    let p = (inst.raw >> 24) & 1 != 0;
    let w = (inst.raw >> 21) & 1 != 0;

    let base = ir.get_register(rn);
    let offset = Value::ImmU32(imm8);
    let offset_addr = if u {
        ir.ir().add_32(base, offset, Value::ImmU1(false))
    } else {
        ir.ir().sub_32(base, offset, Value::ImmU1(true))
    };

    let address = if p { offset_addr } else { base };

    // Match upstream `StoreDual`: pack the two source registers into a single
    // 64-bit value (low/high order driven by CPSR.E) and emit one 64-bit
    // ATOMIC write. Two separate 32-bit Normal writes break atomicity and
    // prevent the backend from recognising 64-bit atomic accesses.
    let value_a = ir.get_register(rt);
    let value_b = ir.get_register(rt2);
    let e_flag = ir
        .current_location
        .expect("current_location not set")
        .e_flag();
    let data = if e_flag {
        ir.ir().pack_2x32_to_1x64(value_b, value_a)
    } else {
        ir.ir().pack_2x32_to_1x64(value_a, value_b)
    };
    ir.write_memory_64(address, data, AccType::Atomic);

    if w {
        ir.set_register(rn, offset_addr);
    }
    true
}

// --- Load/Store multiple ---

fn thumb32_ldm(ir: &mut A32IREmitter, inst: &DecodedThumb32, decrement: bool) -> bool {
    let rn = inst.rn();
    let reglist = inst.register_list();
    let w = (inst.raw >> 21) & 1 != 0;
    let reg_count = reglist.count_ones() as u32;
    let base = ir.get_register(rn);
    let start = if decrement {
        ir.ir()
            .sub_32(base, Value::ImmU32(reg_count * 4), Value::ImmU1(true))
    } else {
        base
    };
    let writeback = if decrement {
        start
    } else {
        ir.ir()
            .add_32(start, Value::ImmU32(reg_count * 4), Value::ImmU1(false))
    };
    let mut addr = start;
    for i in 0..15u32 {
        if reglist & (1 << i) != 0 {
            let val = ir.read_memory_32(addr, AccType::Atomic);
            ir.set_register(Reg::from_u32(i), val);
            addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
        }
    }
    if w && (reglist & (1 << (rn as u32))) == 0 {
        ir.set_register(rn, writeback);
    }
    if reglist & (1 << 15) != 0 {
        ir.update_upper_location_descriptor();
        let pc = ir.read_memory_32(addr, AccType::Atomic);
        ir.load_write_pc(pc);
        if rn == Reg::R13 {
            ir.set_term(Terminal::PopRSBHint);
        } else {
            ir.set_term(Terminal::FastDispatchHint);
        }
        return false;
    }
    true
}

fn thumb32_stm(ir: &mut A32IREmitter, inst: &DecodedThumb32, decrement: bool) -> bool {
    let rn = inst.rn();
    let reglist = inst.register_list() & 0x7fff;
    let w = (inst.raw >> 21) & 1 != 0;
    let reg_count = reglist.count_ones() as u32;

    if rn == Reg::R15 || reg_count < 2 {
        return false;
    }
    if w && (reglist & (1 << (rn as u32))) != 0 {
        return false;
    }
    if (reglist & (1 << 13)) != 0 {
        return false;
    }

    let base = ir.get_register(rn);
    let start = if decrement {
        ir.ir()
            .sub_32(base, Value::ImmU32(reg_count * 4), Value::ImmU1(true))
    } else {
        base
    };
    let mut addr = start;
    for i in 0..15u32 {
        if reglist & (1 << i) != 0 {
            let val = ir.get_register(Reg::from_u32(i));
            ir.write_memory_32(addr, val, AccType::Atomic);
            addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
        }
    }
    if w {
        let wb = if decrement {
            ir.ir()
                .sub_32(base, Value::ImmU32(reg_count * 4), Value::ImmU1(true))
        } else {
            ir.ir()
                .add_32(base, Value::ImmU32(reg_count * 4), Value::ImmU1(false))
        };
        ir.set_register(rn, wb);
    }
    true
}

fn thumb32_push(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    thumb32_stm(ir, inst, true)
}

fn thumb32_pop(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    thumb32_ldm(ir, inst, false)
}

// --- Exclusive ---

fn thumb32_ldrex(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rt = inst.rt();
    let rn = inst.rn();
    let imm8 = inst.imm8() << 2;
    let rn_val = ir.get_register(rn);
    let address = ir
        .ir()
        .add_32(rn_val, Value::ImmU32(imm8), Value::ImmU1(false));
    let value = ir.exclusive_read_memory_32(address, AccType::Ordered);
    ir.set_register(rt, value);
    true
}

fn thumb32_strex(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rt = inst.rt();
    let rn = inst.rn();
    let imm8 = inst.imm8() << 2;
    let rn_val = ir.get_register(rn);
    let address = ir
        .ir()
        .add_32(rn_val, Value::ImmU32(imm8), Value::ImmU1(false));
    let value = ir.get_register(rt);
    let result = ir.exclusive_write_memory_32(address, value, AccType::Ordered);
    ir.set_register(rd, result);
    true
}

// --- Multiply ---

fn thumb32_mul(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rn = inst.rn();
    let rm = inst.rm();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let result = ir.ir().mul_32(rn_val, rm_val);
    ir.set_register(rd, result);
    true
}

fn thumb32_mla(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rn = inst.rn();
    let rm = inst.rm();
    let ra = inst.ra();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let ra_val = ir.get_register(ra);
    let product = ir.ir().mul_32(rn_val, rm_val);
    let result = ir.ir().add_32(product, ra_val, Value::ImmU1(false));
    ir.set_register(rd, result);
    true
}

fn thumb32_mls(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rn = inst.rn();
    let rm = inst.rm();
    let ra = inst.ra();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let ra_val = ir.get_register(ra);
    let product = ir.ir().mul_32(rn_val, rm_val);
    let result = ir.ir().sub_32(ra_val, product, Value::ImmU1(true));
    ir.set_register(rd, result);
    true
}

fn thumb32_smmul(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rn = inst.rn();
    let rm = inst.rm();
    let round = (inst.raw >> 4) & 1 != 0;
    if rd == Reg::R15 || rn == Reg::R15 || rm == Reg::R15 {
        return thumb32_unpredictable(ir);
    }
    let rn32 = ir.get_register(rn);
    let rm32 = ir.get_register(rm);
    let rn64 = ir.ir().sign_extend_word_to_long(rn32);
    let rm64 = ir.ir().sign_extend_word_to_long(rm32);
    let product = ir.ir().mul_64(rn64, rm64);
    let mut result = ir.ir().most_significant_word(product);
    if round {
        let carry = ir.ir().get_carry_from_op(result);
        result = ir.ir().add_32(result, Value::ImmU32(0), carry);
    }
    ir.set_register(rd, result);
    true
}

fn thumb32_smmla(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rn = inst.rn();
    let rm = inst.rm();
    let ra = inst.ra();
    let round = (inst.raw >> 4) & 1 != 0;
    if rd == Reg::R15 || rn == Reg::R15 || rm == Reg::R15 || ra == Reg::R15 {
        return thumb32_unpredictable(ir);
    }
    let rn32 = ir.get_register(rn);
    let rm32 = ir.get_register(rm);
    let ra32 = ir.get_register(ra);
    let rn64 = ir.ir().sign_extend_word_to_long(rn32);
    let rm64 = ir.ir().sign_extend_word_to_long(rm32);
    let ra64 = ir.ir().pack_2x32_to_1x64(Value::ImmU32(0), ra32);
    let product = ir.ir().mul_64(rn64, rm64);
    let temp = ir.ir().add_64(ra64, product, Value::ImmU1(false));
    let mut result = ir.ir().most_significant_word(temp);
    if round {
        let carry = ir.ir().get_carry_from_op(result);
        result = ir.ir().add_32(result, Value::ImmU32(0), carry);
    }
    ir.set_register(rd, result);
    true
}

fn thumb32_smmls(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rn = inst.rn();
    let rm = inst.rm();
    let ra = inst.ra();
    let round = (inst.raw >> 4) & 1 != 0;
    if rd == Reg::R15 || rn == Reg::R15 || rm == Reg::R15 || ra == Reg::R15 {
        return thumb32_unpredictable(ir);
    }
    let rn32 = ir.get_register(rn);
    let rm32 = ir.get_register(rm);
    let ra32 = ir.get_register(ra);
    let rn64 = ir.ir().sign_extend_word_to_long(rn32);
    let rm64 = ir.ir().sign_extend_word_to_long(rm32);
    let ra64 = ir.ir().pack_2x32_to_1x64(Value::ImmU32(0), ra32);
    let product = ir.ir().mul_64(rn64, rm64);
    let temp = ir.ir().sub_64(ra64, product, Value::ImmU1(true));
    let mut result = ir.ir().most_significant_word(temp);
    if round {
        let carry = ir.ir().get_carry_from_op(result);
        result = ir.ir().add_32(result, Value::ImmU32(0), carry);
    }
    ir.set_register(rd, result);
    true
}

fn thumb32_smull(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd_lo = inst.rd_lo();
    let rd_hi = inst.rd_hi();
    let rn = inst.rn();
    let rm = inst.rm();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let rn64 = ir.ir().sign_extend_word_to_long(rn_val);
    let rm64 = ir.ir().sign_extend_word_to_long(rm_val);
    let result = ir.ir().mul_64(rn64, rm64);
    let lo = ir.ir().least_significant_word(result);
    let hi = ir.ir().most_significant_word(result);
    ir.set_register(rd_lo, lo);
    ir.set_register(rd_hi, hi);
    true
}

fn thumb32_umull(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd_lo = inst.rd_lo();
    let rd_hi = inst.rd_hi();
    let rn = inst.rn();
    let rm = inst.rm();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let rn64 = ir.ir().zero_extend_word_to_long(rn_val);
    let rm64 = ir.ir().zero_extend_word_to_long(rm_val);
    let result = ir.ir().mul_64(rn64, rm64);
    let lo = ir.ir().least_significant_word(result);
    let hi = ir.ir().most_significant_word(result);
    ir.set_register(rd_lo, lo);
    ir.set_register(rd_hi, hi);
    true
}

fn thumb32_smlal(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd_lo = inst.rd_lo();
    let rd_hi = inst.rd_hi();
    let rn = inst.rn();
    let rm = inst.rm();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let rdhi_val = ir.get_register(rd_hi);
    let rdlo_val = ir.get_register(rd_lo);
    let rn64 = ir.ir().sign_extend_word_to_long(rn_val);
    let rm64 = ir.ir().sign_extend_word_to_long(rm_val);
    let product = ir.ir().mul_64(rn64, rm64);
    let accum = ir.ir().pack_2x32_to_1x64(rdlo_val, rdhi_val);
    let result = ir.ir().add_64(product, accum, Value::ImmU1(false));
    let lo = ir.ir().least_significant_word(result);
    let hi = ir.ir().most_significant_word(result);
    ir.set_register(rd_lo, lo);
    ir.set_register(rd_hi, hi);
    true
}

fn thumb32_umlal(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd_lo = inst.rd_lo();
    let rd_hi = inst.rd_hi();
    let rn = inst.rn();
    let rm = inst.rm();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let rdhi_val = ir.get_register(rd_hi);
    let rdlo_val = ir.get_register(rd_lo);
    let rn64 = ir.ir().zero_extend_word_to_long(rn_val);
    let rm64 = ir.ir().zero_extend_word_to_long(rm_val);
    let product = ir.ir().mul_64(rn64, rm64);
    let accum = ir.ir().pack_2x32_to_1x64(rdlo_val, rdhi_val);
    let result = ir.ir().add_64(product, accum, Value::ImmU1(false));
    let lo = ir.ir().least_significant_word(result);
    let hi = ir.ir().most_significant_word(result);
    ir.set_register(rd_lo, lo);
    ir.set_register(rd_hi, hi);
    true
}

fn thumb32_sdiv(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rn = inst.rn();
    let rm = inst.rm();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let result = ir.ir().signed_div_32(rn_val, rm_val);
    ir.set_register(rd, result);
    true
}

fn thumb32_udiv(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rn = inst.rn();
    let rm = inst.rm();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let result = ir.ir().unsigned_div_32(rn_val, rm_val);
    ir.set_register(rd, result);
    true
}

// --- Misc ---

fn thumb32_clz(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rm = inst.rm();
    let rm_val = ir.get_register(rm);
    let result = ir.ir().count_leading_zeros_32(rm_val);
    ir.set_register(rd, result);
    true
}

fn thumb32_rbit(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    // Use same pattern as ARM RBIT
    let rd = inst.rd();
    let rm = inst.rm();
    let rm_val = ir.get_register(rm);
    // Simplified: just reverse bytes for now, proper impl would need bit reversal
    let result = ir.ir().byte_reverse_word(rm_val);
    ir.set_register(rd, result);
    true
}

fn thumb32_rev(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rm = inst.rm();
    let rm_val = ir.get_register(rm);
    let result = ir.ir().byte_reverse_word(rm_val);
    ir.set_register(rd, result);
    true
}

fn thumb32_rev16(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rm = inst.rm();
    let rm_val = ir.get_register(rm);
    let byte0 = ir.ir().and_32(rm_val, Value::ImmU32(0xFF));
    let byte1 = ir.ir().and_32(rm_val, Value::ImmU32(0xFF00));
    let byte2 = ir.ir().and_32(rm_val, Value::ImmU32(0xFF_0000));
    let byte3 = ir.ir().and_32(rm_val, Value::ImmU32(0xFF00_0000));
    let s0 = ir
        .ir()
        .logical_shift_left_32(byte0, Value::ImmU8(8), Value::ImmU1(false));
    let s1 = ir
        .ir()
        .logical_shift_right_32(byte1, Value::ImmU8(8), Value::ImmU1(false));
    let s2 = ir
        .ir()
        .logical_shift_left_32(byte2, Value::ImmU8(8), Value::ImmU1(false));
    let s3 = ir
        .ir()
        .logical_shift_right_32(byte3, Value::ImmU8(8), Value::ImmU1(false));
    let lo = ir.ir().or_32(s0, s1);
    let hi = ir.ir().or_32(s2, s3);
    let result = ir.ir().or_32(lo, hi);
    ir.set_register(rd, result);
    true
}

fn thumb32_revsh(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rm = inst.rm();
    let rm_val = ir.get_register(rm);
    let byte0 = ir.ir().and_32(rm_val, Value::ImmU32(0xFF));
    let byte1 = ir.ir().and_32(rm_val, Value::ImmU32(0xFF00));
    let s0 = ir
        .ir()
        .logical_shift_left_32(byte0, Value::ImmU8(8), Value::ImmU1(false));
    let s1 = ir
        .ir()
        .logical_shift_right_32(byte1, Value::ImmU8(8), Value::ImmU1(false));
    let half = ir.ir().or_32(s0, s1);
    let result = ir.ir().sign_extend_half_to_word(half);
    ir.set_register(rd, result);
    true
}

fn thumb32_extension(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let rd = inst.rd();
    let rm = inst.rm();
    let rn = inst.rn();
    let rm_val = ir.get_register(rm);
    let rotation = ((inst.raw >> 4) & 3) * 8;
    let rotated = if rotation != 0 {
        ir.ir()
            .rotate_right_32(rm_val, Value::ImmU8(rotation as u8), Value::ImmU1(false))
    } else {
        rm_val
    };

    let extended = match inst.id {
        Thumb32InstId::SXTB | Thumb32InstId::SXTAB => ir.ir().sign_extend_byte_to_word(rotated),
        Thumb32InstId::SXTH | Thumb32InstId::SXTAH => ir.ir().sign_extend_half_to_word(rotated),
        Thumb32InstId::UXTB | Thumb32InstId::UXTAB => ir.ir().and_32(rotated, Value::ImmU32(0xFF)),
        Thumb32InstId::UXTH | Thumb32InstId::UXTAH => {
            ir.ir().and_32(rotated, Value::ImmU32(0xFFFF))
        }
        _ => rotated,
    };

    let result = match inst.id {
        Thumb32InstId::SXTAB
        | Thumb32InstId::SXTAH
        | Thumb32InstId::UXTAB
        | Thumb32InstId::UXTAH => {
            let rn_val = ir.get_register(rn);
            ir.ir().add_32(rn_val, extended, Value::ImmU1(false))
        }
        _ => extended,
    };

    ir.set_register(rd, result);
    true
}

fn thumb32_svc(ir: &mut A32IREmitter, inst: &DecodedThumb32) -> bool {
    let imm8 = inst.imm8();
    let loc = ir.current_location.expect("location not set");
    let next_loc = loc.advance_pc(4).advance_it();

    ir.base.push_rsb(next_loc.to_location());
    ir.update_upper_location_descriptor();
    ir.branch_write_pc(Value::ImmU32(next_loc.pc()));
    ir.call_supervisor(imm8);
    ir.set_term(Terminal::CheckHalt {
        else_: Box::new(Terminal::PopRSBHint),
    });
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder_thumb32::decode_thumb32;
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::frontend::a32::types::Reg;
    use crate::ir::block::Block;
    use crate::ir::location::{A32LocationDescriptor, LocationDescriptor};
    use crate::ir::opcode::Opcode;

    fn translated_exception(raw: u32, options: TranslationOptions) -> Option<u64> {
        let loc = A32LocationDescriptor::at(0x1000).set_t_flag(true);
        let decoded = decode_thumb32((raw >> 16) as u16, raw as u16);
        let mut block = Block::new(loc.to_location());
        {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            translate_thumb32(&mut ir, &decoded, options);
        }
        block
            .instructions
            .iter()
            .find(|instruction| instruction.opcode == Opcode::A32ExceptionRaised)
            .and_then(|instruction| match instruction.args[1] {
                Value::ImmU64(value) => Some(value),
                _ => None,
            })
    }

    #[test]
    fn thumb32_unknown_raises_undefined_with_full_lifecycle() {
        use crate::ir::terminal::Terminal;
        use crate::ir::value::Value;
        let mut block = Block::new(LocationDescriptor::new(0));
        let loc = A32LocationDescriptor::at(0x1000).set_t_flag(true);
        let cont = {
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            translate_thumb32(
                &mut ir,
                &DecodedThumb32 {
                    raw: 0,
                    id: Thumb32InstId::Unknown,
                },
                TranslationOptions::default(),
            )
        };
        assert!(!cont);
        let ops: Vec<_> = block.instructions.iter().map(|i| i.opcode).collect();
        assert!(ops.contains(&Opcode::A32UpdateUpperLocationDescriptor));
        assert!(ops.contains(&Opcode::A32SetRegister)); // BranchWritePC(PC+4)
        assert_eq!(ops.last(), Some(&Opcode::A32ExceptionRaised));
        let exc = block
            .instructions
            .iter()
            .rev()
            .find(|i| i.opcode == Opcode::A32ExceptionRaised)
            .expect("exception present");
        match exc.args[1] {
            Value::ImmU64(code) => assert_eq!(
                code,
                crate::frontend::a32::types::Exception::UndefinedInstruction.as_u32() as u64
            ),
            ref other => panic!("exception code not immediate: {other:?}"),
        }
        assert!(matches!(
            block.terminal,
            Terminal::CheckHalt { ref else_ }
                if matches!(else_.as_ref(), Terminal::ReturnToDispatch)
        ));
    }

    #[test]
    fn thumb32_hint_families_raise_upstream_exceptions() {
        use crate::frontend::a32::types::Exception;

        for (raw, expected) in [
            (0xF3AF_8005, Exception::SendEventLocal),
            (0xF81F_F123, Exception::PreloadData),
            (0xF835_FC12, Exception::PreloadDataWithIntentToWrite),
            (0xF895_F123, Exception::PreloadData),
            (0xF91F_F123, Exception::PreloadInstruction),
            (0xF915_FC12, Exception::PreloadInstruction),
        ] {
            assert_eq!(
                translated_exception(raw, TranslationOptions::default()),
                Some(expected.as_u32() as u64),
                "raw={raw:08X}"
            );
            assert_eq!(
                translated_exception(
                    raw,
                    TranslationOptions {
                        hook_hint_instructions: false,
                        ..TranslationOptions::default()
                    }
                ),
                None,
                "disabled raw={raw:08X}"
            );
        }
    }

    #[test]
    fn thumb32_preload_register_pc_is_unpredictable_before_hook_option() {
        use crate::frontend::a32::types::Exception;

        for raw in [0xF815_F00F, 0xF915_F00F] {
            assert_eq!(
                translated_exception(
                    raw,
                    TranslationOptions {
                        hook_hint_instructions: false,
                        ..TranslationOptions::default()
                    }
                ),
                Some(Exception::UnpredictableInstruction.as_u32() as u64),
                "raw={raw:08X}"
            );
        }
    }

    #[test]
    fn thumb32_blx_imm_uses_aligned_thumb_pc_base() {
        let mut block = Block::new(LocationDescriptor::new(0));
        let loc = A32LocationDescriptor::at(0x1002).set_t_flag(true);
        let mut ir = A32IREmitter::with_location(&mut block, loc);
        let inst = decode_thumb32(0xF000, 0xC000);
        assert_eq!(inst.id, Thumb32InstId::BLX_imm);
        let expected_pc = (ir.align_pc(4) as i32).wrapping_add(inst.branch_offset_t4()) as u32;
        let old_buggy_pc = ((loc.pc() as i32)
            .wrapping_add(4)
            .wrapping_add(inst.branch_offset_t4()) as u32)
            & !1;

        assert!(!thumb32_blx_imm(&mut ir, &inst));

        let Terminal::LinkBlock { next } = &block.terminal else {
            panic!("expected LinkBlock terminal, got {}", block.terminal);
        };
        let next = A32LocationDescriptor::from_location(*next);
        assert_eq!(next.pc(), expected_pc);
        assert_ne!(next.pc(), old_buggy_pc);
        assert!(!next.t_flag());
    }

    #[test]
    fn thumb32_svc_advances_pc_before_supervisor_call() {
        let mut psr = PSR::default();
        psr.set_t(true);
        let loc = A32LocationDescriptor::new(0x3000, psr, FPSCR::default(), false);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);
        let inst = DecodedThumb32 {
            raw: 0xF7F0_A123,
            id: Thumb32InstId::SVC,
        };

        assert!(!thumb32_svc(&mut ir, &inst));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::PushRSB));
        assert_eq!(
            block
                .instructions
                .iter()
                .find(|inst| inst.opcode == Opcode::A32SetRegister)
                .map(|inst| inst.args[0]),
            Some(Value::ImmA32Reg(Reg::R15))
        );
        assert!(matches!(
            &block.terminal,
            Terminal::CheckHalt { else_ } if matches!(else_.as_ref(), Terminal::PopRSBHint)
        ));
    }

    #[test]
    fn thumb32_strd_emits_single_atomic_write_memory_64() {
        let loc = A32LocationDescriptor::new(0x2000, PSR::default(), FPSCR::default(), false);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);
        let inst = DecodedThumb32 {
            raw: 0xE942_1304,
            id: Thumb32InstId::STRD_imm,
        };

        assert!(thumb32_strd(&mut ir, &inst));
        assert_eq!(
            block
                .instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::A32WriteMemory64)
                .count(),
            1
        );
        assert_eq!(
            block
                .instructions
                .iter()
                .filter(|inst| inst.opcode == Opcode::A32WriteMemory32)
                .count(),
            0
        );
    }
}
