use crate::frontend::a32::decoder::{decode_arm, ArmInstId, DecodedArm};
use crate::frontend::a32::decoder_thumb16::decode_thumb16;
use crate::frontend::a32::decoder_thumb32::decode_thumb32;
use crate::frontend::a32::types::Exception;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::block::Block;
use crate::ir::location::A32LocationDescriptor;
use crate::ir::terminal::Terminal;

use super::a32_translate::TranslationOptions;
use super::conditional_state::{cond_can_continue, is_condition_passed, ConditionalState};
use super::translate_callbacks::TranslateCallbacks;
use super::{
    raise_exception_with_instruction_size, translate_arm_instruction,
    translate_thumb16_instruction, translate_thumb32_instruction,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ThumbInstSize {
    Thumb16,
    Thumb32,
}

pub(super) fn is_thumb16(first_part: u16) -> bool {
    first_part < 0xE800
}

pub(super) fn read_thumb_instruction(
    arm_pc: u32,
    callbacks: &dyn TranslateCallbacks,
) -> Option<(u32, ThumbInstSize)> {
    let first_part = callbacks.memory_read_code(arm_pc & 0xFFFF_FFFC)?;

    let mut instruction = if (arm_pc & 0x2) != 0 {
        first_part >> 16
    } else {
        first_part & 0xFFFF
    };

    if is_thumb16(instruction as u16) {
        return Some((instruction, ThumbInstSize::Thumb16));
    }

    instruction <<= 16;

    let second_part = callbacks.memory_read_code((arm_pc.wrapping_add(2)) & 0xFFFF_FFFC)?;
    instruction |= if ((arm_pc.wrapping_add(2)) & 0x2) != 0 {
        second_part >> 16
    } else {
        second_part & 0xFFFF
    };

    Some((instruction, ThumbInstSize::Thumb32))
}

fn is_unconditional_instruction(is_thumb16: bool, instruction: u32) -> bool {
    is_thumb16
        && ((instruction & 0xFF00) == 0b1011_1110_0000_0000
            || (instruction & 0xFFC0) == 0b1011_1010_1000_0000)
}

pub(super) fn convert_asimd_instruction(thumb_instruction: u32) -> u32 {
    if (thumb_instruction & 0xEF00_0000) == 0xEF00_0000 {
        let u = (thumb_instruction >> 28) & 1;
        return 0xF200_0000 | (u << 24) | (thumb_instruction & 0x00FF_FFFF);
    }

    if (thumb_instruction & 0xFF00_0000) == 0xF900_0000 {
        return 0xF400_0000 | (thumb_instruction & 0x00FF_FFFF);
    }

    0xF7F0_A000
}

fn maybe_vfp_or_asimd_instruction(thumb_instruction: u32) -> bool {
    (thumb_instruction & 0xEC00_0000) == 0xEC00_0000
        || (thumb_instruction & 0xFF10_0000) == 0xF900_0000
}

fn is_vfp_instruction(id: ArmInstId) -> bool {
    use ArmInstId::*;
    matches!(
        id,
        VPUSH
            | VPOP
            | VLDR_fp
            | VSTR_fp
            | VSTM
            | VLDM
            | VMLA_fp
            | VMLS_fp
            | VNMLS_fp
            | VNMLA_fp
            | VMUL_fp
            | VNMUL_fp
            | VADD_fp
            | VSUB_fp
            | VDIV_fp
            | VFNMS_fp
            | VFNMA_fp
            | VFMA_fp
            | VFMS_fp
            | VSEL_fp
            | VMAXNM_fp
            | VMINNM_fp
            | VMOV_fp_reg
            | VMOV_fp_imm
            | VABS_fp
            | VNEG_fp
            | VSQRT_fp
            | VCMP_fp
            | VCMP_zero_fp
            | VCVT_f_to_f
            | VCVT_from_int
            | VCVT_to_u32
            | VCVT_to_s32
            | VMOV_u32_f64
            | VMOV_f64_u32
            | VMOV_u32_f32
            | VMOV_f32_u32
            | VMOV_2u32_2f32
            | VMOV_2f32_2u32
            | VMOV_2u32_f64
            | VMOV_f64_2u32
            | VMOV_from_i32
            | VMOV_to_i32
            | VMSR
            | VMRS
            | VFP_VDUP
            | VFP_VRINT_rm
            | VFP_VCVT_rm
    )
}

pub(super) fn decode_thumb_vfp_or_asimd(thumb_instruction: u32) -> Option<DecodedArm> {
    if !maybe_vfp_or_asimd_instruction(thumb_instruction) {
        return None;
    }

    let vfp_decoded = decode_arm(thumb_instruction);
    if is_vfp_instruction(vfp_decoded.id) {
        return Some(vfp_decoded);
    }

    let asimd_decoded = decode_arm(convert_asimd_instruction(thumb_instruction));
    (asimd_decoded.id != ArmInstId::Unknown).then_some(asimd_decoded)
}

/// Translate a block of Thumb instructions.
///
/// Upstream owner: `frontend/A32/translate/translate_thumb.cpp::TranslateThumb`.
pub(super) fn translate_thumb(
    block: &mut Block,
    current: &mut A32LocationDescriptor,
    callbacks: &dyn TranslateCallbacks,
    options: TranslationOptions,
) {
    let single_step = current.single_stepping();
    let mut cond_state = ConditionalState::None;
    let mut should_continue = true;

    loop {
        let arm_pc = current.pc();

        {
            let mut ir =
                A32IREmitter::with_location_and_arch(block, *current, options.arch_version);
            if !callbacks.pre_code_read_hook(true, arm_pc, &mut ir) {
                *current = ir.current_location.expect("current_location not set");
                should_continue = false;
                break;
            }
            *current = ir.current_location.expect("current_location not set");
        }

        let (thumb_instruction, inst_size) = match read_thumb_instruction(arm_pc, callbacks) {
            Some(instruction) => instruction,
            None => {
                let mut ir =
                    A32IREmitter::with_location_and_arch(block, *current, options.arch_version);
                should_continue =
                    raise_exception_with_instruction_size(&mut ir, Exception::NoExecuteFault, 2);
                *current = current.advance_pc(2).advance_it();
                block.cycle_count += 1;
                break;
            }
        };

        let is_thumb16 = inst_size == ThumbInstSize::Thumb16;
        let instruction_size = if is_thumb16 { 2 } else { 4 };

        {
            let mut ir =
                A32IREmitter::with_location_and_arch(block, *current, options.arch_version);
            callbacks.pre_code_translation_hook(true, arm_pc, &mut ir);
            *current = ir.current_location.expect("current_location not set");
        }
        let ticks_for_instruction = callbacks.get_ticks_for_code(true, arm_pc, thumb_instruction);

        if is_unconditional_instruction(is_thumb16, thumb_instruction)
            || is_condition_passed(
                &mut cond_state,
                block,
                *current,
                instruction_size,
                current.it().cond(),
            )
        {
            let mut ir =
                A32IREmitter::with_location_and_arch(block, *current, options.arch_version);
            if is_thumb16 {
                let decoded = decode_thumb16(thumb_instruction as u16);
                should_continue = translate_thumb16_instruction(&mut ir, &decoded, options);
            } else if let Some(decoded) = decode_thumb_vfp_or_asimd(thumb_instruction) {
                should_continue = translate_arm_instruction(&mut ir, &decoded, options);
            } else {
                let first_part = (thumb_instruction >> 16) as u16;
                let second_part = thumb_instruction as u16;
                let decoded = decode_thumb32(first_part, second_part);
                should_continue = translate_thumb32_instruction(&mut ir, &decoded, options);
            }
        }

        if cond_state == ConditionalState::Break {
            break;
        }

        *current = current.advance_pc(instruction_size as i32).advance_it();
        block.cycle_count += ticks_for_instruction;

        if !should_continue || !cond_can_continue(cond_state, block) || single_step {
            break;
        }
    }

    if matches!(
        cond_state,
        ConditionalState::Translating | ConditionalState::Trailing
    ) || single_step
    {
        if should_continue {
            let next = current.to_location();
            if single_step {
                block.set_terminal(Terminal::LinkBlock { next });
            } else {
                block.set_terminal(Terminal::LinkBlockFast { next });
            }
        }
    }

    assert!(
        !block.terminal.is_invalid(),
        "Thumb translation completed without a terminal"
    );
    block.set_end_location(current.to_location());
}

/// Translate one supplied Thumb instruction.
///
/// Upstream owner:
/// `frontend/A32/translate/translate_thumb.cpp::TranslateSingleThumbInstruction`.
pub(super) fn translate_single_thumb_instruction(
    block: &mut Block,
    descriptor: A32LocationDescriptor,
    mut thumb_instruction: u32,
) -> bool {
    let is_thumb16 = is_thumb16(thumb_instruction as u16);
    let instruction_size = if is_thumb16 { 2 } else { 4 };
    let options = TranslationOptions::default();
    let mut ir = A32IREmitter::with_location_and_arch(block, descriptor, options.arch_version);

    let should_continue = if is_thumb16 {
        let decoded = decode_thumb16(thumb_instruction as u16);
        translate_thumb16_instruction(&mut ir, &decoded, options)
    } else {
        thumb_instruction = thumb_instruction.rotate_left(16);
        if let Some(decoded) = decode_thumb_vfp_or_asimd(thumb_instruction) {
            translate_arm_instruction(&mut ir, &decoded, options)
        } else {
            let first_part = (thumb_instruction >> 16) as u16;
            let second_part = thumb_instruction as u16;
            let decoded = decode_thumb32(first_part, second_part);
            translate_thumb32_instruction(&mut ir, &decoded, options)
        }
    };

    let end_location = descriptor.advance_pc(instruction_size);
    block.cycle_count += 1;
    block.set_end_location(end_location.to_location());
    should_continue
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::it_state::ITState;
    use crate::frontend::a32::psr::PSR;
    use crate::ir::cond::Cond;

    #[test]
    fn it_block_uses_block_condition_and_advances_location_state() {
        let mut psr = PSR::default();
        psr.set_t(true);
        psr.set_it(ITState::new(0x08).value());
        let location = A32LocationDescriptor::new(0x1000, psr, FPSCR::default(), true);
        let read_code = |addr| (addr == 0x1000).then_some(0x0000_BF00);

        let block = crate::frontend::a32::translate::translate(
            location,
            &read_code,
            TranslationOptions::default(),
        );

        assert_eq!(block.cond, Some(Cond::EQ));
        assert_eq!(
            block.condition_failed_location,
            Some(location.advance_pc(2).advance_it().to_location())
        );
        assert_eq!(
            block.end_location(),
            location.advance_pc(2).advance_it().to_location()
        );
    }
}
