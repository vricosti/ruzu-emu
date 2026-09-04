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
pub mod thumb32_branch;
pub mod thumb32_control;
pub mod thumb32_coprocessor;
pub mod thumb32_data_processing_modified_immediate;
pub mod thumb32_data_processing_plain_binary_immediate;
pub mod thumb32_data_processing_register;
pub mod thumb32_data_processing_shifted_register;
pub mod thumb32_load_byte;
pub mod thumb32_load_halfword;
pub mod thumb32_load_store_dual;
pub mod thumb32_load_store_multiple;
pub mod thumb32_load_word;
pub mod thumb32_long_multiply;
pub mod thumb32_misc;
pub mod thumb32_multiply;
pub mod thumb32_parallel;
pub mod thumb32_store_single_data_item;
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

/// Result of Eden's immediate-expansion helpers.
pub(crate) struct ImmAndCarry {
    pub imm32: u32,
    pub carry: Value,
}

/// Matches `TranslatorVisitor::ThumbExpandImm_C` from `a32_translate_impl.h`.
pub(crate) fn thumb_expand_imm_c(imm12: u32, carry_in: Value) -> ImmAndCarry {
    if (imm12 >> 10) & 3 == 0 {
        let imm8 = imm12 & 0xff;
        let imm32 = match (imm12 >> 8) & 3 {
            0b00 => imm8,
            0b01 => (imm8 << 16) | imm8,
            0b10 => (imm8 << 24) | (imm8 << 8),
            0b11 => imm8 * 0x0101_0101,
            _ => unreachable!(),
        };
        return ImmAndCarry {
            imm32,
            carry: carry_in,
        };
    }

    let imm32 = (0x80 | (imm12 & 0x7f)).rotate_right((imm12 >> 7) & 0x1f);
    ImmAndCarry {
        imm32,
        carry: Value::ImmU1(imm32 & (1 << 31) != 0),
    }
}

/// Matches `TranslatorVisitor::ThumbExpandImm` from `a32_translate_impl.h`.
pub(crate) fn thumb_expand_imm(imm12: u32) -> u32 {
    thumb_expand_imm_c(imm12, Value::ImmU1(false)).imm32
}

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
mod immediate_tests {
    use super::{thumb_expand_imm, thumb_expand_imm_c};
    use crate::ir::value::{InstRef, Value};

    fn reference_thumb_expand_imm(imm12: u32) -> u32 {
        if imm12 & 0xc00 == 0 {
            let imm8 = imm12 & 0xff;
            return match imm12 & 0x300 {
                0x000 => imm8,
                0x100 => (imm8 << 16) | imm8,
                0x200 => (imm8 << 24) | (imm8 << 8),
                0x300 => (imm8 << 24) | (imm8 << 16) | (imm8 << 8) | imm8,
                _ => unreachable!(),
            };
        }
        (0x80 | (imm12 & 0x7f)).rotate_right((imm12 >> 7) & 31)
    }

    #[test]
    fn thumb_expand_imm_matches_all_twelve_bit_inputs() {
        for imm12 in 0..=0xfff {
            assert_eq!(thumb_expand_imm(imm12), reference_thumb_expand_imm(imm12));
        }
    }

    #[test]
    fn thumb_expand_imm_c_preserves_dynamic_carry_only_for_replication_forms() {
        let dynamic_carry = Value::Inst(InstRef(42));
        for imm12 in 0..=0x3ff {
            let expanded = thumb_expand_imm_c(imm12, dynamic_carry);
            assert_eq!(expanded.carry, dynamic_carry, "imm12={imm12:03X}");
        }

        for imm12 in 0x400..=0xfff {
            let expanded = thumb_expand_imm_c(imm12, dynamic_carry);
            assert_eq!(
                expanded.carry,
                Value::ImmU1(expanded.imm32 & (1 << 31) != 0),
                "imm12={imm12:03X}"
            );
        }
    }
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
            Some(ArmInstId::Vmov2u32_2f32)
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
        AndImm | EorImm | SubImm | RsbImm | AddImm | AdcImm | SbcImm | RscImm | TstImm | TeqImm
        | CmpImm | CmnImm | OrrImm | MovImm | BicImm | MvnImm => {
            data_processing::arm_dp_imm(ir, decoded)
        }
        // Data processing - register
        AndReg | EorReg | SubReg | RsbReg | AddReg | AdcReg | SbcReg | RscReg | TstReg | TeqReg
        | CmpReg | CmnReg | OrrReg | MovReg | BicReg | MvnReg => {
            data_processing::arm_dp_reg(ir, decoded)
        }
        // Data processing - register-shifted register
        AndRsr | EorRsr | SubRsr | RsbRsr | AddRsr | AdcRsr | SbcRsr | RscRsr | TstRsr | TeqRsr
        | CmpRsr | CmnRsr | OrrRsr | MovRsr | BicRsr | MvnRsr => {
            data_processing::arm_dp_rsr(ir, decoded)
        }
        // Branch
        B => branch::arm_b(ir, decoded),
        BL => branch::arm_bl(ir, decoded),
        BX => branch::arm_bx(ir, decoded),
        BlxReg => branch::arm_blx_reg(ir, decoded),
        BlxImm => branch::arm_blx_imm(ir, decoded),
        // Load/Store
        LdrLit => load_store::arm_ldr_lit(ir, decoded),
        LdrImm => load_store::arm_ldr_imm(ir, decoded),
        LdrReg => load_store::arm_ldr_reg(ir, decoded),
        StrImm => load_store::arm_str_imm(ir, decoded),
        StrReg => load_store::arm_str_reg(ir, decoded),
        LdrbLit => load_store::arm_ldrb_lit(ir, decoded),
        LdrbImm => load_store::arm_ldrb_imm(ir, decoded),
        LdrbReg => load_store::arm_ldrb_reg(ir, decoded),
        StrbImm => load_store::arm_strb_imm(ir, decoded),
        StrbReg => load_store::arm_strb_reg(ir, decoded),
        LdrhLit => load_store::arm_ldrh_lit(ir, decoded),
        LdrhImm => load_store::arm_ldrh_imm(ir, decoded),
        LdrhReg => load_store::arm_ldrh_reg(ir, decoded),
        StrhImm => load_store::arm_strh_imm(ir, decoded),
        StrhReg => load_store::arm_strh_reg(ir, decoded),
        LdrsbLit => load_store::arm_ldrsb_lit(ir, decoded),
        LdrsbImm => load_store::arm_ldrsb_imm(ir, decoded),
        LdrsbReg => load_store::arm_ldrsb_reg(ir, decoded),
        LdrshLit => load_store::arm_ldrsh_lit(ir, decoded),
        LdrshImm => load_store::arm_ldrsh_imm(ir, decoded),
        LdrshReg => load_store::arm_ldrsh_reg(ir, decoded),
        LdrdLit => load_store::arm_ldrd_lit(ir, decoded),
        LdrdImm => load_store::arm_ldrd_imm(ir, decoded),
        LdrdReg => load_store::arm_ldrd_reg(ir, decoded),
        StrdImm => load_store::arm_strd_imm(ir, decoded),
        StrdReg => load_store::arm_strd_reg(ir, decoded),
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
        MsrImm => status_register::arm_msr_imm(ir, decoded),
        MsrReg => status_register::arm_msr_reg(ir, decoded),
        // Barriers
        DMB => barrier::arm_dmb(ir),
        DSB => barrier::arm_dsb(ir),
        ISB => barrier::arm_isb(ir),
        // Exception
        SVC => exception::arm_svc(ir, decoded),
        UDF => exception::arm_udf(ir, decoded),
        BKPT => exception::arm_bkpt(ir, decoded, options),
        // VFP three-register data processing
        VmlaFp => vfp::arm_vmla_fp(ir, decoded),
        VmlsFp => vfp::arm_vmls_fp(ir, decoded),
        VnmlsFp => vfp::arm_vnmls_fp(ir, decoded),
        VnmlaFp => vfp::arm_vnmla_fp(ir, decoded),
        VaddFp => vfp::arm_vadd_fp(ir, decoded),
        VsubFp => vfp::arm_vsub_fp(ir, decoded),
        VmulFp => vfp::arm_vmul_fp(ir, decoded),
        VnmulFp => vfp::arm_vnmul_fp(ir, decoded),
        VdivFp => vfp::arm_vdiv_fp(ir, decoded),
        VfmaFp => vfp::arm_vfma_fp(ir, decoded),
        VfmsFp => vfp::arm_vfms_fp(ir, decoded),
        VfnmaFp => vfp::arm_vfnma_fp(ir, decoded),
        VfnmsFp => vfp::arm_vfnms_fp(ir, decoded),
        VselFp => vfp::arm_vsel_fp(ir, decoded),
        VmaxnmFp => vfp::arm_vmaxnm_fp(ir, decoded),
        VminnmFp => vfp::arm_vminnm_fp(ir, decoded),
        // VFP unary data processing
        VmovFpReg => vfp::arm_vmov_fp_reg(ir, decoded),
        VmovFpImm => vfp::arm_vmov_fp_imm(ir, decoded),
        VabsFp => vfp::arm_vabs_fp(ir, decoded),
        VnegFp => vfp::arm_vneg_fp(ir, decoded),
        VsqrtFp => vfp::arm_vsqrt_fp(ir, decoded),
        VcmpFp => vfp::arm_vcmp_fp(ir, decoded),
        VcmpZeroFp => vfp::arm_vcmp_zero_fp(ir, decoded),
        VcvtFToF => vfp::arm_vcvt_f_to_f(ir, decoded),
        VcvtFromInt => vfp::arm_vcvt_from_int(ir, decoded),
        VcvtToU32 => vfp::arm_vcvt_to_u32(ir, decoded),
        VcvtToS32 => vfp::arm_vcvt_to_s32(ir, decoded),
        // VFP core register moves
        VmovU32F64 => vfp::arm_vmov_u32_f64(ir, decoded),
        VmovF64U32 => vfp::arm_vmov_f64_u32(ir, decoded),
        VmovU32F32 => vfp::arm_vmov_u32_f32(ir, decoded),
        VmovF32U32 => vfp::arm_vmov_f32_u32(ir, decoded),
        Vmov2u32_2f32 => vfp::vfp_vmov_2u32_2f32(ir, decoded.raw),
        Vmov2f32_2u32 => vfp::vfp_vmov_2f32_2u32(ir, decoded.raw),
        Vmov2u32F64 => vfp::vfp_vmov_2u32_f64(ir, decoded.raw),
        VmovF64_2u32 => vfp::vfp_vmov_f64_2u32(ir, decoded.raw),
        VmovFromI32 => vfp::arm_vmov_from_i32(ir, decoded),
        VmovToI32 => vfp::arm_vmov_to_i32(ir, decoded),
        VMSR => vfp::vfp_vmsr(ir, decoded.raw),
        VMRS => vfp::vfp_vmrs(ir, decoded.raw),
        VfpVdup => vfp::arm_vdup(ir, decoded),
        VfpVrintRm => vfp::arm_vfp_vrint_rm(ir, decoded),
        VfpVcvtRm => vfp::arm_vfp_vcvt_rm(ir, decoded),
        // VFP load/store
        VPUSH => vfp::arm_vpush(ir, decoded),
        VPOP => vfp::arm_vpop(ir, decoded),
        VldrFp => vfp::arm_vldr_fp(ir, decoded),
        VstrFp => vfp::arm_vstr_fp(ir, decoded),
        VSTM => vfp::arm_vstm(ir, decoded),
        VLDM => vfp::arm_vldm(ir, decoded),
        // ASIMD
        AsimdVmovImm => asimd::arm_asimd_vmov_imm(ir, decoded),
        AsimdVmovn => asimd_two_regs_misc::arm_asimd_vmovn(ir, decoded),
        // ASIMD three-register same (integer)
        AsimdVhadd => asimd_three_regs::arm_asimd_vhadd(ir, decoded),
        AsimdVqadd => asimd_three_regs::arm_asimd_vqadd(ir, decoded),
        AsimdVrhadd => asimd_three_regs::arm_asimd_vrhadd(ir, decoded),
        AsimdVhsub => asimd_three_regs::arm_asimd_vhsub(ir, decoded),
        AsimdVqsub => asimd_three_regs::arm_asimd_vqsub(ir, decoded),
        AsimdVcgtRegInt => asimd_three_regs::arm_asimd_vcgt_reg_int(ir, decoded),
        AsimdVcgeRegInt => asimd_three_regs::arm_asimd_vcge_reg_int(ir, decoded),
        AsimdVshlReg => asimd_three_regs::arm_asimd_vshl_reg(ir, decoded),
        AsimdVqshlReg => asimd_three_regs::arm_asimd_vqshl_reg(ir, decoded),
        AsimdVrshl => asimd_three_regs::arm_asimd_vrshl(ir, decoded),
        AsimdVmaxInt => asimd_three_regs::arm_asimd_vmax_min_int(ir, decoded),
        AsimdVminInt => asimd_three_regs::arm_asimd_vmax_min_int(ir, decoded),
        AsimdVabdInt => asimd_three_regs::arm_asimd_vabd_int(ir, decoded),
        AsimdVaba => asimd_three_regs::arm_asimd_vaba(ir, decoded),
        AsimdVaddInt => asimd_three_regs::arm_asimd_vadd_int(ir, decoded),
        AsimdVsubInt => asimd_three_regs::arm_asimd_vsub_int(ir, decoded),
        AsimdVtst => asimd_three_regs::arm_asimd_vtst(ir, decoded),
        AsimdVceqRegInt => asimd_three_regs::arm_asimd_vceq_reg_int(ir, decoded),
        AsimdVmlaInt => asimd_three_regs::arm_asimd_vmla_int(ir, decoded),
        AsimdVmulInt => asimd_three_regs::arm_asimd_vmul_int(ir, decoded),
        AsimdVpmaxInt => asimd_three_regs::arm_asimd_vpmax_int(ir, decoded),
        AsimdVqdmulh => asimd_three_regs::arm_asimd_vqdmulh(ir, decoded),
        AsimdVqrdmulh => asimd_three_regs::arm_asimd_vqrdmulh(ir, decoded),
        AsimdVpaddInt => asimd_three_regs::arm_asimd_vpadd_int(ir, decoded),
        // ASIMD three-register same (bitwise)
        AsimdVandReg => asimd_three_regs::arm_asimd_vand_reg(ir, decoded),
        AsimdVbicReg => asimd_three_regs::arm_asimd_vbic_reg(ir, decoded),
        AsimdVorrReg => asimd_three_regs::arm_asimd_vorr_reg(ir, decoded),
        AsimdVornReg => asimd_three_regs::arm_asimd_vorn_reg(ir, decoded),
        AsimdVeorReg => asimd_three_regs::arm_asimd_veor_reg(ir, decoded),
        AsimdVbsl => asimd_three_regs::arm_asimd_vbsl(ir, decoded),
        AsimdVbit => asimd_three_regs::arm_asimd_vbit(ir, decoded),
        AsimdVbif => asimd_three_regs::arm_asimd_vbif(ir, decoded),
        // ASIMD three-register same (float)
        AsimdVfma => asimd_three_regs::arm_asimd_vfma(ir, decoded),
        AsimdVfms => asimd_three_regs::arm_asimd_vfms(ir, decoded),
        AsimdVaddFloat => asimd_three_regs::arm_asimd_vadd_float(ir, decoded),
        AsimdVsubFloat => asimd_three_regs::arm_asimd_vsub_float(ir, decoded),
        AsimdVmlaFloat => asimd_three_regs::arm_asimd_vmla_float(ir, decoded),
        AsimdVmlsFloat => asimd_three_regs::arm_asimd_vmls_float(ir, decoded),
        AsimdVpaddFloat => asimd_three_regs::arm_asimd_vpadd_float(ir, decoded),
        AsimdVabdFloat => asimd_three_regs::arm_asimd_vabd_float(ir, decoded),
        AsimdVmulFloat => asimd_three_regs::arm_asimd_vmul_float(ir, decoded),
        AsimdVceqRegFloat => asimd_three_regs::arm_asimd_vceq_reg_float(ir, decoded),
        AsimdVcgeRegFloat => asimd_three_regs::arm_asimd_vcge_reg_float(ir, decoded),
        AsimdVcgtRegFloat => asimd_three_regs::arm_asimd_vcgt_reg_float(ir, decoded),
        AsimdVacge => asimd_three_regs::arm_asimd_vacge(ir, decoded),
        AsimdVmaxFloat => asimd_three_regs::arm_asimd_vmax_float(ir, decoded),
        AsimdVminFloat => asimd_three_regs::arm_asimd_vmin_float(ir, decoded),
        AsimdVpmaxFloat => asimd_three_regs::arm_asimd_vpmax_float(ir, decoded),
        AsimdVpminFloat => asimd_three_regs::arm_asimd_vpmin_float(ir, decoded),
        AsimdVrecps => asimd_three_regs::arm_asimd_vrecps(ir, decoded),
        AsimdVrsqrts => asimd_three_regs::arm_asimd_vrsqrts(ir, decoded),
        // ASIMD three registers of different length
        AsimdVaddl => asimd_three_regs::arm_asimd_vaddl(ir, decoded),
        AsimdVsubl => asimd_three_regs::arm_asimd_vsubl(ir, decoded),
        AsimdVabal => asimd_three_regs::arm_asimd_vabal(ir, decoded),
        AsimdVabdl => asimd_three_regs::arm_asimd_vabdl(ir, decoded),
        AsimdVmlal => asimd_three_regs::arm_asimd_vmlal(ir, decoded),
        AsimdVmull => asimd_three_regs::arm_asimd_vmull(ir, decoded),
        // ASIMD scalar
        AsimdVmlaScalar => asimd::arm_asimd_vmla_scalar(ir, decoded),
        AsimdVmulScalar => asimd::arm_asimd_vmul_scalar(ir, decoded),
        AsimdVdupScalar => asimd::arm_asimd_vdup_scalar(ir, decoded),
        AsimdVcvtInteger => asimd::arm_asimd_vcvt_integer(ir, decoded),
        AsimdVtrn => asimd::arm_asimd_vtrn(ir, decoded),
        AsimdVtbl => asimd::arm_asimd_vtbl(ir, decoded),
        AsimdVtbx => asimd::arm_asimd_vtbx(ir, decoded),
        AsimdShr => asimd_two_regs_shift::arm_asimd_shr(ir, decoded),
        AsimdSra => asimd_two_regs_shift::arm_asimd_sra(ir, decoded),
        AsimdVshrn => asimd_two_regs_shift::arm_asimd_vshrn(ir, decoded),
        AsimdVshlImm => asimd_two_regs_shift::arm_asimd_vshl_imm(ir, decoded),
        AsimdVsli => asimd_two_regs_shift::arm_asimd_vsli(ir, decoded),
        AsimdVsri => asimd_two_regs_shift::arm_asimd_vsri(ir, decoded),
        AsimdVqshlImm => asimd_two_regs_shift::arm_asimd_vqshl_imm(ir, decoded),
        V8VstMultiple => asimd::arm_v8_vst_multiple(ir, decoded),
        V8VldMultiple => asimd::arm_v8_vld_multiple(ir, decoded),
        V8VstSingle => asimd::arm_v8_vst_single(ir, decoded),
        V8VldSingle => asimd::arm_v8_vld_single(ir, decoded),
        V8VldAllLanes => asimd::arm_v8_vld_all_lanes(ir, decoded),
        AsimdVext => asimd::arm_asimd_vext(ir, decoded),
        AsimdVuzp => asimd::arm_asimd_vuzp(ir, decoded),
        AsimdVzip => asimd::arm_asimd_vzip(ir, decoded),
        AsimdVcgtZero => asimd::arm_asimd_vcgt_zero(ir, decoded),
        AsimdVcgeZero => asimd::arm_asimd_vcge_zero(ir, decoded),
        AsimdVceqZero => asimd::arm_asimd_vceq_zero(ir, decoded),
        AsimdVcleZero => asimd::arm_asimd_vcle_zero(ir, decoded),
        AsimdVcltZero => asimd::arm_asimd_vclt_zero(ir, decoded),
        AsimdVrecpe => asimd::arm_asimd_vrecpe(ir, decoded),
        AsimdVrsqrte => asimd::arm_asimd_vrsqrte(ir, decoded),
        AsimdVnegInt => asimd::arm_asimd_vneg_int(ir, decoded),
        AsimdVabsInt => asimd::arm_asimd_vabs_int(ir, decoded),
        // Hints
        PldImm | PldReg => hint::arm_pld(ir, decoded, options),
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
