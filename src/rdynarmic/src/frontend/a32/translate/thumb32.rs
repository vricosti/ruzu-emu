use super::thumb32_branch;
use super::thumb32_control;
use super::thumb32_coprocessor;
use super::thumb32_data_processing_modified_immediate;
use super::thumb32_data_processing_plain_binary_immediate;
use super::thumb32_data_processing_register;
use super::thumb32_data_processing_shifted_register;
use super::thumb32_load_byte;
use super::thumb32_load_halfword;
use super::thumb32_load_store_dual;
use super::thumb32_load_store_multiple;
use super::thumb32_load_word;
use super::thumb32_long_multiply;
use super::thumb32_misc;
use super::thumb32_multiply;
use super::thumb32_parallel;
use super::thumb32_store_single_data_item;
use crate::frontend::a32::decoder_thumb32::{DecodedThumb32, Thumb32InstId};
use crate::frontend::a32::types::Reg;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

use super::TranslationOptions;

/// Translate a single Thumb32 instruction. Returns true to continue.
pub fn translate_thumb32(
    ir: &mut A32IREmitter,
    inst: &DecodedThumb32,
    options: TranslationOptions,
) -> bool {
    use Thumb32InstId::*;
    match inst.id {
        // Data processing (modified immediate)
        TST_imm => thumb32_data_processing_modified_immediate::thumb32_tst_imm(ir, inst),
        AND_imm => thumb32_data_processing_modified_immediate::thumb32_and_imm(ir, inst),
        BIC_imm => thumb32_data_processing_modified_immediate::thumb32_bic_imm(ir, inst),
        MOV_imm => thumb32_data_processing_modified_immediate::thumb32_mov_imm(ir, inst),
        ORR_imm => thumb32_data_processing_modified_immediate::thumb32_orr_imm(ir, inst),
        MVN_imm => thumb32_data_processing_modified_immediate::thumb32_mvn_imm(ir, inst),
        ORN_imm => thumb32_data_processing_modified_immediate::thumb32_orn_imm(ir, inst),
        TEQ_imm => thumb32_data_processing_modified_immediate::thumb32_teq_imm(ir, inst),
        EOR_imm => thumb32_data_processing_modified_immediate::thumb32_eor_imm(ir, inst),
        CMN_imm => thumb32_data_processing_modified_immediate::thumb32_cmn_imm(ir, inst),
        ADD_imm_1 => thumb32_data_processing_modified_immediate::thumb32_add_imm_1(ir, inst),
        ADC_imm => thumb32_data_processing_modified_immediate::thumb32_adc_imm(ir, inst),
        SBC_imm => thumb32_data_processing_modified_immediate::thumb32_sbc_imm(ir, inst),
        CMP_imm => thumb32_data_processing_modified_immediate::thumb32_cmp_imm(ir, inst),
        SUB_imm_1 => thumb32_data_processing_modified_immediate::thumb32_sub_imm_1(ir, inst),
        RSB_imm => thumb32_data_processing_modified_immediate::thumb32_rsb_imm(ir, inst),

        // Data processing (shifted register)
        TST_reg => thumb32_data_processing_shifted_register::thumb32_tst_reg(ir, inst),
        AND_reg => thumb32_data_processing_shifted_register::thumb32_and_reg(ir, inst),
        BIC_reg => thumb32_data_processing_shifted_register::thumb32_bic_reg(ir, inst),
        MOV_reg => thumb32_data_processing_shifted_register::thumb32_mov_reg(ir, inst),
        ORR_reg => thumb32_data_processing_shifted_register::thumb32_orr_reg(ir, inst),
        MVN_reg => thumb32_data_processing_shifted_register::thumb32_mvn_reg(ir, inst),
        ORN_reg => thumb32_data_processing_shifted_register::thumb32_orn_reg(ir, inst),
        TEQ_reg => thumb32_data_processing_shifted_register::thumb32_teq_reg(ir, inst),
        EOR_reg => thumb32_data_processing_shifted_register::thumb32_eor_reg(ir, inst),
        PKH => thumb32_data_processing_shifted_register::thumb32_pkh(ir, inst),
        CMN_reg => thumb32_data_processing_shifted_register::thumb32_cmn_reg(ir, inst),
        ADD_reg => thumb32_data_processing_shifted_register::thumb32_add_reg(ir, inst),
        ADC_reg => thumb32_data_processing_shifted_register::thumb32_adc_reg(ir, inst),
        SBC_reg => thumb32_data_processing_shifted_register::thumb32_sbc_reg(ir, inst),
        CMP_reg => thumb32_data_processing_shifted_register::thumb32_cmp_reg(ir, inst),
        SUB_reg => thumb32_data_processing_shifted_register::thumb32_sub_reg(ir, inst),
        RSB_reg => thumb32_data_processing_shifted_register::thumb32_rsb_reg(ir, inst),

        // Data processing (plain binary immediate)
        ADR_t2 => thumb32_data_processing_plain_binary_immediate::thumb32_adr_t2(ir, inst),
        ADR_t3 => thumb32_data_processing_plain_binary_immediate::thumb32_adr_t3(ir, inst),
        ADD_imm_2 => thumb32_data_processing_plain_binary_immediate::thumb32_add_imm_2(ir, inst),
        BFC => thumb32_data_processing_plain_binary_immediate::thumb32_bfc(ir, inst),
        BFI => thumb32_data_processing_plain_binary_immediate::thumb32_bfi(ir, inst),
        MOVT => thumb32_data_processing_plain_binary_immediate::thumb32_movt(ir, inst),
        MOVW_imm => thumb32_data_processing_plain_binary_immediate::thumb32_movw_imm(ir, inst),
        SBFX => thumb32_data_processing_plain_binary_immediate::thumb32_sbfx(ir, inst),
        SSAT => thumb32_data_processing_plain_binary_immediate::thumb32_ssat(ir, inst),
        SSAT16 => thumb32_data_processing_plain_binary_immediate::thumb32_ssat16(ir, inst),
        SUB_imm_2 => thumb32_data_processing_plain_binary_immediate::thumb32_sub_imm_2(ir, inst),
        UBFX => thumb32_data_processing_plain_binary_immediate::thumb32_ubfx(ir, inst),
        USAT => thumb32_data_processing_plain_binary_immediate::thumb32_usat(ir, inst),
        USAT16 => thumb32_data_processing_plain_binary_immediate::thumb32_usat16(ir, inst),

        // Branch
        B => thumb32_branch::thumb32_b(ir, inst),
        B_cond => thumb32_branch::thumb32_b_cond(ir, inst),
        BL_imm => thumb32_branch::thumb32_bl_imm(ir, inst),
        BLX_imm => thumb32_branch::thumb32_blx_imm(ir, inst),

        // Load/Store
        LDR_lit => thumb32_load_word::thumb32_ldr_lit(ir, inst),
        LDR_imm_t4 => thumb32_load_word::thumb32_ldr_imm8(ir, inst),
        LDR_imm_t3 => thumb32_load_word::thumb32_ldr_imm12(ir, inst),
        LDR_reg => thumb32_load_word::thumb32_ldr_reg(ir, inst),
        LDRT => thumb32_load_word::thumb32_ldrt(ir, inst),
        STR_imm_1 => thumb32_store_single_data_item::thumb32_str_imm_1(ir, inst),
        STR_imm_2 => thumb32_store_single_data_item::thumb32_str_imm_2(ir, inst),
        STR_imm_3 => thumb32_store_single_data_item::thumb32_str_imm_3(ir, inst),
        STRT => thumb32_store_single_data_item::thumb32_strt(ir, inst),
        STR_reg => thumb32_store_single_data_item::thumb32_str_reg(ir, inst),
        LDRB_lit => thumb32_load_byte::thumb32_ldrb_lit(ir, inst),
        LDRB_imm_t3 => thumb32_load_byte::thumb32_ldrb_imm8(ir, inst),
        LDRB_imm_t2 => thumb32_load_byte::thumb32_ldrb_imm12(ir, inst),
        LDRB_reg => thumb32_load_byte::thumb32_ldrb_reg(ir, inst),
        LDRBT => thumb32_load_byte::thumb32_ldrbt(ir, inst),
        STRB_imm_1 => thumb32_store_single_data_item::thumb32_strb_imm_1(ir, inst),
        STRB_imm_2 => thumb32_store_single_data_item::thumb32_strb_imm_2(ir, inst),
        STRB_imm_3 => thumb32_store_single_data_item::thumb32_strb_imm_3(ir, inst),
        STRBT => thumb32_store_single_data_item::thumb32_strbt(ir, inst),
        STRB_reg => thumb32_store_single_data_item::thumb32_strb(ir, inst),
        LDRH_lit => thumb32_load_halfword::thumb32_ldrh_lit(ir, inst),
        LDRH_reg => thumb32_load_halfword::thumb32_ldrh_reg(ir, inst),
        LDRH_imm_t3 => thumb32_load_halfword::thumb32_ldrh_imm8(ir, inst),
        LDRH_imm_t2 => thumb32_load_halfword::thumb32_ldrh_imm12(ir, inst),
        LDRHT => thumb32_load_halfword::thumb32_ldrht(ir, inst),
        STRH_imm_1 => thumb32_store_single_data_item::thumb32_strh_imm_1(ir, inst),
        STRH_imm_2 => thumb32_store_single_data_item::thumb32_strh_imm_2(ir, inst),
        STRH_imm_3 => thumb32_store_single_data_item::thumb32_strh_imm_3(ir, inst),
        STRHT => thumb32_store_single_data_item::thumb32_strht(ir, inst),
        STRH_reg => thumb32_store_single_data_item::thumb32_strh(ir, inst),
        LDRSB_lit => thumb32_load_byte::thumb32_ldrsb_lit(ir, inst),
        LDRSB_imm_t2 => thumb32_load_byte::thumb32_ldrsb_imm8(ir, inst),
        LDRSB_imm_t1 => thumb32_load_byte::thumb32_ldrsb_imm12(ir, inst),
        LDRSB_reg => thumb32_load_byte::thumb32_ldrsb_reg(ir, inst),
        LDRSBT => thumb32_load_byte::thumb32_ldrsbt(ir, inst),
        LDRSH_lit => thumb32_load_halfword::thumb32_ldrsh_lit(ir, inst),
        LDRSH_reg => thumb32_load_halfword::thumb32_ldrsh_reg(ir, inst),
        LDRSH_imm_t2 => thumb32_load_halfword::thumb32_ldrsh_imm8(ir, inst),
        LDRSH_imm_t1 => thumb32_load_halfword::thumb32_ldrsh_imm12(ir, inst),
        LDRSHT => thumb32_load_halfword::thumb32_ldrsht(ir, inst),

        // Load/Store dual, exclusive, and table branch
        LDA => thumb32_load_store_dual::thumb32_lda(ir, inst),
        LDRD_imm_1 => thumb32_load_store_dual::thumb32_ldrd_imm_1(ir, inst),
        LDRD_imm_2 => thumb32_load_store_dual::thumb32_ldrd_imm_2(ir, inst),
        LDRD_lit_1 => thumb32_load_store_dual::thumb32_ldrd_lit_1(ir, inst),
        LDRD_lit_2 => thumb32_load_store_dual::thumb32_ldrd_lit_2(ir, inst),
        STRD_imm_1 => thumb32_load_store_dual::thumb32_strd_imm_1(ir, inst),
        STRD_imm_2 => thumb32_load_store_dual::thumb32_strd_imm_2(ir, inst),
        LDREX => thumb32_load_store_dual::thumb32_ldrex(ir, inst),
        LDREXB => thumb32_load_store_dual::thumb32_ldrexb(ir, inst),
        LDREXH => thumb32_load_store_dual::thumb32_ldrexh(ir, inst),
        LDREXD => thumb32_load_store_dual::thumb32_ldrexd(ir, inst),
        STL => thumb32_load_store_dual::thumb32_stl(ir, inst),
        STREX => thumb32_load_store_dual::thumb32_strex(ir, inst),
        STREXB => thumb32_load_store_dual::thumb32_strexb(ir, inst),
        STREXH => thumb32_load_store_dual::thumb32_strexh(ir, inst),
        STREXD => thumb32_load_store_dual::thumb32_strexd(ir, inst),
        TBB => thumb32_load_store_dual::thumb32_tbb(ir, inst),
        TBH => thumb32_load_store_dual::thumb32_tbh(ir, inst),

        // Load/Store multiple
        LDMDB => thumb32_load_store_multiple::thumb32_ldmdb(ir, inst),
        LDMIA => thumb32_load_store_multiple::thumb32_ldmia(ir, inst),
        POP => thumb32_load_store_multiple::thumb32_pop(ir, inst),
        PUSH => thumb32_load_store_multiple::thumb32_push(ir, inst),
        STMIA => thumb32_load_store_multiple::thumb32_stmia(ir, inst),
        STMDB => thumb32_load_store_multiple::thumb32_stmdb(ir, inst),

        // Exclusive control
        CLREX => thumb32_control::thumb32_clrex(ir),
        BXJ => thumb32_control::thumb32_bxj(ir, inst),

        // Multiply
        MLA => thumb32_multiply::thumb32_mla(ir, inst),
        MLS => thumb32_multiply::thumb32_mls(ir, inst),
        MUL => thumb32_multiply::thumb32_mul(ir, inst),
        SMLAD => thumb32_multiply::thumb32_smlad(ir, inst),
        SMLAXY => thumb32_multiply::thumb32_smlaxy(ir, inst),
        SMLAWY => thumb32_multiply::thumb32_smlawy(ir, inst),
        SMLSD => thumb32_multiply::thumb32_smlsd(ir, inst),
        SMMLA => thumb32_multiply::thumb32_smmla(ir, inst),
        SMMLS => thumb32_multiply::thumb32_smmls(ir, inst),
        SMMUL => thumb32_multiply::thumb32_smmul(ir, inst),
        SMUAD => thumb32_multiply::thumb32_smuad(ir, inst),
        SMUSD => thumb32_multiply::thumb32_smusd(ir, inst),
        SMULXY => thumb32_multiply::thumb32_smulxy(ir, inst),
        SMULWY => thumb32_multiply::thumb32_smulwy(ir, inst),
        USAD8 => thumb32_multiply::thumb32_usad8(ir, inst),
        USADA8 => thumb32_multiply::thumb32_usada8(ir, inst),
        SDIV => thumb32_long_multiply::thumb32_sdiv(ir, inst),
        SMLAL => thumb32_long_multiply::thumb32_smlal(ir, inst),
        SMLALD => thumb32_long_multiply::thumb32_smlald(ir, inst),
        SMLALXY => thumb32_long_multiply::thumb32_smlalxy(ir, inst),
        SMLSLD => thumb32_long_multiply::thumb32_smlsld(ir, inst),
        SMULL => thumb32_long_multiply::thumb32_smull(ir, inst),
        UDIV => thumb32_long_multiply::thumb32_udiv(ir, inst),
        UMAAL => thumb32_long_multiply::thumb32_umaal(ir, inst),
        UMLAL => thumb32_long_multiply::thumb32_umlal(ir, inst),
        UMULL => thumb32_long_multiply::thumb32_umull(ir, inst),

        // Coprocessor
        MCRR => thumb32_coprocessor::thumb32_mcrr(ir, inst),
        MRRC => thumb32_coprocessor::thumb32_mrrc(ir, inst),
        STC => thumb32_coprocessor::thumb32_stc(ir, inst),
        LDC => thumb32_coprocessor::thumb32_ldc(ir, inst),
        CDP => thumb32_coprocessor::thumb32_cdp(ir, inst),
        MCR => thumb32_coprocessor::thumb32_mcr(ir, inst),
        MRC => thumb32_coprocessor::thumb32_mrc(ir, inst),

        // Parallel addition and subtraction
        SADD8 => thumb32_parallel::thumb32_sadd8(ir, inst),
        SADD16 => thumb32_parallel::thumb32_sadd16(ir, inst),
        SASX => thumb32_parallel::thumb32_sasx(ir, inst),
        SSAX => thumb32_parallel::thumb32_ssax(ir, inst),
        SSUB8 => thumb32_parallel::thumb32_ssub8(ir, inst),
        SSUB16 => thumb32_parallel::thumb32_ssub16(ir, inst),
        UADD8 => thumb32_parallel::thumb32_uadd8(ir, inst),
        UADD16 => thumb32_parallel::thumb32_uadd16(ir, inst),
        UASX => thumb32_parallel::thumb32_uasx(ir, inst),
        USAX => thumb32_parallel::thumb32_usax(ir, inst),
        USUB8 => thumb32_parallel::thumb32_usub8(ir, inst),
        USUB16 => thumb32_parallel::thumb32_usub16(ir, inst),
        QADD8 => thumb32_parallel::thumb32_qadd8(ir, inst),
        QADD16 => thumb32_parallel::thumb32_qadd16(ir, inst),
        QASX => thumb32_parallel::thumb32_qasx(ir, inst),
        QSAX => thumb32_parallel::thumb32_qsax(ir, inst),
        QSUB8 => thumb32_parallel::thumb32_qsub8(ir, inst),
        QSUB16 => thumb32_parallel::thumb32_qsub16(ir, inst),
        UQADD8 => thumb32_parallel::thumb32_uqadd8(ir, inst),
        UQADD16 => thumb32_parallel::thumb32_uqadd16(ir, inst),
        UQASX => thumb32_parallel::thumb32_uqasx(ir, inst),
        UQSAX => thumb32_parallel::thumb32_uqsax(ir, inst),
        UQSUB8 => thumb32_parallel::thumb32_uqsub8(ir, inst),
        UQSUB16 => thumb32_parallel::thumb32_uqsub16(ir, inst),
        SHADD8 => thumb32_parallel::thumb32_shadd8(ir, inst),
        SHADD16 => thumb32_parallel::thumb32_shadd16(ir, inst),
        SHASX => thumb32_parallel::thumb32_shasx(ir, inst),
        SHSAX => thumb32_parallel::thumb32_shsax(ir, inst),
        SHSUB8 => thumb32_parallel::thumb32_shsub8(ir, inst),
        SHSUB16 => thumb32_parallel::thumb32_shsub16(ir, inst),
        UHADD8 => thumb32_parallel::thumb32_uhadd8(ir, inst),
        UHADD16 => thumb32_parallel::thumb32_uhadd16(ir, inst),
        UHASX => thumb32_parallel::thumb32_uhasx(ir, inst),
        UHSAX => thumb32_parallel::thumb32_uhsax(ir, inst),
        UHSUB8 => thumb32_parallel::thumb32_uhsub8(ir, inst),
        UHSUB16 => thumb32_parallel::thumb32_uhsub16(ir, inst),

        // Misc
        CLZ => thumb32_misc::thumb32_clz(ir, inst),
        QADD => thumb32_misc::thumb32_qadd(ir, inst),
        QDADD => thumb32_misc::thumb32_qdadd(ir, inst),
        QDSUB => thumb32_misc::thumb32_qdsub(ir, inst),
        QSUB => thumb32_misc::thumb32_qsub(ir, inst),
        RBIT => thumb32_misc::thumb32_rbit(ir, inst),
        REV => thumb32_misc::thumb32_rev(ir, inst),
        REV16 => thumb32_misc::thumb32_rev16(ir, inst),
        REVSH => thumb32_misc::thumb32_revsh(ir, inst),
        SEL => thumb32_misc::thumb32_sel(ir, inst),
        LSL_reg => thumb32_data_processing_register::thumb32_lsl_reg(ir, inst),
        LSR_reg => thumb32_data_processing_register::thumb32_lsr_reg(ir, inst),
        ASR_reg => thumb32_data_processing_register::thumb32_asr_reg(ir, inst),
        ROR_reg => thumb32_data_processing_register::thumb32_ror_reg(ir, inst),
        SXTB => thumb32_data_processing_register::thumb32_sxtb(ir, inst),
        SXTB16 => thumb32_data_processing_register::thumb32_sxtb16(ir, inst),
        SXTAB => thumb32_data_processing_register::thumb32_sxtab(ir, inst),
        SXTAB16 => thumb32_data_processing_register::thumb32_sxtab16(ir, inst),
        SXTH => thumb32_data_processing_register::thumb32_sxth(ir, inst),
        SXTAH => thumb32_data_processing_register::thumb32_sxtah(ir, inst),
        UXTB => thumb32_data_processing_register::thumb32_uxtb(ir, inst),
        UXTB16 => thumb32_data_processing_register::thumb32_uxtb16(ir, inst),
        UXTAB => thumb32_data_processing_register::thumb32_uxtab(ir, inst),
        UXTAB16 => thumb32_data_processing_register::thumb32_uxtab16(ir, inst),
        UXTH => thumb32_data_processing_register::thumb32_uxth(ir, inst),
        UXTAH => thumb32_data_processing_register::thumb32_uxtah(ir, inst),

        // Barriers
        DMB => thumb32_control::thumb32_dmb(ir),
        DSB => thumb32_control::thumb32_dsb(ir),
        ISB => thumb32_control::thumb32_isb(ir),

        // System
        MRS_reg => thumb32_control::thumb32_mrs_reg(ir, inst),
        MSR_reg => thumb32_control::thumb32_msr_reg(ir, inst),
        UDF | BKPT => thumb32_control::thumb32_udf(ir),
        NOP => thumb32_control::thumb32_nop(),
        SEV => thumb32_control::thumb32_sev(ir, options),
        SEVL => thumb32_control::thumb32_sevl(ir, options),
        WFE => thumb32_control::thumb32_wfe(ir, options),
        WFI => thumb32_control::thumb32_wfi(ir, options),
        YIELD => thumb32_control::thumb32_yield(ir, options),

        PLD_lit => thumb32_load_byte::thumb32_pld_lit(ir, inst, options),
        PLD_imm8 => thumb32_load_byte::thumb32_pld_imm8(ir, inst, options),
        PLD_imm12 => thumb32_load_byte::thumb32_pld_imm12(ir, inst, options),
        PLD_reg => thumb32_load_byte::thumb32_pld_reg(ir, inst, options),
        PLI_lit => thumb32_load_byte::thumb32_pli_lit(ir, inst, options),
        PLI_imm8 => thumb32_load_byte::thumb32_pli_imm8(ir, inst, options),
        PLI_imm12 => thumb32_load_byte::thumb32_pli_imm12(ir, inst, options),
        PLI_reg => thumb32_load_byte::thumb32_pli_reg(ir, inst, options),

        // Unmatched Thumb32 encoding → UndefinedInstruction with the full
        // RaiseException lifecycle (PC+4 for a 4-byte Thumb32 instruction),
        // matching upstream rather than the bare opcode + terminal.
        Unknown => super::undefined_instruction(ir),
    }
}

// --- Load/Store ---

// --- Multiply ---

// --- Misc ---

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
}
