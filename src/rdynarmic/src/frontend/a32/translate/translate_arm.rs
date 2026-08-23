use crate::frontend::a32::decoder::{decode_arm, ArmInstId};
use crate::frontend::a32::types::Exception;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::block::Block;
use crate::ir::location::A32LocationDescriptor;
use crate::ir::terminal::Terminal;

use super::a32_translate::TranslationOptions;
use super::conditional_state::{cond_can_continue, is_condition_passed, ConditionalState};
use super::translate_callbacks::TranslateCallbacks;
use super::{raise_exception_with_instruction_size, translate_arm_instruction};

/// Translate a block of ARM instructions.
///
/// Upstream owner: `frontend/A32/translate/translate_arm.cpp::TranslateArm`.
pub(super) fn translate_arm(
    block: &mut Block,
    current: &mut A32LocationDescriptor,
    descriptor: A32LocationDescriptor,
    callbacks: &dyn TranslateCallbacks,
    options: TranslationOptions,
) {
    let single_step = descriptor.single_stepping();
    let mut cond_state = ConditionalState::None;
    let mut should_continue = true;

    loop {
        let arm_pc = current.pc();

        {
            let mut ir =
                A32IREmitter::with_location_and_arch(block, *current, options.arch_version);
            if !callbacks.pre_code_read_hook(false, arm_pc, &mut ir) {
                *current = ir.current_location.expect("current_location not set");
                should_continue = false;
                break;
            }
            *current = ir.current_location.expect("current_location not set");
        }

        let arm_instruction = match callbacks.memory_read_code(arm_pc) {
            Some(instruction) => instruction,
            None => {
                let mut ir =
                    A32IREmitter::with_location_and_arch(block, *current, options.arch_version);
                should_continue =
                    raise_exception_with_instruction_size(&mut ir, Exception::NoExecuteFault, 4);
                *current = current.advance_pc(4);
                block.cycle_count += 1;
                break;
            }
        };

        {
            let mut ir =
                A32IREmitter::with_location_and_arch(block, *current, options.arch_version);
            callbacks.pre_code_translation_hook(false, arm_pc, &mut ir);
            *current = ir.current_location.expect("current_location not set");
        }
        let ticks_for_instruction = callbacks.get_ticks_for_code(false, arm_pc, arm_instruction);

        let decoded = decode_arm(arm_instruction);
        let cond = decoded.cond();
        let is_unconditional_space = ((arm_instruction >> 28) & 0xF) == 0xF;
        let unconditional_unpredictable_bkpt = decoded.id == ArmInstId::BKPT
            && cond != crate::ir::cond::Cond::AL
            && !options.define_unpredictable_behaviour;

        if !is_unconditional_space
            && !unconditional_unpredictable_bkpt
            && !is_condition_passed(&mut cond_state, block, *current, 4, cond)
        {
            break;
        }

        let mut ir = A32IREmitter::with_location_and_arch(block, *current, options.arch_version);
        should_continue = translate_arm_instruction(&mut ir, &decoded, options);

        if cond_state == ConditionalState::Break {
            break;
        }

        *current = current.advance_pc(4);
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
        "ARM translation completed without a terminal"
    );
    block.set_end_location(current.to_location());
}

/// Translate one supplied ARM instruction.
///
/// Upstream owner:
/// `frontend/A32/translate/translate_arm.cpp::TranslateSingleArmInstruction`.
pub(super) fn translate_single_arm_instruction(
    block: &mut Block,
    descriptor: A32LocationDescriptor,
    arm_instruction: u32,
) -> bool {
    let decoded = decode_arm(arm_instruction);
    let options = TranslationOptions::default();
    let cond = decoded.cond();
    let is_unconditional_space = ((arm_instruction >> 28) & 0xF) == 0xF;
    let unconditional_unpredictable_bkpt = decoded.id == ArmInstId::BKPT
        && cond != crate::ir::cond::Cond::AL
        && !options.define_unpredictable_behaviour;
    let mut cond_state = ConditionalState::None;

    let should_translate = is_unconditional_space
        || unconditional_unpredictable_bkpt
        || is_condition_passed(&mut cond_state, block, descriptor, 4, cond);
    let should_continue = if should_translate {
        let mut ir = A32IREmitter::with_location_and_arch(block, descriptor, options.arch_version);
        translate_arm_instruction(&mut ir, &decoded, options)
    } else {
        // Upstream instruction visitors return true when ArmConditionPassed
        // rejects the instruction; the condition state/terminal carries why.
        true
    };

    let end_location = descriptor.advance_pc(4);
    block.cycle_count += 1;
    block.set_end_location(end_location.to_location());
    should_continue
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::translate::translate;
    use crate::frontend::a32::types::Exception;
    use crate::ir::cond::Cond;
    use crate::ir::opcode::Opcode;
    use crate::ir::value::Value;

    fn exception_value(block: &Block) -> u64 {
        let instruction = block
            .instructions
            .iter()
            .find(|instruction| instruction.opcode == Opcode::A32ExceptionRaised)
            .expect("missing A32ExceptionRaised");
        let Value::ImmU64(value) = instruction.args[1] else {
            panic!("exception must be an immediate");
        };
        value
    }

    #[test]
    fn conditional_bkpt_is_unconditionally_unpredictable_unless_defined() {
        let location = A32LocationDescriptor::at(0x1000);
        let read_code = |address| (address == 0x1000).then_some(0x1120_0070);

        let strict = translate(location, &read_code, TranslationOptions::default());
        assert_eq!(strict.cond, None);
        assert_eq!(
            exception_value(&strict),
            Exception::UnpredictableInstruction.as_u32() as u64
        );

        let defined = translate(
            location,
            &read_code,
            TranslationOptions {
                define_unpredictable_behaviour: true,
                ..TranslationOptions::default()
            },
        );
        assert_eq!(defined.cond, Some(Cond::NE));
        assert_eq!(
            exception_value(&defined),
            Exception::Breakpoint.as_u32() as u64
        );
    }

    #[test]
    fn single_arm_instruction_preserves_condition_metadata() {
        let location = A32LocationDescriptor::at(0x2000);
        let mut block = Block::new(location.to_location());

        assert!(translate_single_arm_instruction(
            &mut block,
            location,
            0x11A0_0000,
        ));
        assert_eq!(block.cond, Some(Cond::NE));
        assert_eq!(
            block.condition_failed_location,
            Some(location.advance_pc(4).to_location())
        );
    }
}
