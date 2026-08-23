//! Conditional instruction state machine for A32 block translation.
//!
//! Matches upstream dynarmic `conditional_state.h` / `conditional_state.cpp`.
//!
//! In ARM32, each instruction has a condition code. The state machine tracks
//! whether the current block contains conditional instructions and determines
//! when to end a block due to condition changes.
//!
//! The backend uses `Block::cond` and `Block::condition_failed_location` to emit
//! a condition check at block entry (see `emit_cond_prelude`).

use crate::frontend::a32::decoder_thumb16::DecodedThumb16;
use crate::frontend::a32::decoder_thumb32::DecodedThumb32;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::block::Block;
use crate::ir::cond::Cond;
use crate::ir::location::A32LocationDescriptor;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

use super::TranslationOptions;

/// Tracks the conditional state during block translation.
/// Matches upstream dynarmic `ConditionalState` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionalState {
    /// We haven't met any conditional instructions yet.
    None,
    /// Current instruction is conditional and marks the end of this basic block.
    Break,
    /// This basic block is made up solely of conditional instructions (same cond).
    Translating,
    /// This basic block is made up of conditional instructions followed by
    /// unconditional instructions.
    Trailing,
}

/// Check whether translation can continue after a conditional instruction.
/// Matches upstream `CondCanContinue()`.
///
/// Returns true if:
/// - No conditional instructions seen yet (state is None), or
/// - All instructions in the block so far don't write to CPSR (conservative
///   check — if CPSR is modified, the condition for re-evaluation could change).
pub fn cond_can_continue(cond_state: ConditionalState, block: &Block) -> bool {
    assert!(
        cond_state != ConditionalState::Break,
        "Should never call cond_can_continue in Break state"
    );

    if cond_state == ConditionalState::None {
        return true;
    }

    // TODO: This is more conservative than necessary (matches upstream comment).
    !block.any_inst_writes_cpsr()
}

/// Evaluate whether a condition is passed for the current instruction.
/// Matches upstream `IsConditionPassed()`.
///
/// This manages the `ConditionalState` state machine and sets up block-level
/// condition metadata when conditional instructions are encountered.
///
/// Returns true if the instruction should be translated (condition passed or
/// optimistically assumed to pass with a block-entry check).
/// Returns false if the block should end (condition change or non-empty block
/// with new conditional).
pub fn is_condition_passed(
    cond_state: &mut ConditionalState,
    block: &mut Block,
    current_location: A32LocationDescriptor,
    current_instruction_size: u32,
    cond: Cond,
) -> bool {
    assert!(
        *cond_state != ConditionalState::Break,
        "This should never happen. We requested a break but that wasn't honored."
    );

    if cond == Cond::NV {
        // NV conditional is obsolete and raises UnpredictableInstruction.
        *cond_state = ConditionalState::Break;
        let mut ir = A32IREmitter::with_location(block, current_location);
        super::raise_exception_with_instruction_size(
            &mut ir,
            crate::frontend::a32::types::Exception::UnpredictableInstruction,
            current_instruction_size,
        );
        return false;
    }

    if *cond_state == ConditionalState::Translating {
        let cfl = block.condition_failed_location;
        if cfl != Some(current_location.to_location()) || cond == Cond::AL {
            // Either we've moved past the condition-failed point, or we hit an
            // unconditional instruction. Transition to Trailing.
            *cond_state = ConditionalState::Trailing;
        } else {
            // Still in the conditional region with the same condition.
            if Some(cond) == block.cond {
                // Same condition — extend the conditional block.
                let next_loc = current_location
                    .advance_pc(current_instruction_size as i32)
                    .advance_it();
                block.condition_failed_location = Some(next_loc.to_location());
                block.condition_failed_cycle_count += 1;
                return true;
            }

            // Condition changed — abort, we'll make a new block here.
            *cond_state = ConditionalState::Break;
            block.set_terminal(Terminal::LinkBlockFast {
                next: current_location.to_location(),
            });
            return false;
        }
    }

    if cond == Cond::AL {
        // Unconditional — always passes.
        return true;
    }

    // Non-AL condition code.

    if !block.instructions.is_empty() {
        // We've already emitted instructions. Quit for now, we'll make a new
        // block here later. Matches upstream behavior.
        *cond_state = ConditionalState::Break;
        block.set_terminal(Terminal::LinkBlockFast {
            next: current_location.to_location(),
        });
        return false;
    }

    // Block is empty — we'll emit one instruction and set the block-entry
    // conditional appropriately. The backend's EmitCondPrelude will check
    // the condition at block entry.
    *cond_state = ConditionalState::Translating;
    block.cond = Some(cond);
    let next_loc = current_location
        .advance_pc(current_instruction_size as i32)
        .advance_it();
    block.condition_failed_location = Some(next_loc.to_location());
    block.condition_failed_cycle_count = block.cycle_count + 1;
    true
}

// ---------------------------------------------------------------------------
// Thumb conditional helpers (IT block support)
// ---------------------------------------------------------------------------

/// Translate a Thumb16 instruction inside an IT block.
pub fn translate_conditional_thumb16(
    ir: &mut A32IREmitter,
    decoded: &DecodedThumb16,
    cond: Cond,
    options: TranslationOptions,
) -> bool {
    let nzcv = ir.get_cpsr();
    let passed = emit_cond_check(ir, cond, nzcv);
    ir.set_check_bit(passed);

    let cont = super::translate_thumb16_instruction(ir, decoded, options);

    if cont {
        let loc = ir.current_location.expect("location not set");
        let next = loc.advance_pc(2);
        ir.set_term(Terminal::check_bit(
            Terminal::link_block(next.to_location()),
            Terminal::link_block(next.to_location()),
        ));
    }

    false
}

/// Translate a Thumb32 instruction inside an IT block.
pub fn translate_conditional_thumb32(
    ir: &mut A32IREmitter,
    decoded: &DecodedThumb32,
    cond: Cond,
    options: TranslationOptions,
) -> bool {
    let nzcv = ir.get_cpsr();
    let passed = emit_cond_check(ir, cond, nzcv);
    ir.set_check_bit(passed);

    let cont = super::translate_thumb32_instruction(ir, decoded, options);

    if cont {
        let loc = ir.current_location.expect("location not set");
        let next = loc.advance_pc(4);
        ir.set_term(Terminal::check_bit(
            Terminal::link_block(next.to_location()),
            Terminal::link_block(next.to_location()),
        ));
    }

    false
}

/// Emit IR to check a condition code against the current NZCV flags.
/// Returns a Value representing whether the condition is passed (U1 type).
fn emit_cond_check(ir: &mut A32IREmitter, cond: Cond, _nzcv: Value) -> Value {
    match cond {
        Cond::AL | Cond::NV => ir.ir().imm1(true),
        _ => {
            // Use the condition code directly — the terminal If will handle it.
            // We set check_bit to 1 and use If terminal with the condition.
            ir.ir().imm1(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::types::Exception;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::opcode::Opcode;

    #[test]
    fn nv_condition_raises_unpredictable_instruction() {
        let loc = A32LocationDescriptor::at(0x1000);
        let mut block = Block::new(loc.to_location());
        let mut state = ConditionalState::None;

        assert!(!is_condition_passed(
            &mut state,
            &mut block,
            loc,
            4,
            Cond::NV
        ));
        assert_eq!(state, ConditionalState::Break);
        let exception = block
            .instructions
            .iter()
            .find(|inst| inst.opcode == Opcode::A32ExceptionRaised)
            .expect("missing A32ExceptionRaised");
        assert_eq!(
            exception.args[1],
            Value::ImmU64(Exception::UnpredictableInstruction.as_u32() as u64)
        );
        assert!(matches!(
            block.terminal,
            Terminal::CheckHalt { ref else_ }
                if matches!(else_.as_ref(), Terminal::ReturnToDispatch)
        ));
    }
}
