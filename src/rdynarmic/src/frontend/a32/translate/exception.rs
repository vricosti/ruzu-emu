use crate::frontend::a32::decoder::DecodedArm;
use crate::ir::a32_emitter::A32IREmitter;
use crate::ir::terminal::Terminal;
use crate::ir::value::Value;

use super::TranslationOptions;

/// ARM SVC (Supervisor Call).
///
/// Matching dynarmic: advance PC past SVC before halting so the host
/// can read the SVC instruction at PC−4.
pub fn arm_svc(ir: &mut A32IREmitter, inst: &DecodedArm) -> bool {
    let imm24 = inst.imm24();
    let loc = ir.current_location.expect("current_location not set");

    ir.base.push_rsb(loc.advance_pc(4).to_location());

    // Advance PC past SVC (matching dynarmic's BranchWritePC(PC + 4))
    let next_pc = loc.pc().wrapping_add(4);
    ir.branch_write_pc(Value::ImmU32(next_pc));

    ir.call_supervisor(imm24);
    ir.set_term(Terminal::CheckHalt {
        else_: Box::new(Terminal::PopRSBHint),
    });
    false
}

/// ARM UDF (Undefined instruction).
///
/// Upstream routes the `arm_UDF` decode entry to `UndefinedInstruction()`
/// (i.e. `RaiseException(UndefinedInstruction)`), which performs the full
/// lifecycle: `UpdateUpperLocationDescriptor` + `BranchWritePC(PC+4)` +
/// `ExceptionRaised` + `SetTerm`. The previous manual body raised the wrong
/// exception kind (Unpredictable) and skipped the PC bookkeeping.
pub fn arm_udf(ir: &mut A32IREmitter, _inst: &DecodedArm) -> bool {
    super::undefined_instruction(ir)
}

/// ARM BKPT (Breakpoint).
pub fn arm_bkpt(ir: &mut A32IREmitter, inst: &DecodedArm, options: TranslationOptions) -> bool {
    if inst.cond() != crate::ir::cond::Cond::AL && !options.define_unpredictable_behaviour {
        return super::unpredictable_instruction(ir);
    }

    // The block-level conditional state has already guarded execution.
    super::raise_exception(ir, crate::frontend::a32::types::Exception::Breakpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::decoder::ArmInstId;
    use crate::frontend::a32::types::Exception;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
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
    fn conditional_bkpt_respects_define_unpredictable_option() {
        let location = A32LocationDescriptor::at(0x1000);
        let decoded = DecodedArm {
            raw: 0x1120_0070,
            id: ArmInstId::BKPT,
        };

        let mut strict = Block::new(location.to_location());
        assert!(!arm_bkpt(
            &mut A32IREmitter::with_location(&mut strict, location),
            &decoded,
            TranslationOptions::default(),
        ));
        assert_eq!(
            exception_value(&strict),
            Exception::UnpredictableInstruction.as_u32() as u64
        );

        let mut defined = Block::new(location.to_location());
        assert!(!arm_bkpt(
            &mut A32IREmitter::with_location(&mut defined, location),
            &decoded,
            TranslationOptions {
                define_unpredictable_behaviour: true,
                ..TranslationOptions::default()
            },
        ));
        assert_eq!(
            exception_value(&defined),
            Exception::Breakpoint.as_u32() as u64
        );
    }
}
