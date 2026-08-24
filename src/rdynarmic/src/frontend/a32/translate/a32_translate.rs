use crate::interface::a32::arch_version::ArchVersion;
use crate::ir::block::Block;
use crate::ir::location::A32LocationDescriptor;

use super::translate_arm::{translate_arm, translate_single_arm_instruction};
use super::translate_callbacks::TranslateCallbacks;
use super::translate_thumb::{translate_single_thumb_instruction, translate_thumb};

/// Options controlling A32 instruction translation.
///
/// Upstream owner: `frontend/A32/translate/a32_translate.h::TranslationOptions`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TranslationOptions {
    pub arch_version: ArchVersion,
    pub define_unpredictable_behaviour: bool,
    pub hook_hint_instructions: bool,
}

impl Default for TranslationOptions {
    fn default() -> Self {
        Self {
            // `TranslationOptions{}` value-initializes the first C++ enum value.
            arch_version: ArchVersion::V3,
            define_unpredictable_behaviour: false,
            hook_hint_instructions: true,
        }
    }
}

/// Translate a block of A32 code starting at `descriptor`.
///
/// Upstream owner: `frontend/A32/translate/a32_translate.cpp::Translate`.
pub fn translate(
    descriptor: A32LocationDescriptor,
    callbacks: &dyn TranslateCallbacks,
    options: TranslationOptions,
) -> Block {
    let mut block = Block::new(descriptor.to_location());
    let mut current = descriptor;

    if descriptor.t_flag() {
        translate_thumb(&mut block, &mut current, callbacks, options);
    } else {
        translate_arm(&mut block, &mut current, descriptor, callbacks, options);
    }

    block
}

/// Translate one supplied A32 instruction into `block`.
///
/// Upstream owner:
/// `frontend/A32/translate/a32_translate.cpp::TranslateSingleInstruction`.
pub fn translate_single_instruction(
    block: &mut Block,
    descriptor: A32LocationDescriptor,
    instruction: u32,
) -> bool {
    if descriptor.t_flag() {
        translate_single_thumb_instruction(block, descriptor, instruction)
    } else {
        translate_single_arm_instruction(block, descriptor, instruction)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::ir::opcode::Opcode;

    #[test]
    fn single_instruction_dispatches_from_t_flag() {
        let arm_location = A32LocationDescriptor::at(0x1000);
        let mut arm_block = Block::new(arm_location.to_location());
        assert!(translate_single_instruction(
            &mut arm_block,
            arm_location,
            0xE1A0_0000
        ));
        assert_eq!(arm_block.cycle_count, 1);
        assert_eq!(
            arm_block.end_location(),
            arm_location.advance_pc(4).to_location()
        );

        let mut cpsr = PSR::default();
        cpsr.set_t(true);
        let thumb_location = A32LocationDescriptor::new(0x2000, cpsr, FPSCR::default(), false);
        let mut thumb_block = Block::new(thumb_location.to_location());
        assert!(translate_single_instruction(
            &mut thumb_block,
            thumb_location,
            0x0000_BF00
        ));
        assert_eq!(thumb_block.cycle_count, 1);
        assert_eq!(
            thumb_block.end_location(),
            thumb_location.advance_pc(2).to_location()
        );
    }

    #[test]
    fn block_translation_threads_hint_options_to_thumb_visitor() {
        let mut cpsr = PSR::default();
        cpsr.set_t(true);
        let location = A32LocationDescriptor::new(0x3000, cpsr, FPSCR::default(), true);
        let read_code = |address| (address == 0x3000).then_some(0x0000_BF30);

        let disabled = translate(
            location,
            &read_code,
            TranslationOptions {
                hook_hint_instructions: false,
                ..TranslationOptions::default()
            },
        );
        assert!(!disabled
            .instructions
            .iter()
            .any(|instruction| instruction.opcode == Opcode::A32ExceptionRaised));

        let enabled = translate(location, &read_code, TranslationOptions::default());
        assert!(enabled
            .instructions
            .iter()
            .any(|instruction| instruction.opcode == Opcode::A32ExceptionRaised));
    }
}
