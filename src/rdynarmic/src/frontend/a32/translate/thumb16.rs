use super::helpers::emit_imm_shift;
use crate::frontend::a32::decoder_thumb16::{DecodedThumb16, Thumb16InstId};
use crate::frontend::a32::types::{Exception, Reg, ShiftType};
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::acc_type::AccType;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

use super::TranslationOptions;

/// Translate a single Thumb16 instruction. Returns true to continue.
pub fn translate_thumb16(
    ir: &mut A32IREmitter,
    inst: &DecodedThumb16,
    options: TranslationOptions,
) -> bool {
    use Thumb16InstId::*;
    match inst.id {
        // Shift immediate
        LSL_imm => thumb16_lsl_imm(ir, inst),
        LSR_imm => thumb16_lsr_imm(ir, inst),
        ASR_imm => thumb16_asr_imm(ir, inst),

        // Add/Sub
        ADD_reg_t1 => thumb16_add_reg(ir, inst),
        SUB_reg => thumb16_sub_reg(ir, inst),
        ADD_imm_t1 => thumb16_add_imm3(ir, inst),
        SUB_imm_t1 => thumb16_sub_imm3(ir, inst),
        MOV_imm => thumb16_mov_imm(ir, inst),
        CMP_imm => thumb16_cmp_imm(ir, inst),
        ADD_imm_t2 => thumb16_add_imm8(ir, inst),
        SUB_imm_t2 => thumb16_sub_imm8(ir, inst),

        // Data processing
        AND_reg => thumb16_and_reg(ir, inst),
        EOR_reg => thumb16_eor_reg(ir, inst),
        LSL_reg => thumb16_lsl_reg(ir, inst),
        LSR_reg => thumb16_lsr_reg(ir, inst),
        ASR_reg => thumb16_asr_reg(ir, inst),
        ADC_reg => thumb16_adc_reg(ir, inst),
        SBC_reg => thumb16_sbc_reg(ir, inst),
        ROR_reg => thumb16_ror_reg(ir, inst),
        TST_reg => thumb16_tst_reg(ir, inst),
        RSB_imm => thumb16_rsb_imm(ir, inst),
        CMP_reg_t1 => thumb16_cmp_reg(ir, inst),
        CMN_reg => thumb16_cmn_reg(ir, inst),
        ORR_reg => thumb16_orr_reg(ir, inst),
        MUL_reg => thumb16_mul_reg(ir, inst),
        BIC_reg => thumb16_bic_reg(ir, inst),
        MVN_reg => thumb16_mvn_reg(ir, inst),

        // Special data
        ADD_reg_t2 => thumb16_add_reg_t2(ir, inst),
        CMP_reg_t2 => thumb16_cmp_reg_t2(ir, inst),
        MOV_reg => thumb16_mov_reg(ir, inst),

        // Branch
        BX => thumb16_bx(ir, inst),
        BLX_reg => thumb16_blx_reg(ir, inst),
        B_t1 => thumb16_b_cond(ir, inst),
        B_t2 => thumb16_b_uncond(ir, inst),

        // Load/Store
        LDR_literal => thumb16_ldr_literal(ir, inst),
        LDR_reg => thumb16_ldr_reg(ir, inst),
        LDR_imm_t1 => thumb16_ldr_imm_t1(ir, inst),
        LDR_imm_t2 => thumb16_ldr_imm_t2(ir, inst),
        STR_reg => thumb16_str_reg(ir, inst),
        STR_imm_t1 => thumb16_str_imm_t1(ir, inst),
        STR_imm_t2 => thumb16_str_imm_t2(ir, inst),
        LDRB_reg => thumb16_ldrb_reg(ir, inst),
        LDRB_imm => thumb16_ldrb_imm(ir, inst),
        STRB_reg => thumb16_strb_reg(ir, inst),
        STRB_imm => thumb16_strb_imm(ir, inst),
        LDRH_reg => thumb16_ldrh_reg(ir, inst),
        LDRH_imm => thumb16_ldrh_imm(ir, inst),
        STRH_reg => thumb16_strh_reg(ir, inst),
        STRH_imm => thumb16_strh_imm(ir, inst),
        LDRSB_reg => thumb16_ldrsb_reg(ir, inst),
        LDRSH_reg => thumb16_ldrsh_reg(ir, inst),

        // Address generation
        ADR => thumb16_adr(ir, inst),
        ADD_sp_t1 => thumb16_add_sp_imm_t1(ir, inst),
        ADD_sp_t2 => thumb16_add_sp_imm_t2(ir, inst),
        SUB_sp => thumb16_sub_sp(ir, inst),

        // Extensions
        SXTH => thumb16_sxth(ir, inst),
        SXTB => thumb16_sxtb(ir, inst),
        UXTH => thumb16_uxth(ir, inst),
        UXTB => thumb16_uxtb(ir, inst),

        // Load/Store multiple
        PUSH => thumb16_push(ir, inst),
        POP => thumb16_pop(ir, inst),
        STMIA => thumb16_stmia(ir, inst),
        LDMIA => thumb16_ldmia(ir, inst),

        // Reversal
        REV => thumb16_rev(ir, inst),
        REV16 => thumb16_rev16(ir, inst),
        REVSH => thumb16_revsh(ir, inst),

        // Misc
        NOP => true,
        SEV => thumb16_hint(ir, options, Exception::SendEvent),
        SEVL => thumb16_hint(ir, options, Exception::SendEventLocal),
        WFE => thumb16_hint(ir, options, Exception::WaitForEvent),
        WFI => thumb16_hint(ir, options, Exception::WaitForInterrupt),
        YIELD => thumb16_hint(ir, options, Exception::Yield),
        BKPT => super::raise_exception_with_instruction_size(
            ir,
            crate::frontend::a32::types::Exception::Breakpoint,
            2,
        ),
        SVC => thumb16_svc(ir, inst),
        UDF => super::raise_exception_with_instruction_size(
            ir,
            crate::frontend::a32::types::Exception::UndefinedInstruction,
            2,
        ),

        // IT
        IT => thumb16_it(ir, inst),

        // CBZ/CBNZ
        CBZ_CBNZ => thumb16_cbz_cbnz(ir, inst),

        SETEND | CPS => true, // Privileged, ignore
        Unknown => super::raise_exception_with_instruction_size(
            ir,
            crate::frontend::a32::types::Exception::UndefinedInstruction,
            2,
        ),
    }
}

fn thumb16_hint(ir: &mut A32IREmitter, options: TranslationOptions, exception: Exception) -> bool {
    if !options.hook_hint_instructions {
        return true;
    }
    super::raise_exception_with_instruction_size(ir, exception, 2)
}

// --- Shift immediate ---

fn thumb16_lsl_imm(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rd_lo();
    let rm = inst.rn_lo();
    let imm5 = inst.imm5();
    let carry_in = ir.get_c_flag();
    let rm_val = ir.get_register(rm);
    let (result, carry) = emit_imm_shift(ir, rm_val, ShiftType::LSL, imm5, carry_in);
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nzc(nzcv, carry);
    ir.set_register(rd, result);
    true
}

fn thumb16_lsr_imm(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rd_lo();
    let rm = inst.rn_lo();
    let imm5 = inst.imm5();
    let carry_in = ir.get_c_flag();
    let rm_val = ir.get_register(rm);
    let (result, carry) = emit_imm_shift(ir, rm_val, ShiftType::LSR, imm5, carry_in);
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nzc(nzcv, carry);
    ir.set_register(rd, result);
    true
}

fn thumb16_asr_imm(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rd_lo();
    let rm = inst.rn_lo();
    let imm5 = inst.imm5();
    let carry_in = ir.get_c_flag();
    let rm_val = ir.get_register(rm);
    let (result, carry) = emit_imm_shift(ir, rm_val, ShiftType::ASR, imm5, carry_in);
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nzc(nzcv, carry);
    ir.set_register(rd, result);
    true
}

// --- Add/Sub ---

fn thumb16_add_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rd_lo();
    let rn = inst.rn_lo();
    let rm = inst.rm_lo();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let result = ir.ir().add_32(rn_val, rm_val, Value::ImmU1(false));
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nzcv(nzcv);
    ir.set_register(rd, result);
    true
}

fn thumb16_sub_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rd_lo();
    let rn = inst.rn_lo();
    let rm = inst.rm_lo();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let result = ir.ir().sub_32(rn_val, rm_val, Value::ImmU1(true));
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nzcv(nzcv);
    ir.set_register(rd, result);
    true
}

fn thumb16_add_imm3(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rd_lo();
    let rn = inst.rn_lo();
    let imm3 = inst.imm3();
    let rn_val = ir.get_register(rn);
    let result = ir
        .ir()
        .add_32(rn_val, Value::ImmU32(imm3), Value::ImmU1(false));
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nzcv(nzcv);
    ir.set_register(rd, result);
    true
}

fn thumb16_sub_imm3(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rd_lo();
    let rn = inst.rn_lo();
    let imm3 = inst.imm3();
    let rn_val = ir.get_register(rn);
    let result = ir
        .ir()
        .sub_32(rn_val, Value::ImmU32(imm3), Value::ImmU1(true));
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nzcv(nzcv);
    ir.set_register(rd, result);
    true
}

fn thumb16_mov_imm(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rt_hi();
    let imm8 = inst.imm8();
    let result = Value::ImmU32(imm8);
    // MOV imm sets N and Z
    let r = ir
        .ir()
        .add_32(result, Value::ImmU32(0), Value::ImmU1(false));
    let nzcv = ir.ir().get_nzcv_from_op(r);
    ir.set_cpsr_nz(nzcv);
    ir.set_register(rd, result);
    true
}

fn thumb16_cmp_imm(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rn = inst.rt_hi();
    let imm8 = inst.imm8();
    let rn_val = ir.get_register(rn);
    let result = ir
        .ir()
        .sub_32(rn_val, Value::ImmU32(imm8), Value::ImmU1(true));
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nzcv(nzcv);
    true
}

fn thumb16_add_imm8(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rdn = inst.rt_hi();
    let imm8 = inst.imm8();
    let rdn_val = ir.get_register(rdn);
    let result = ir
        .ir()
        .add_32(rdn_val, Value::ImmU32(imm8), Value::ImmU1(false));
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nzcv(nzcv);
    ir.set_register(rdn, result);
    true
}

fn thumb16_sub_imm8(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rdn = inst.rt_hi();
    let imm8 = inst.imm8();
    let rdn_val = ir.get_register(rdn);
    let result = ir
        .ir()
        .sub_32(rdn_val, Value::ImmU32(imm8), Value::ImmU1(true));
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nzcv(nzcv);
    ir.set_register(rdn, result);
    true
}

// --- Data processing ---

fn thumb16_and_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rdn = inst.rd_lo();
    let rm = inst.rn_lo();
    let rdn_val = ir.get_register(rdn);
    let rm_val = ir.get_register(rm);
    let result = ir.ir().and_32(rdn_val, rm_val);
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nz(nzcv);
    ir.set_register(rdn, result);
    true
}

fn thumb16_eor_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rdn = inst.rd_lo();
    let rm = inst.rn_lo();
    let rdn_val = ir.get_register(rdn);
    let rm_val = ir.get_register(rm);
    let result = ir.ir().eor_32(rdn_val, rm_val);
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nz(nzcv);
    ir.set_register(rdn, result);
    true
}

fn thumb16_lsl_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rdn = inst.rd_lo();
    let rm = inst.rn_lo();
    let carry_in = ir.get_c_flag();
    let rdn_val = ir.get_register(rdn);
    let rm_val = ir.get_register(rm);
    let amount = ir.ir().least_significant_byte(rm_val);
    let result = ir.ir().logical_shift_left_32(rdn_val, amount, carry_in);
    let nzcv = ir.ir().get_nzcv_from_op(result);
    let carry = ir.ir().get_carry_from_op(result);
    ir.set_cpsr_nzc(nzcv, carry);
    ir.set_register(rdn, result);
    true
}

fn thumb16_lsr_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rdn = inst.rd_lo();
    let rm = inst.rn_lo();
    let carry_in = ir.get_c_flag();
    let rdn_val = ir.get_register(rdn);
    let rm_val = ir.get_register(rm);
    let amount = ir.ir().least_significant_byte(rm_val);
    let result = ir.ir().logical_shift_right_32(rdn_val, amount, carry_in);
    let nzcv = ir.ir().get_nzcv_from_op(result);
    let carry = ir.ir().get_carry_from_op(result);
    ir.set_cpsr_nzc(nzcv, carry);
    ir.set_register(rdn, result);
    true
}

fn thumb16_asr_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rdn = inst.rd_lo();
    let rm = inst.rn_lo();
    let carry_in = ir.get_c_flag();
    let rdn_val = ir.get_register(rdn);
    let rm_val = ir.get_register(rm);
    let amount = ir.ir().least_significant_byte(rm_val);
    let result = ir.ir().arithmetic_shift_right_32(rdn_val, amount, carry_in);
    let nzcv = ir.ir().get_nzcv_from_op(result);
    let carry = ir.ir().get_carry_from_op(result);
    ir.set_cpsr_nzc(nzcv, carry);
    ir.set_register(rdn, result);
    true
}

fn thumb16_adc_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rdn = inst.rd_lo();
    let rm = inst.rn_lo();
    let c = ir.get_c_flag();
    let rdn_val = ir.get_register(rdn);
    let rm_val = ir.get_register(rm);
    let result = ir.ir().add_32(rdn_val, rm_val, c);
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nzcv(nzcv);
    ir.set_register(rdn, result);
    true
}

fn thumb16_sbc_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rdn = inst.rd_lo();
    let rm = inst.rn_lo();
    let c = ir.get_c_flag();
    let rdn_val = ir.get_register(rdn);
    let rm_val = ir.get_register(rm);
    let result = ir.ir().sub_32(rdn_val, rm_val, c);
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nzcv(nzcv);
    ir.set_register(rdn, result);
    true
}

fn thumb16_ror_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rdn = inst.rd_lo();
    let rm = inst.rn_lo();
    let carry_in = ir.get_c_flag();
    let rdn_val = ir.get_register(rdn);
    let rm_val = ir.get_register(rm);
    let amount = ir.ir().least_significant_byte(rm_val);
    let result = ir.ir().rotate_right_32(rdn_val, amount, carry_in);
    let nzcv = ir.ir().get_nzcv_from_op(result);
    let carry = ir.ir().get_carry_from_op(result);
    ir.set_cpsr_nzc(nzcv, carry);
    ir.set_register(rdn, result);
    true
}

fn thumb16_tst_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rn = inst.rd_lo();
    let rm = inst.rn_lo();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let result = ir.ir().and_32(rn_val, rm_val);
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nz(nzcv);
    true
}

fn thumb16_rsb_imm(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    // RSB Rd, Rn, #0 (negate)
    let rd = inst.rd_lo();
    let rn = inst.rn_lo();
    let rn_val = ir.get_register(rn);
    let result = ir.ir().sub_32(Value::ImmU32(0), rn_val, Value::ImmU1(true));
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nzcv(nzcv);
    ir.set_register(rd, result);
    true
}

fn thumb16_cmp_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rn = inst.rd_lo();
    let rm = inst.rn_lo();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let result = ir.ir().sub_32(rn_val, rm_val, Value::ImmU1(true));
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nzcv(nzcv);
    true
}

fn thumb16_cmn_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rn = inst.rd_lo();
    let rm = inst.rn_lo();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let result = ir.ir().add_32(rn_val, rm_val, Value::ImmU1(false));
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nzcv(nzcv);
    true
}

fn thumb16_orr_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rdn = inst.rd_lo();
    let rm = inst.rn_lo();
    let rdn_val = ir.get_register(rdn);
    let rm_val = ir.get_register(rm);
    let result = ir.ir().or_32(rdn_val, rm_val);
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nz(nzcv);
    ir.set_register(rdn, result);
    true
}

fn thumb16_mul_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rdn = inst.rd_lo();
    let rm = inst.rn_lo();
    let rdn_val = ir.get_register(rdn);
    let rm_val = ir.get_register(rm);
    let result = ir.ir().mul_32(rdn_val, rm_val);
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nz(nzcv);
    ir.set_register(rdn, result);
    true
}

fn thumb16_bic_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rdn = inst.rd_lo();
    let rm = inst.rn_lo();
    let rdn_val = ir.get_register(rdn);
    let rm_val = ir.get_register(rm);
    let not_rm = ir.ir().not_32(rm_val);
    let result = ir.ir().and_32(rdn_val, not_rm);
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nz(nzcv);
    ir.set_register(rdn, result);
    true
}

fn thumb16_mvn_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rd_lo();
    let rm = inst.rn_lo();
    let rm_val = ir.get_register(rm);
    let result = ir.ir().not_32(rm_val);
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nz(nzcv);
    ir.set_register(rd, result);
    true
}

// --- Special data ---

fn thumb16_add_reg_t2(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rdn = inst.rd_dn();
    let rm = inst.rm_hi();
    let rdn_val = ir.get_register(rdn);
    let rm_val = ir.get_register(rm);
    let result = ir.ir().add_32(rdn_val, rm_val, Value::ImmU1(false));
    if rdn == Reg::R15 {
        ir.update_upper_location_descriptor();
        ir.alu_write_pc(result);
        ir.set_term(Terminal::FastDispatchHint);
        return false;
    }
    ir.set_register(rdn, result);
    true
}

fn thumb16_cmp_reg_t2(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rn = inst.rn_dn();
    let rm = inst.rm_hi();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let result = ir.ir().sub_32(rn_val, rm_val, Value::ImmU1(true));
    let nzcv = ir.ir().get_nzcv_from_op(result);
    ir.set_cpsr_nzcv(nzcv);
    true
}

fn thumb16_mov_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rd_dn();
    let rm = inst.rm_hi();
    let value = ir.get_register(rm);
    if rd == Reg::R15 {
        ir.update_upper_location_descriptor();
        ir.alu_write_pc(value);
        ir.set_term(Terminal::FastDispatchHint);
        return false;
    }
    ir.set_register(rd, value);
    true
}

// --- Branch ---

fn thumb16_bx(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rm = inst.rm_hi();
    let target = ir.get_register(rm);
    ir.update_upper_location_descriptor();
    ir.bx_write_pc(target);
    if rm == Reg::R14 {
        ir.set_term(Terminal::PopRSBHint);
    } else {
        ir.set_term(Terminal::FastDispatchHint);
    }
    false
}

fn thumb16_blx_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rm = inst.rm_hi();
    let loc = ir.current_location.expect("location not set");
    let target = ir.get_register(rm);
    ir.base
        .push_rsb(loc.advance_pc(2).advance_it().to_location());
    ir.update_upper_location_descriptor();
    ir.bx_write_pc(target);
    let return_addr = loc.pc().wrapping_add(2) | 1; // Thumb bit
    ir.set_register(Reg::R14, Value::ImmU32(return_addr));
    ir.set_term(Terminal::FastDispatchHint);
    false
}

fn thumb16_b_cond(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let cond = crate::ir::cond::Cond::from_u8(inst.cond());
    let offset = ((inst.imm8() as i8) as i32) << 1;
    let loc = ir.current_location.expect("location not set");
    let target_pc = (loc.pc() as i32).wrapping_add(4).wrapping_add(offset) as u32;

    let next = loc.advance_pc(2);
    let target = loc.set_pc(target_pc);

    ir.set_term(Terminal::if_then_else(
        cond,
        Terminal::link_block(target.to_location()),
        Terminal::link_block(next.to_location()),
    ));
    false
}

fn thumb16_b_uncond(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let imm11 = inst.imm11();
    // Sign-extend from 12 bits (imm11 << 1)
    let offset = (((imm11 << 1) as i32) << 20) >> 20;
    let loc = ir.current_location.expect("location not set");
    let target_pc = (loc.pc() as i32).wrapping_add(4).wrapping_add(offset) as u32;

    let target = loc.set_pc(target_pc);
    ir.set_term(Terminal::link_block(target.to_location()));
    false
}

// --- Load/Store ---

fn thumb16_ldr_literal(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rt = inst.rt_hi();
    let imm8 = inst.imm8() << 2;
    let loc = ir.current_location.expect("location not set");
    let base = (loc.pc().wrapping_add(4)) & !3; // Align PC
    let address = Value::ImmU32(base.wrapping_add(imm8));
    let value = ir.read_memory_32(address, AccType::Normal);
    ir.set_register(rt, value);
    true
}

fn thumb16_ldr_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rt = inst.rd_lo();
    let rn = inst.rn_lo();
    let rm = inst.rm_lo();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let address = ir.ir().add_32(rn_val, rm_val, Value::ImmU1(false));
    let value = ir.read_memory_32(address, AccType::Normal);
    ir.set_register(rt, value);
    true
}

fn thumb16_ldr_imm_t1(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rt = inst.rd_lo();
    let rn = inst.rn_lo();
    let imm5 = inst.imm5() << 2;
    let rn_val = ir.get_register(rn);
    let address = ir
        .ir()
        .add_32(rn_val, Value::ImmU32(imm5), Value::ImmU1(false));
    let value = ir.read_memory_32(address, AccType::Normal);
    ir.set_register(rt, value);
    true
}

fn thumb16_ldr_imm_t2(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rt = inst.rt_hi();
    let imm8 = inst.imm8() << 2;
    let sp = ir.get_register(Reg::R13);
    let address = ir.ir().add_32(sp, Value::ImmU32(imm8), Value::ImmU1(false));
    let value = ir.read_memory_32(address, AccType::Normal);
    ir.set_register(rt, value);
    true
}

fn thumb16_str_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rt = inst.rd_lo();
    let rn = inst.rn_lo();
    let rm = inst.rm_lo();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let address = ir.ir().add_32(rn_val, rm_val, Value::ImmU1(false));
    let value = ir.get_register(rt);
    ir.write_memory_32(address, value, AccType::Normal);
    true
}

fn thumb16_str_imm_t1(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rt = inst.rd_lo();
    let rn = inst.rn_lo();
    let imm5 = inst.imm5() << 2;
    let rn_val = ir.get_register(rn);
    let address = ir
        .ir()
        .add_32(rn_val, Value::ImmU32(imm5), Value::ImmU1(false));
    let value = ir.get_register(rt);
    ir.write_memory_32(address, value, AccType::Normal);
    true
}

fn thumb16_str_imm_t2(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rt = inst.rt_hi();
    let imm8 = inst.imm8() << 2;
    let sp = ir.get_register(Reg::R13);
    let address = ir.ir().add_32(sp, Value::ImmU32(imm8), Value::ImmU1(false));
    let value = ir.get_register(rt);
    ir.write_memory_32(address, value, AccType::Normal);
    true
}

fn thumb16_ldrb_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rt = inst.rd_lo();
    let rn = inst.rn_lo();
    let rm = inst.rm_lo();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let address = ir.ir().add_32(rn_val, rm_val, Value::ImmU1(false));
    let value = ir.read_memory_8(address, AccType::Normal);
    let extended = ir.ir().zero_extend_byte_to_word(value);
    ir.set_register(rt, extended);
    true
}

fn thumb16_ldrb_imm(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rt = inst.rd_lo();
    let rn = inst.rn_lo();
    let imm5 = inst.imm5();
    let rn_val = ir.get_register(rn);
    let address = ir
        .ir()
        .add_32(rn_val, Value::ImmU32(imm5), Value::ImmU1(false));
    let value = ir.read_memory_8(address, AccType::Normal);
    let extended = ir.ir().zero_extend_byte_to_word(value);
    ir.set_register(rt, extended);
    true
}

fn thumb16_strb_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rt = inst.rd_lo();
    let rn = inst.rn_lo();
    let rm = inst.rm_lo();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let address = ir.ir().add_32(rn_val, rm_val, Value::ImmU1(false));
    let value = ir.get_register(rt);
    let byte = ir.ir().least_significant_byte(value);
    ir.write_memory_8(address, byte, AccType::Normal);
    true
}

fn thumb16_strb_imm(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rt = inst.rd_lo();
    let rn = inst.rn_lo();
    let imm5 = inst.imm5();
    let rn_val = ir.get_register(rn);
    let address = ir
        .ir()
        .add_32(rn_val, Value::ImmU32(imm5), Value::ImmU1(false));
    let value = ir.get_register(rt);
    let byte = ir.ir().least_significant_byte(value);
    ir.write_memory_8(address, byte, AccType::Normal);
    true
}

fn thumb16_ldrh_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rt = inst.rd_lo();
    let rn = inst.rn_lo();
    let rm = inst.rm_lo();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let address = ir.ir().add_32(rn_val, rm_val, Value::ImmU1(false));
    let value = ir.read_memory_16(address, AccType::Normal);
    let extended = ir.ir().zero_extend_half_to_word(value);
    ir.set_register(rt, extended);
    true
}

fn thumb16_ldrh_imm(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rt = inst.rd_lo();
    let rn = inst.rn_lo();
    let imm5 = inst.imm5() << 1;
    let rn_val = ir.get_register(rn);
    let address = ir
        .ir()
        .add_32(rn_val, Value::ImmU32(imm5), Value::ImmU1(false));
    let value = ir.read_memory_16(address, AccType::Normal);
    let extended = ir.ir().zero_extend_half_to_word(value);
    ir.set_register(rt, extended);
    true
}

fn thumb16_strh_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rt = inst.rd_lo();
    let rn = inst.rn_lo();
    let rm = inst.rm_lo();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let address = ir.ir().add_32(rn_val, rm_val, Value::ImmU1(false));
    let value = ir.get_register(rt);
    let half = ir.ir().least_significant_half(value);
    ir.write_memory_16(address, half, AccType::Normal);
    true
}

fn thumb16_strh_imm(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rt = inst.rd_lo();
    let rn = inst.rn_lo();
    let imm5 = inst.imm5() << 1;
    let rn_val = ir.get_register(rn);
    let address = ir
        .ir()
        .add_32(rn_val, Value::ImmU32(imm5), Value::ImmU1(false));
    let value = ir.get_register(rt);
    let half = ir.ir().least_significant_half(value);
    ir.write_memory_16(address, half, AccType::Normal);
    true
}

fn thumb16_ldrsb_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rt = inst.rd_lo();
    let rn = inst.rn_lo();
    let rm = inst.rm_lo();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let address = ir.ir().add_32(rn_val, rm_val, Value::ImmU1(false));
    let value = ir.read_memory_8(address, AccType::Normal);
    let extended = ir.ir().sign_extend_byte_to_word(value);
    ir.set_register(rt, extended);
    true
}

fn thumb16_ldrsh_reg(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rt = inst.rd_lo();
    let rn = inst.rn_lo();
    let rm = inst.rm_lo();
    let rn_val = ir.get_register(rn);
    let rm_val = ir.get_register(rm);
    let address = ir.ir().add_32(rn_val, rm_val, Value::ImmU1(false));
    let value = ir.read_memory_16(address, AccType::Normal);
    let extended = ir.ir().sign_extend_half_to_word(value);
    ir.set_register(rt, extended);
    true
}

// --- Address generation ---

fn thumb16_adr(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rt_hi();
    let imm8 = inst.imm8() << 2;
    let loc = ir.current_location.expect("location not set");
    let base = (loc.pc().wrapping_add(4)) & !3;
    ir.set_register(rd, Value::ImmU32(base.wrapping_add(imm8)));
    true
}

fn thumb16_add_sp_imm_t1(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rt_hi();
    let imm8 = inst.imm8() << 2;
    let sp = ir.get_register(Reg::R13);
    let result = ir.ir().add_32(sp, Value::ImmU32(imm8), Value::ImmU1(false));
    ir.set_register(rd, result);
    true
}

fn thumb16_add_sp_imm_t2(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let imm7 = inst.imm7() << 2;
    let sp = ir.get_register(Reg::R13);
    let result = ir.ir().add_32(sp, Value::ImmU32(imm7), Value::ImmU1(false));
    ir.set_register(Reg::R13, result);
    true
}

fn thumb16_sub_sp(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let imm7 = inst.imm7() << 2;
    let sp = ir.get_register(Reg::R13);
    let result = ir.ir().sub_32(sp, Value::ImmU32(imm7), Value::ImmU1(true));
    ir.set_register(Reg::R13, result);
    true
}

// --- Extensions ---

fn thumb16_sxth(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rd_lo();
    let rm = inst.rn_lo();
    let rm_val = ir.get_register(rm);
    let result = ir.ir().sign_extend_half_to_word(rm_val);
    ir.set_register(rd, result);
    true
}

fn thumb16_sxtb(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rd_lo();
    let rm = inst.rn_lo();
    let rm_val = ir.get_register(rm);
    let result = ir.ir().sign_extend_byte_to_word(rm_val);
    ir.set_register(rd, result);
    true
}

fn thumb16_uxth(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rd_lo();
    let rm = inst.rn_lo();
    let rm_val = ir.get_register(rm);
    let result = ir.ir().and_32(rm_val, Value::ImmU32(0xFFFF));
    ir.set_register(rd, result);
    true
}

fn thumb16_uxtb(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rd_lo();
    let rm = inst.rn_lo();
    let rm_val = ir.get_register(rm);
    let result = ir.ir().and_32(rm_val, Value::ImmU32(0xFF));
    ir.set_register(rd, result);
    true
}

// --- Load/Store multiple ---

fn thumb16_push(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let mut reglist = inst.register_list() as u16;
    let lr_bit = (inst.raw >> 8) & 1 != 0;
    if lr_bit {
        reglist |= 1 << 14;
    }
    if reglist.count_ones() < 1 {
        return super::unpredictable_instruction(ir);
    }

    let sp = ir.get_register(Reg::R13);
    let new_sp = ir.ir().sub_32(
        sp,
        Value::ImmU32(reglist.count_ones() * 4),
        Value::ImmU1(true),
    );

    let mut addr = new_sp;
    for i in 0..16u32 {
        if reglist & (1 << i) != 0 {
            let reg = Reg::from_u32(i);
            let val = ir.get_register(reg);
            ir.write_memory_32(addr, val, AccType::Atomic);
            addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
        }
    }

    ir.set_register(Reg::R13, new_sp);
    true
}

fn thumb16_pop(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let mut reglist = inst.register_list() as u16;
    let pc_bit = (inst.raw >> 8) & 1 != 0;
    if pc_bit {
        reglist |= 1 << 15;
    }
    if reglist.count_ones() < 1 {
        return super::unpredictable_instruction(ir);
    }

    let mut addr = ir.get_register(Reg::R13);

    for i in 0..15u32 {
        if reglist & (1 << i) != 0 {
            let val = ir.read_memory_32(addr, AccType::Atomic);
            ir.set_register(Reg::from_u32(i), val);
            addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
        }
    }

    if pc_bit {
        let val = ir.read_memory_32(addr, AccType::Atomic);
        ir.update_upper_location_descriptor();
        ir.load_write_pc(val);
        addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
        ir.set_register(Reg::R13, addr);
        ir.set_term(Terminal::PopRSBHint);
        return false;
    }

    ir.set_register(Reg::R13, addr);
    true
}

fn thumb16_stmia(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rn = inst.rt_hi();
    let reglist = inst.register_list() as u16;

    let base = ir.get_register(rn);
    let mut addr = base;

    for i in 0..8u32 {
        if reglist & (1 << i) != 0 {
            let val = ir.get_register(Reg::from_u32(i));
            ir.write_memory_32(addr, val, AccType::Normal);
            addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
        }
    }

    // Writeback
    let count = reglist.count_ones();
    let new_base = ir
        .ir()
        .add_32(base, Value::ImmU32(count * 4), Value::ImmU1(false));
    ir.set_register(rn, new_base);
    true
}

fn thumb16_ldmia(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rn = inst.rt_hi();
    let reglist = inst.register_list() as u16;

    let base = ir.get_register(rn);
    let mut addr = base;

    for i in 0..8u32 {
        if reglist & (1 << i) != 0 {
            let val = ir.read_memory_32(addr, AccType::Normal);
            ir.set_register(Reg::from_u32(i), val);
            addr = ir.ir().add_32(addr, Value::ImmU32(4), Value::ImmU1(false));
        }
    }

    // Writeback if Rn not in register list
    if reglist & (1 << (rn as u32)) == 0 {
        let count = reglist.count_ones();
        let new_base = ir
            .ir()
            .add_32(base, Value::ImmU32(count * 4), Value::ImmU1(false));
        ir.set_register(rn, new_base);
    }
    true
}

// --- Reversal ---

fn thumb16_rev(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rd_lo();
    let rm = inst.rn_lo();
    let rm_val = ir.get_register(rm);
    let result = ir.ir().byte_reverse_word(rm_val);
    ir.set_register(rd, result);
    true
}

fn thumb16_rev16(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rd_lo();
    let rm = inst.rn_lo();
    let rm_val = ir.get_register(rm);
    // Swap bytes in each halfword
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

fn thumb16_revsh(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rd = inst.rd_lo();
    let rm = inst.rn_lo();
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

// --- System ---

fn thumb16_svc(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let imm8 = inst.imm8();
    let loc = ir.current_location.expect("location not set");
    ir.base
        .push_rsb(loc.advance_pc(2).advance_it().to_location());
    ir.update_upper_location_descriptor();
    ir.branch_write_pc(Value::ImmU32(loc.pc().wrapping_add(2)));
    ir.call_supervisor(imm8);
    ir.set_term(Terminal::CheckHalt {
        else_: Box::new(Terminal::PopRSBHint),
    });
    false
}

// --- IT ---

fn thumb16_it(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let imm8 = inst.imm8() as u8;
    assert_ne!(imm8 & 0xF, 0, "IT mask must not be zero");

    let cond = imm8 >> 4;
    let mask = imm8 & 0xF;
    if cond == 0xF || (cond == 0xE && mask.count_ones() != 1) {
        return super::raise_exception_with_instruction_size(
            ir,
            crate::frontend::a32::types::Exception::UnpredictableInstruction,
            2,
        );
    }

    let loc = ir.current_location.expect("location not set");
    if loc.it().is_in_it_block() {
        return super::raise_exception_with_instruction_size(
            ir,
            crate::frontend::a32::types::Exception::UnpredictableInstruction,
            2,
        );
    }

    let next = loc
        .advance_pc(2)
        .set_it(crate::frontend::a32::it_state::ITState::new(imm8));
    ir.set_term(Terminal::LinkBlockFast {
        next: next.to_location(),
    });
    false
}

// --- CBZ/CBNZ ---

fn thumb16_cbz_cbnz(ir: &mut A32IREmitter, inst: &DecodedThumb16) -> bool {
    let rn = inst.rd_lo();
    let i = ((inst.raw >> 9) & 1) as u32;
    let imm5 = ((inst.raw >> 3) & 0x1F) as u32;
    let offset = (i << 6) | (imm5 << 1);
    let nonzero = (inst.raw >> 11) & 1 != 0; // CBNZ if bit 11 set

    let loc = ir.current_location.expect("location not set");
    let target_pc = loc.pc().wrapping_add(4).wrapping_add(offset);

    let rn_val = ir.get_register(rn);
    let is_zero = ir.ir().is_zero_32(rn_val);
    ir.set_check_bit(is_zero);

    let next = loc.advance_pc(2);
    let target = loc.set_pc(target_pc);

    if nonzero {
        // CBNZ: branch if NOT zero → check_bit=is_zero, so else_ is the branch
        ir.set_term(Terminal::check_bit(
            Terminal::link_block(next.to_location()), // is_zero=true → fallthrough
            Terminal::link_block(target.to_location()), // is_zero=false → branch
        ));
    } else {
        // CBZ: branch if zero → check_bit=is_zero, so then_ is the branch
        ir.set_term(Terminal::check_bit(
            Terminal::link_block(target.to_location()), // is_zero=true → branch
            Terminal::link_block(next.to_location()),   // is_zero=false → fallthrough
        ));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::terminal::Terminal;
    use crate::ir::value::Value;

    #[test]
    fn thumb16_blx_reg_writes_pc_before_link_register() {
        let loc = A32LocationDescriptor::new(0x4000, PSR::new(0x20), FPSCR::default(), false);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);
        let inst = DecodedThumb16 {
            raw: 0x47F0,
            id: Thumb16InstId::BLX_reg,
        };

        assert!(!thumb16_blx_reg(&mut ir, &inst));

        let bx_pos = block
            .instructions
            .iter()
            .position(|inst| inst.opcode == Opcode::A32BXWritePC)
            .expect("missing A32BXWritePC");
        let lr_pos = block
            .instructions
            .iter()
            .position(|inst| {
                inst.opcode == Opcode::A32SetRegister && inst.args[0].get_a32_reg() == Reg::R14
            })
            .expect("missing LR write");

        assert!(bx_pos < lr_pos);
        assert!(matches!(block.terminal, Terminal::FastDispatchHint));
    }

    #[test]
    fn thumb16_it_links_to_descriptor_with_it_state() {
        let loc = A32LocationDescriptor::new(0x4000, PSR::new(0x20), FPSCR::default(), false);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);
        let inst = DecodedThumb16 {
            raw: 0xBF08, // IT EQ
            id: Thumb16InstId::IT,
        };

        assert!(!thumb16_it(&mut ir, &inst));
        let expected = loc
            .advance_pc(2)
            .set_it(crate::frontend::a32::it_state::ITState::new(0x08));
        assert!(matches!(
            block.terminal,
            Terminal::LinkBlockFast { next } if next == expected.to_location()
        ));
    }

    #[test]
    fn thumb16_it_reserved_condition_is_unpredictable() {
        let loc = A32LocationDescriptor::new(0x4000, PSR::new(0x20), FPSCR::default(), false);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);
        let inst = DecodedThumb16 {
            raw: 0xBFF8,
            id: Thumb16InstId::IT,
        };

        assert!(!thumb16_it(&mut ir, &inst));
        assert!(block
            .instructions
            .iter()
            .any(|inst| inst.opcode == Opcode::A32ExceptionRaised));
        assert!(matches!(
            block.terminal,
            Terminal::CheckHalt { ref else_ }
                if matches!(else_.as_ref(), Terminal::ReturnToDispatch)
        ));
    }

    #[test]
    fn thumb16_udf_raises_undefined_with_two_byte_return_pc() {
        let loc = A32LocationDescriptor::new(0x4000, PSR::new(0x20), FPSCR::default(), false);
        let mut block = Block::new(loc.to_location());
        let mut ir = A32IREmitter::with_location(&mut block, loc);
        let inst = DecodedThumb16 {
            raw: 0xDE00,
            id: Thumb16InstId::UDF,
        };

        assert!(!translate_thumb16(
            &mut ir,
            &inst,
            TranslationOptions::default(),
        ));
        let exception = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::A32ExceptionRaised)
            .expect("missing A32ExceptionRaised");
        assert_eq!(
            exception.args[1],
            Value::ImmU64(
                crate::frontend::a32::types::Exception::UndefinedInstruction.as_u32() as u64
            )
        );
        assert!(block.instructions.iter().any(|inst| {
            inst.opcode == Opcode::And32
                && inst.args[0] == Value::ImmU32(0x4002)
                && inst.args[1] == Value::ImmU32(0xFFFF_FFFE)
        }));
    }

    #[test]
    fn thumb16_hint_option_selects_nop_or_exception() {
        let loc = A32LocationDescriptor::new(0x4000, PSR::new(0x20), FPSCR::default(), false);
        let inst = DecodedThumb16 {
            raw: 0xBF30,
            id: Thumb16InstId::WFI,
        };

        let mut disabled = Block::new(loc.to_location());
        assert!(translate_thumb16(
            &mut A32IREmitter::with_location(&mut disabled, loc),
            &inst,
            TranslationOptions {
                hook_hint_instructions: false,
                ..TranslationOptions::default()
            },
        ));
        assert!(disabled.instructions.is_empty());

        let mut enabled = Block::new(loc.to_location());
        assert!(!translate_thumb16(
            &mut A32IREmitter::with_location(&mut enabled, loc),
            &inst,
            TranslationOptions::default(),
        ));
        let exception = enabled
            .instructions
            .iter()
            .find(|instruction| instruction.opcode == Opcode::A32ExceptionRaised)
            .expect("missing A32ExceptionRaised");
        assert_eq!(
            exception.args[1],
            Value::ImmU64(crate::frontend::a32::types::Exception::WaitForInterrupt.as_u32() as u64)
        );
    }

    #[test]
    fn thumb16_push_pop_use_atomic_stack_accesses() {
        let loc = A32LocationDescriptor::new(0x4000, PSR::new(0x20), FPSCR::default(), false);

        let mut push_block = Block::new(loc.to_location());
        {
            let mut ir = A32IREmitter::with_location(&mut push_block, loc);
            let inst = DecodedThumb16 {
                raw: 0xB501, // PUSH {r0, lr}
                id: Thumb16InstId::PUSH,
            };
            assert!(thumb16_push(&mut ir, &inst));
        }
        let writes: Vec<_> = push_block
            .instructions
            .iter()
            .filter(|inst| inst.opcode == Opcode::A32WriteMemory32)
            .collect();
        assert_eq!(writes.len(), 2);
        assert!(writes
            .iter()
            .all(|inst| inst.args[3] == Value::ImmAccType(AccType::Atomic)));

        let mut pop_block = Block::new(loc.to_location());
        {
            let mut ir = A32IREmitter::with_location(&mut pop_block, loc);
            let inst = DecodedThumb16 {
                raw: 0xBD01, // POP {r0, pc}
                id: Thumb16InstId::POP,
            };
            assert!(!thumb16_pop(&mut ir, &inst));
        }
        let reads: Vec<_> = pop_block
            .instructions
            .iter()
            .filter(|inst| inst.opcode == Opcode::A32ReadMemory32)
            .collect();
        assert_eq!(reads.len(), 2);
        assert!(reads
            .iter()
            .all(|inst| inst.args[2] == Value::ImmAccType(AccType::Atomic)));
        let last_increment = pop_block
            .instructions
            .iter()
            .rposition(|inst| inst.opcode == Opcode::Add32)
            .expect("POP address increment");
        let sp_write = pop_block
            .instructions
            .iter()
            .position(|inst| {
                inst.opcode == Opcode::A32SetRegister && inst.args[0].get_a32_reg() == Reg::R13
            })
            .expect("POP stack-pointer write");
        assert!(last_increment < sp_write);
        assert!(matches!(pop_block.terminal, Terminal::PopRSBHint));
    }

    #[test]
    fn thumb16_empty_push_pop_are_unpredictable() {
        let loc = A32LocationDescriptor::new(0x4000, PSR::new(0x20), FPSCR::default(), false);
        for (raw, id) in [(0xB400, Thumb16InstId::PUSH), (0xBC00, Thumb16InstId::POP)] {
            let mut block = Block::new(loc.to_location());
            let mut ir = A32IREmitter::with_location(&mut block, loc);
            let inst = DecodedThumb16 { raw, id };
            let result = match id {
                Thumb16InstId::PUSH => thumb16_push(&mut ir, &inst),
                Thumb16InstId::POP => thumb16_pop(&mut ir, &inst),
                _ => unreachable!(),
            };
            assert!(!result);
            assert!(block
                .instructions
                .iter()
                .any(|inst| inst.opcode == Opcode::A32ExceptionRaised));
            assert!(!block
                .instructions
                .iter()
                .any(|inst| inst.opcode == Opcode::A32GetRegister));
        }
    }
}
