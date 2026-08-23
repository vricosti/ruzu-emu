mod a32_translate;
pub mod asimd;
pub mod asimd_three_regs;
pub mod asimd_two_regs_misc;
pub mod asimd_two_regs_shift;
pub mod barrier;
pub mod branch;
pub mod conditional_state;
pub mod coprocessor;
pub mod data_processing;
pub mod divide;
pub mod exception;
pub mod extension;
pub mod helpers;
pub mod hint;
pub mod load_store;
pub mod load_store_multiple;
pub mod misc;
pub mod multiply;
pub mod packing;
pub mod reversal;
pub mod saturated;
pub mod status_register;
pub mod synchronization;
pub mod thumb16;
pub mod thumb32;
pub mod thumb32_control;
pub mod thumb32_coprocessor;
mod translate_arm;
pub mod translate_callbacks;
mod translate_thumb;
pub mod vfp;

use crate::frontend::a32::decoder::ArmInstId;
use crate::frontend::a32::types::Exception;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

pub use a32_translate::{translate, translate_single_instruction, TranslationOptions};

/// Matches upstream `TranslatorVisitor::RaiseException`.
pub(crate) fn raise_exception_with_instruction_size(
    ir: &mut A32IREmitter,
    exception: Exception,
    current_instruction_size: u32,
) -> bool {
    let location = ir.current_location.expect("current_location not set");
    ir.update_upper_location_descriptor();
    ir.branch_write_pc(Value::ImmU32(
        location.pc().wrapping_add(current_instruction_size),
    ));
    ir.exception_raised(exception);
    ir.set_term(Terminal::CheckHalt {
        else_: Box::new(Terminal::ReturnToDispatch),
    });
    false
}

/// ARM and Thumb32 instructions use a four-byte return-PC advance.
pub(crate) fn raise_exception(ir: &mut A32IREmitter, exception: Exception) -> bool {
    raise_exception_with_instruction_size(ir, exception, 4)
}

/// Matches upstream `TranslatorVisitor::UndefinedInstruction` (ARM / Thumb32).
pub(crate) fn undefined_instruction(ir: &mut A32IREmitter) -> bool {
    raise_exception(ir, Exception::UndefinedInstruction)
}

/// Matches upstream `TranslatorVisitor::UnpredictableInstruction` (ARM / Thumb32).
pub(crate) fn unpredictable_instruction(ir: &mut A32IREmitter) -> bool {
    raise_exception(ir, Exception::UnpredictableInstruction)
}

/// Matches upstream `TranslatorVisitor::DecodeError` (ARM / Thumb32).
pub(crate) fn decode_error(ir: &mut A32IREmitter) -> bool {
    raise_exception(ir, Exception::DecodeError)
}

#[cfg(test)]
mod tests {
    use super::translate_thumb::{
        convert_asimd_instruction, decode_thumb_vfp_or_asimd, read_thumb_instruction, ThumbInstSize,
    };
    use super::{translate, TranslationOptions};
    use crate::frontend::a32::decoder::ArmInstId;
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::frontend::a32::types::Exception;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;
    use crate::ir::terminal::Terminal;
    use crate::ir::value::Value;

    fn assert_no_execute_fault(
        block: &crate::ir::block::Block,
        expected_end: A32LocationDescriptor,
    ) {
        assert_eq!(block.end_location(), expected_end.to_location());
        assert!(matches!(
            &block.terminal,
            Terminal::CheckHalt { else_ }
                if matches!(else_.as_ref(), Terminal::ReturnToDispatch)
        ));
        let exception = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::A32ExceptionRaised)
            .expect("missing A32ExceptionRaised");
        assert_eq!(
            exception.args[1],
            Value::ImmU64(Exception::NoExecuteFault.as_u32() as u64)
        );
    }

    #[test]
    fn convert_asimd_instruction_uses_upstream_second_mask() {
        let thumb_instruction = 0xF910_0000;
        assert_eq!(convert_asimd_instruction(thumb_instruction), 0xF410_0000);
    }

    #[test]
    fn thumb_vfp_decode_precedes_generic_thumb32_coprocessor_decode() {
        assert_eq!(
            decode_thumb_vfp_or_asimd(0xEC42_3A1E).map(|decoded| decoded.id),
            Some(ArmInstId::VMOV_2u32_2f32)
        );
        assert_eq!(
            decode_thumb_vfp_or_asimd(0xEEF1_FA10).map(|decoded| decoded.id),
            Some(ArmInstId::VMRS)
        );
        assert!(decode_thumb_vfp_or_asimd(0xEC42_3F1E).is_none());
    }

    #[test]
    fn read_thumb_instruction_uses_upper_half_when_pc_is_word_plus_two() {
        let read_code = |addr: u32| match addr {
            0x1000 => Some(0xE12F_72F9),
            0x1004 => Some(0xD00C_E92D),
            _ => None,
        };

        let (instruction, size) = read_thumb_instruction(0x1002, &read_code).unwrap();
        assert_eq!(size, ThumbInstSize::Thumb16);
        assert_eq!(instruction, 0xE12F);

        let (instruction, size) = read_thumb_instruction(0x1006, &read_code).unwrap();
        assert_eq!(size, ThumbInstSize::Thumb16);
        assert_eq!(instruction, 0xD00C);
    }

    #[test]
    fn read_thumb32_instruction_crosses_word_boundary_correctly() {
        let read_code = |addr: u32| match addr {
            0x1004 => Some(0xE92D_4FF0),
            0x1008 => Some(0xE24D_D00C),
            _ => None,
        };

        let (instruction, size) = read_thumb_instruction(0x1006, &read_code).unwrap();
        assert_eq!(size, ThumbInstSize::Thumb32);
        assert_eq!(instruction, 0xE92D_D00C);
    }

    #[test]
    fn translate_arm_sets_end_location_from_current_location() {
        let loc = A32LocationDescriptor::new(0x1000, PSR::default(), FPSCR::default(), true);
        let read_code = |addr: u32| match addr {
            0x1000 => Some(0xE1A0_0000), // MOV r0, r0
            _ => None,
        };

        let block = translate(loc, &read_code, TranslationOptions::default());
        assert_eq!(block.end_location(), loc.advance_pc(4).to_location());
    }

    #[test]
    fn translate_thumb_sets_end_location_from_current_location() {
        let mut psr = PSR::default();
        psr.set_t(true);
        let loc = A32LocationDescriptor::new(0x1002, psr, FPSCR::default(), true);
        let read_code = |addr: u32| match addr {
            0x1000 => Some(0x0000_0000), // upper halfword at 0x1002 is Thumb16 LSL r0, r0, #0
            _ => None,
        };

        let block = translate(loc, &read_code, TranslationOptions::default());
        assert_eq!(block.end_location(), loc.advance_pc(2).to_location());
    }

    #[test]
    fn translate_arm_missing_code_raises_no_execute_fault() {
        let loc = A32LocationDescriptor::new(0x4000, PSR::default(), FPSCR::default(), false);
        let block = translate(loc, &|_| None, TranslationOptions::default());

        assert_no_execute_fault(&block, loc.advance_pc(4));
    }

    #[test]
    fn translate_thumb_missing_code_raises_no_execute_fault() {
        let mut psr = PSR::default();
        psr.set_t(true);
        let loc = A32LocationDescriptor::new(0x4000, psr, FPSCR::default(), false);
        let block = translate(loc, &|_| None, TranslationOptions::default());

        assert_no_execute_fault(&block, loc.advance_pc(2).advance_it());
    }
}

/// Translate a single ARM instruction. Returns true to continue translating.
fn translate_arm_instruction(
    ir: &mut A32IREmitter,
    decoded: &crate::frontend::a32::decoder::DecodedArm,
    options: TranslationOptions,
) -> bool {
    use ArmInstId::*;
    match decoded.id {
        // Data processing - immediate
        AND_imm | EOR_imm | SUB_imm | RSB_imm | ADD_imm | ADC_imm | SBC_imm | RSC_imm | TST_imm
        | TEQ_imm | CMP_imm | CMN_imm | ORR_imm | MOV_imm | BIC_imm | MVN_imm => {
            data_processing::arm_dp_imm(ir, decoded)
        }
        // Data processing - register
        AND_reg | EOR_reg | SUB_reg | RSB_reg | ADD_reg | ADC_reg | SBC_reg | RSC_reg | TST_reg
        | TEQ_reg | CMP_reg | CMN_reg | ORR_reg | MOV_reg | BIC_reg | MVN_reg => {
            data_processing::arm_dp_reg(ir, decoded)
        }
        // Data processing - register-shifted register
        AND_rsr | EOR_rsr | SUB_rsr | RSB_rsr | ADD_rsr | ADC_rsr | SBC_rsr | RSC_rsr | TST_rsr
        | TEQ_rsr | CMP_rsr | CMN_rsr | ORR_rsr | MOV_rsr | BIC_rsr | MVN_rsr => {
            data_processing::arm_dp_rsr(ir, decoded)
        }
        // Branch
        B => branch::arm_b(ir, decoded),
        BL => branch::arm_bl(ir, decoded),
        BX => branch::arm_bx(ir, decoded),
        BLX_reg => branch::arm_blx_reg(ir, decoded),
        BLX_imm => branch::arm_blx_imm(ir, decoded),
        // Load/Store
        LDR_lit => load_store::arm_ldr_lit(ir, decoded),
        LDR_imm => load_store::arm_ldr_imm(ir, decoded),
        LDR_reg => load_store::arm_ldr_reg(ir, decoded),
        STR_imm => load_store::arm_str_imm(ir, decoded),
        STR_reg => load_store::arm_str_reg(ir, decoded),
        LDRB_lit => load_store::arm_ldrb_lit(ir, decoded),
        LDRB_imm => load_store::arm_ldrb_imm(ir, decoded),
        LDRB_reg => load_store::arm_ldrb_reg(ir, decoded),
        STRB_imm => load_store::arm_strb_imm(ir, decoded),
        STRB_reg => load_store::arm_strb_reg(ir, decoded),
        LDRH_lit => load_store::arm_ldrh_lit(ir, decoded),
        LDRH_imm => load_store::arm_ldrh_imm(ir, decoded),
        LDRH_reg => load_store::arm_ldrh_reg(ir, decoded),
        STRH_imm => load_store::arm_strh_imm(ir, decoded),
        STRH_reg => load_store::arm_strh_reg(ir, decoded),
        LDRSB_lit => load_store::arm_ldrsb_lit(ir, decoded),
        LDRSB_imm => load_store::arm_ldrsb_imm(ir, decoded),
        LDRSB_reg => load_store::arm_ldrsb_reg(ir, decoded),
        LDRSH_lit => load_store::arm_ldrsh_lit(ir, decoded),
        LDRSH_imm => load_store::arm_ldrsh_imm(ir, decoded),
        LDRSH_reg => load_store::arm_ldrsh_reg(ir, decoded),
        LDRD_lit => load_store::arm_ldrd_lit(ir, decoded),
        LDRD_imm => load_store::arm_ldrd_imm(ir, decoded),
        LDRD_reg => load_store::arm_ldrd_reg(ir, decoded),
        STRD_imm => load_store::arm_strd_imm(ir, decoded),
        STRD_reg => load_store::arm_strd_reg(ir, decoded),
        // Load/Store multiple
        LDM | LDMDA | LDMDB | LDMIB => load_store_multiple::arm_ldm(ir, decoded),
        STM | STMDA | STMDB | STMIB => load_store_multiple::arm_stm(ir, decoded),
        // Multiply
        MUL => multiply::arm_mul(ir, decoded),
        MLA => multiply::arm_mla(ir, decoded),
        MLS => multiply::arm_mls(ir, decoded),
        UMULL => multiply::arm_umull(ir, decoded),
        UMLAL => multiply::arm_umlal(ir, decoded),
        SMULL => multiply::arm_smull(ir, decoded),
        SMLAL => multiply::arm_smlal(ir, decoded),
        UMAAL => multiply::arm_umaal(ir, decoded),
        SMLALxy => multiply::arm_smlalxy(ir, decoded),
        SMLAxy => multiply::arm_smlaxy(ir, decoded),
        SMULxy => multiply::arm_smulxy(ir, decoded),
        SMLAWy => multiply::arm_smlawy(ir, decoded),
        SMULWy => multiply::arm_smulwy(ir, decoded),
        SMMUL => multiply::arm_smmul(ir, decoded),
        SMMLA => multiply::arm_smmla(ir, decoded),
        SMMLS => multiply::arm_smmls(ir, decoded),
        SDIV => divide::arm_sdiv(ir, decoded),
        UDIV => divide::arm_udiv(ir, decoded),
        // Extensions
        SXTB => extension::arm_sxtb(ir, decoded),
        SXTH => extension::arm_sxth(ir, decoded),
        UXTB => extension::arm_uxtb(ir, decoded),
        UXTH => extension::arm_uxth(ir, decoded),
        SXTAB => extension::arm_sxtab(ir, decoded),
        SXTAH => extension::arm_sxtah(ir, decoded),
        UXTAB => extension::arm_uxtab(ir, decoded),
        UXTAH => extension::arm_uxtah(ir, decoded),
        SXTB16 => extension::arm_sxtb16(ir, decoded),
        SXTAB16 => extension::arm_sxtab16(ir, decoded),
        UXTB16 => extension::arm_uxtb16(ir, decoded),
        UXTAB16 => extension::arm_uxtab16(ir, decoded),
        // Misc
        NOP => true,
        CLZ => misc::arm_clz(ir, decoded),
        RBIT => reversal::arm_rbit(ir, decoded),
        REV => reversal::arm_rev(ir, decoded),
        REV16 => reversal::arm_rev16(ir, decoded),
        REVSH => reversal::arm_revsh(ir, decoded),
        MOVW => misc::arm_movw(ir, decoded),
        MOVT => misc::arm_movt(ir, decoded),
        BFC => misc::arm_bfc(ir, decoded),
        BFI => misc::arm_bfi(ir, decoded),
        SBFX => misc::arm_sbfx(ir, decoded),
        UBFX => misc::arm_ubfx(ir, decoded),
        SEL => {
            log::warn!("STUBBED SEL at PC={:#x}", ir.pc());
            true
        }
        // Saturated
        SSAT => saturated::arm_ssat(ir, decoded),
        USAT => saturated::arm_usat(ir, decoded),
        SSAT16 => saturated::arm_ssat16(ir, decoded),
        USAT16 => saturated::arm_usat16(ir, decoded),
        QADD => saturated::arm_qadd(ir, decoded),
        QSUB => saturated::arm_qsub(ir, decoded),
        QDADD => saturated::arm_qdadd(ir, decoded),
        QDSUB => saturated::arm_qdsub(ir, decoded),
        // Packing
        PKHBT => packing::arm_pkhbt(ir, decoded),
        PKHTB => packing::arm_pkhtb(ir, decoded),
        // Coprocessor
        MCR => coprocessor::arm_mcr(ir, decoded),
        MRC => coprocessor::arm_mrc(ir, decoded),
        CDP => coprocessor::arm_cdp(ir, decoded),
        MRRC => coprocessor::arm_mrrc(ir, decoded),
        MCRR => coprocessor::arm_mcrr(ir, decoded),
        LDC => coprocessor::arm_ldc(ir, decoded),
        STC => coprocessor::arm_stc(ir, decoded),
        // Synchronization
        STL => synchronization::arm_stl(ir, decoded),
        STLEX => synchronization::arm_stlex(ir, decoded),
        LDREX => synchronization::arm_ldrex(ir, decoded),
        LDA => synchronization::arm_lda(ir, decoded),
        LDAEX => synchronization::arm_ldaex(ir, decoded),
        LDREXB => synchronization::arm_ldrexb(ir, decoded),
        LDAB => synchronization::arm_ldab(ir, decoded),
        LDAEXB => synchronization::arm_ldaexb(ir, decoded),
        LDREXH => synchronization::arm_ldrexh(ir, decoded),
        LDAH => synchronization::arm_ldah(ir, decoded),
        LDAEXH => synchronization::arm_ldaexh(ir, decoded),
        LDREXD => synchronization::arm_ldrexd(ir, decoded),
        LDAEXD => synchronization::arm_ldaexd(ir, decoded),
        STREX => synchronization::arm_strex(ir, decoded),
        STLEXB => synchronization::arm_stlexb(ir, decoded),
        STREXB => synchronization::arm_strexb(ir, decoded),
        STLEXH => synchronization::arm_stlexh(ir, decoded),
        STREXH => synchronization::arm_strexh(ir, decoded),
        STLEXD => synchronization::arm_stlexd(ir, decoded),
        STREXD => synchronization::arm_strexd(ir, decoded),
        STLB => synchronization::arm_stlb(ir, decoded),
        STLH => synchronization::arm_stlh(ir, decoded),
        SWP => synchronization::arm_swp(ir, decoded),
        SWPB => synchronization::arm_swpb(ir, decoded),
        CLREX => synchronization::arm_clrex(ir),
        // Status register
        MRS => status_register::arm_mrs(ir, decoded),
        MSR_imm => status_register::arm_msr_imm(ir, decoded),
        MSR_reg => status_register::arm_msr_reg(ir, decoded),
        // Barriers
        DMB => barrier::arm_dmb(ir),
        DSB => barrier::arm_dsb(ir),
        ISB => barrier::arm_isb(ir),
        // Exception
        SVC => exception::arm_svc(ir, decoded),
        UDF => exception::arm_udf(ir, decoded),
        BKPT => exception::arm_bkpt(ir, decoded, options),
        // VFP three-register data processing
        VMLA_fp => vfp::arm_vmla_fp(ir, decoded),
        VMLS_fp => vfp::arm_vmls_fp(ir, decoded),
        VNMLS_fp => vfp::arm_vnmls_fp(ir, decoded),
        VNMLA_fp => vfp::arm_vnmla_fp(ir, decoded),
        VADD_fp => vfp::arm_vadd_fp(ir, decoded),
        VSUB_fp => vfp::arm_vsub_fp(ir, decoded),
        VMUL_fp => vfp::arm_vmul_fp(ir, decoded),
        VNMUL_fp => vfp::arm_vnmul_fp(ir, decoded),
        VDIV_fp => vfp::arm_vdiv_fp(ir, decoded),
        VFMA_fp => vfp::arm_vfma_fp(ir, decoded),
        VFMS_fp => vfp::arm_vfms_fp(ir, decoded),
        VFNMA_fp => vfp::arm_vfnma_fp(ir, decoded),
        VFNMS_fp => vfp::arm_vfnms_fp(ir, decoded),
        VSEL_fp => vfp::arm_vsel_fp(ir, decoded),
        VMAXNM_fp => vfp::arm_vmaxnm_fp(ir, decoded),
        VMINNM_fp => vfp::arm_vminnm_fp(ir, decoded),
        // VFP unary data processing
        VMOV_fp_reg => vfp::arm_vmov_fp_reg(ir, decoded),
        VMOV_fp_imm => vfp::arm_vmov_fp_imm(ir, decoded),
        VABS_fp => vfp::arm_vabs_fp(ir, decoded),
        VNEG_fp => vfp::arm_vneg_fp(ir, decoded),
        VSQRT_fp => vfp::arm_vsqrt_fp(ir, decoded),
        VCMP_fp => vfp::arm_vcmp_fp(ir, decoded),
        VCMP_zero_fp => vfp::arm_vcmp_zero_fp(ir, decoded),
        VCVT_f_to_f => vfp::arm_vcvt_f_to_f(ir, decoded),
        VCVT_from_int => vfp::arm_vcvt_from_int(ir, decoded),
        VCVT_to_u32 => vfp::arm_vcvt_to_u32(ir, decoded),
        VCVT_to_s32 => vfp::arm_vcvt_to_s32(ir, decoded),
        // VFP core register moves
        VMOV_u32_f64 => vfp::arm_vmov_u32_f64(ir, decoded),
        VMOV_f64_u32 => vfp::arm_vmov_f64_u32(ir, decoded),
        VMOV_u32_f32 => vfp::arm_vmov_u32_f32(ir, decoded),
        VMOV_f32_u32 => vfp::arm_vmov_f32_u32(ir, decoded),
        VMOV_2u32_2f32 => vfp::vfp_vmov_2u32_2f32(ir, decoded.raw),
        VMOV_2f32_2u32 => vfp::vfp_vmov_2f32_2u32(ir, decoded.raw),
        VMOV_2u32_f64 => vfp::vfp_vmov_2u32_f64(ir, decoded.raw),
        VMOV_f64_2u32 => vfp::vfp_vmov_f64_2u32(ir, decoded.raw),
        VMOV_from_i32 => vfp::arm_vmov_from_i32(ir, decoded),
        VMOV_to_i32 => vfp::arm_vmov_to_i32(ir, decoded),
        VMSR => vfp::vfp_vmsr(ir, decoded.raw),
        VMRS => vfp::vfp_vmrs(ir, decoded.raw),
        VFP_VDUP => vfp::arm_vdup(ir, decoded),
        VFP_VRINT_rm => vfp::arm_vfp_vrint_rm(ir, decoded),
        VFP_VCVT_rm => vfp::arm_vfp_vcvt_rm(ir, decoded),
        // VFP load/store
        VPUSH => vfp::arm_vpush(ir, decoded),
        VPOP => vfp::arm_vpop(ir, decoded),
        VLDR_fp => vfp::arm_vldr_fp(ir, decoded),
        VSTR_fp => vfp::arm_vstr_fp(ir, decoded),
        VSTM => vfp::arm_vstm(ir, decoded),
        VLDM => vfp::arm_vldm(ir, decoded),
        // ASIMD
        ASIMD_VMOV_imm => asimd::arm_asimd_vmov_imm(ir, decoded),
        ASIMD_VMOVN => asimd_two_regs_misc::arm_asimd_vmovn(ir, decoded),
        // ASIMD three-register same (integer)
        ASIMD_VHADD => asimd_three_regs::arm_asimd_vhadd(ir, decoded),
        ASIMD_VQADD => asimd_three_regs::arm_asimd_vqadd(ir, decoded),
        ASIMD_VRHADD => asimd_three_regs::arm_asimd_vrhadd(ir, decoded),
        ASIMD_VHSUB => asimd_three_regs::arm_asimd_vhsub(ir, decoded),
        ASIMD_VQSUB => asimd_three_regs::arm_asimd_vqsub(ir, decoded),
        ASIMD_VCGT_reg_int => asimd_three_regs::arm_asimd_vcgt_reg_int(ir, decoded),
        ASIMD_VCGE_reg_int => asimd_three_regs::arm_asimd_vcge_reg_int(ir, decoded),
        ASIMD_VSHL_reg => asimd_three_regs::arm_asimd_vshl_reg(ir, decoded),
        ASIMD_VQSHL_reg => asimd_three_regs::arm_asimd_vqshl_reg(ir, decoded),
        ASIMD_VRSHL => asimd_three_regs::arm_asimd_vrshl(ir, decoded),
        ASIMD_VMAX_int => asimd_three_regs::arm_asimd_vmax_min_int(ir, decoded),
        ASIMD_VMIN_int => asimd_three_regs::arm_asimd_vmax_min_int(ir, decoded),
        ASIMD_VABD_int => asimd_three_regs::arm_asimd_vabd_int(ir, decoded),
        ASIMD_VABA => asimd_three_regs::arm_asimd_vaba(ir, decoded),
        ASIMD_VADD_int => asimd_three_regs::arm_asimd_vadd_int(ir, decoded),
        ASIMD_VSUB_int => asimd_three_regs::arm_asimd_vsub_int(ir, decoded),
        ASIMD_VTST => asimd_three_regs::arm_asimd_vtst(ir, decoded),
        ASIMD_VCEQ_reg_int => asimd_three_regs::arm_asimd_vceq_reg_int(ir, decoded),
        ASIMD_VMLA_int => asimd_three_regs::arm_asimd_vmla_int(ir, decoded),
        ASIMD_VMUL_int => asimd_three_regs::arm_asimd_vmul_int(ir, decoded),
        ASIMD_VPMAX_int => asimd_three_regs::arm_asimd_vpmax_int(ir, decoded),
        ASIMD_VQDMULH => asimd_three_regs::arm_asimd_vqdmulh(ir, decoded),
        ASIMD_VQRDMULH => asimd_three_regs::arm_asimd_vqrdmulh(ir, decoded),
        ASIMD_VPADD_int => asimd_three_regs::arm_asimd_vpadd_int(ir, decoded),
        // ASIMD three-register same (bitwise)
        ASIMD_VAND_reg => asimd_three_regs::arm_asimd_vand_reg(ir, decoded),
        ASIMD_VBIC_reg => asimd_three_regs::arm_asimd_vbic_reg(ir, decoded),
        ASIMD_VORR_reg => asimd_three_regs::arm_asimd_vorr_reg(ir, decoded),
        ASIMD_VORN_reg => asimd_three_regs::arm_asimd_vorn_reg(ir, decoded),
        ASIMD_VEOR_reg => asimd_three_regs::arm_asimd_veor_reg(ir, decoded),
        ASIMD_VBSL => asimd_three_regs::arm_asimd_vbsl(ir, decoded),
        ASIMD_VBIT => asimd_three_regs::arm_asimd_vbit(ir, decoded),
        ASIMD_VBIF => asimd_three_regs::arm_asimd_vbif(ir, decoded),
        // ASIMD three-register same (float)
        ASIMD_VFMA => asimd_three_regs::arm_asimd_vfma(ir, decoded),
        ASIMD_VFMS => asimd_three_regs::arm_asimd_vfms(ir, decoded),
        ASIMD_VADD_float => asimd_three_regs::arm_asimd_vadd_float(ir, decoded),
        ASIMD_VSUB_float => asimd_three_regs::arm_asimd_vsub_float(ir, decoded),
        ASIMD_VMLA_float => asimd_three_regs::arm_asimd_vmla_float(ir, decoded),
        ASIMD_VMLS_float => asimd_three_regs::arm_asimd_vmls_float(ir, decoded),
        ASIMD_VPADD_float => asimd_three_regs::arm_asimd_vpadd_float(ir, decoded),
        ASIMD_VABD_float => asimd_three_regs::arm_asimd_vabd_float(ir, decoded),
        ASIMD_VMUL_float => asimd_three_regs::arm_asimd_vmul_float(ir, decoded),
        ASIMD_VCEQ_reg_float => asimd_three_regs::arm_asimd_vceq_reg_float(ir, decoded),
        ASIMD_VCGE_reg_float => asimd_three_regs::arm_asimd_vcge_reg_float(ir, decoded),
        ASIMD_VCGT_reg_float => asimd_three_regs::arm_asimd_vcgt_reg_float(ir, decoded),
        ASIMD_VACGE => asimd_three_regs::arm_asimd_vacge(ir, decoded),
        ASIMD_VMAX_float => asimd_three_regs::arm_asimd_vmax_float(ir, decoded),
        ASIMD_VMIN_float => asimd_three_regs::arm_asimd_vmin_float(ir, decoded),
        ASIMD_VPMAX_float => asimd_three_regs::arm_asimd_vpmax_float(ir, decoded),
        ASIMD_VPMIN_float => asimd_three_regs::arm_asimd_vpmin_float(ir, decoded),
        ASIMD_VRECPS => asimd_three_regs::arm_asimd_vrecps(ir, decoded),
        ASIMD_VRSQRTS => asimd_three_regs::arm_asimd_vrsqrts(ir, decoded),
        // ASIMD three registers of different length
        ASIMD_VADDL => asimd_three_regs::arm_asimd_vaddl(ir, decoded),
        ASIMD_VSUBL => asimd_three_regs::arm_asimd_vsubl(ir, decoded),
        ASIMD_VABAL => asimd_three_regs::arm_asimd_vabal(ir, decoded),
        ASIMD_VABDL => asimd_three_regs::arm_asimd_vabdl(ir, decoded),
        ASIMD_VMLAL => asimd_three_regs::arm_asimd_vmlal(ir, decoded),
        ASIMD_VMULL => asimd_three_regs::arm_asimd_vmull(ir, decoded),
        // ASIMD scalar
        ASIMD_VMLA_scalar => asimd::arm_asimd_vmla_scalar(ir, decoded),
        ASIMD_VMUL_scalar => asimd::arm_asimd_vmul_scalar(ir, decoded),
        ASIMD_VDUP_scalar => asimd::arm_asimd_vdup_scalar(ir, decoded),
        ASIMD_VCVT_integer => asimd::arm_asimd_vcvt_integer(ir, decoded),
        ASIMD_VTRN => asimd::arm_asimd_vtrn(ir, decoded),
        ASIMD_VTBL => asimd::arm_asimd_vtbl(ir, decoded),
        ASIMD_VTBX => asimd::arm_asimd_vtbx(ir, decoded),
        ASIMD_SHR => asimd_two_regs_shift::arm_asimd_shr(ir, decoded),
        ASIMD_SRA => asimd_two_regs_shift::arm_asimd_sra(ir, decoded),
        ASIMD_VSHRN => asimd_two_regs_shift::arm_asimd_vshrn(ir, decoded),
        ASIMD_VSHL_imm => asimd_two_regs_shift::arm_asimd_vshl_imm(ir, decoded),
        ASIMD_VSLI => asimd_two_regs_shift::arm_asimd_vsli(ir, decoded),
        ASIMD_VSRI => asimd_two_regs_shift::arm_asimd_vsri(ir, decoded),
        ASIMD_VQSHL_imm => asimd_two_regs_shift::arm_asimd_vqshl_imm(ir, decoded),
        V8_VST_multiple => asimd::arm_v8_vst_multiple(ir, decoded),
        V8_VLD_multiple => asimd::arm_v8_vld_multiple(ir, decoded),
        V8_VST_single => asimd::arm_v8_vst_single(ir, decoded),
        V8_VLD_single => asimd::arm_v8_vld_single(ir, decoded),
        V8_VLD_all_lanes => asimd::arm_v8_vld_all_lanes(ir, decoded),
        ASIMD_VEXT => asimd::arm_asimd_vext(ir, decoded),
        ASIMD_VUZP => asimd::arm_asimd_vuzp(ir, decoded),
        ASIMD_VZIP => asimd::arm_asimd_vzip(ir, decoded),
        ASIMD_VCGT_zero => asimd::arm_asimd_vcgt_zero(ir, decoded),
        ASIMD_VCGE_zero => asimd::arm_asimd_vcge_zero(ir, decoded),
        ASIMD_VCEQ_zero => asimd::arm_asimd_vceq_zero(ir, decoded),
        ASIMD_VCLE_zero => asimd::arm_asimd_vcle_zero(ir, decoded),
        ASIMD_VCLT_zero => asimd::arm_asimd_vclt_zero(ir, decoded),
        ASIMD_VRECPE => asimd::arm_asimd_vrecpe(ir, decoded),
        ASIMD_VRSQRTE => asimd::arm_asimd_vrsqrte(ir, decoded),
        ASIMD_VNEG_int => asimd::arm_asimd_vneg_int(ir, decoded),
        ASIMD_VABS_int => asimd::arm_asimd_vabs_int(ir, decoded),
        // Hints
        PLD_imm | PLD_reg => hint::arm_pld(ir, decoded, options),
        SEV => hint::arm_sev(ir, options),
        SEVL => hint::arm_sevl(ir, options),
        WFI => hint::arm_wfi(ir, options),
        WFE => hint::arm_wfe(ir, options),
        YIELD => hint::arm_yield(ir, options),
        // An unmatched encoding is treated as undefined, matching upstream's
        // behaviour where any bit pattern not claimed by a decode-table entry
        // raises UndefinedInstruction. Use the shared helper so the full
        // RaiseException lifecycle (UpdateUpperLocationDescriptor +
        // BranchWritePC(PC+4)) runs, not just the bare opcode + terminal.
        Unknown => undefined_instruction(ir),
    }
}

/// Translate a single Thumb16 instruction. Returns true to continue translating.
fn translate_thumb16_instruction(
    ir: &mut A32IREmitter,
    decoded: &crate::frontend::a32::decoder_thumb16::DecodedThumb16,
    options: TranslationOptions,
) -> bool {
    thumb16::translate_thumb16(ir, decoded, options)
}

/// Translate a single Thumb32 instruction. Returns true to continue translating.
fn translate_thumb32_instruction(
    ir: &mut A32IREmitter,
    decoded: &crate::frontend::a32::decoder_thumb32::DecodedThumb32,
    options: TranslationOptions,
) -> bool {
    thumb32::translate_thumb32(ir, decoded, options)
}
